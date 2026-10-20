#![allow(dead_code)]

use core::cell::Cell;
use handler_derive::{handler, handler_table};

use bytecode::Opcode;
use vm_core::ic::{CallHit, CallProbe, CallRecord, InlineCache};
use vm_core::proxy::Proxy;
use vm_core::{
    Args, CallTarget, Callee, Coercion, Convert, DenseString, EntryFn, FeedbackVector, FrameMeta,
    Handle, HandleSlice, Heap, Hint, HostCtx, Interpreter, NativeIndex, Object, Register, Smi,
    Tagged, ThreadState, VM, Value, VmError, Word, interpreter::SAFEPOINT_INTERVAL, raise_runtime,
};

#[cfg(feature = "star_fusion")]
use crate::star_lookahead;
use crate::{FloatReg, Ops, Regs, RootsArg};

/// Placeholder receiver type for `#[handler]` fns.
/// The proc macro rewrites access to it.
#[doc(hidden)]
pub struct Frame;

/// Instruction Pointer
macro_rules! ip {
    ($ip:expr) => {
        $ip
    };
}

/// Insruction Offset
macro_rules! pc {
    ($ip:expr, $x:expr) => {{ $ip as usize - $x.code_ptr() as usize }};
}

/// Register Window of the Frame
macro_rules! regs {
    ($r:expr) => {
        $r
    };
}

/// Accumulator, must be used before reaching safepoints!
macro_rules! acc {
    ($acc:expr, $x:expr) => {{ $acc.get($x.heap()) }};
}

/// Float Register
macro_rules! float {
    ($f:expr) => {{ $f.get() }};
}

/// Set the accumulator from a tagged value.
macro_rules! set_acc {
    ($acc:ident, $v:expr) => {
        $acc = Acc::set($v)
    };
}

/// Set the accumulator from raw smi bits.
macro_rules! set_acc_smi {
    ($acc:ident, $v:expr) => {
        $acc = Acc::from_smi_bits($v)
    };
}

/// Park a float payload in the float register for a slow path.
macro_rules! set_float {
    ($f:ident, $v:expr) => {
        $f = FloatReg::new($v)
    };
}

pub struct Ctx<'s, 'h> {
    vm: &'s VM,
    state: &'s ThreadState,
    heap: &'h mut Heap,
    /// Anchor of the frame `execute` entered
    base_anchor: usize,
    /// Data pointer of the current frame's bytecode — the pc origin
    /// (`ip - code`). Refreshed at frame switches and after points
    /// that may collect (reenter, the safepoint poll, the unwinder);
    /// interior mutability, like `safepoints`.
    code: Cell<*const u8>,
    safepoints: Cell<u32>,
}

impl<'s, 'h> Ctx<'s, 'h> {
    #[inline(always)]
    pub fn new(vm: &'s VM, state: &'s ThreadState, heap: &'h mut Heap, base_anchor: usize) -> Self {
        let code = state.stack().frame_header(base_anchor).code(heap).as_ptr();
        Ctx {
            vm,
            state,
            heap,
            base_anchor,
            code: Cell::new(code),
            safepoints: Cell::new(SAFEPOINT_INTERVAL),
        }
    }

    #[inline(always)]
    pub fn heap(&self) -> &Heap {
        self.heap
    }

    #[inline(always)]
    pub fn heap_mut(&mut self) -> &mut Heap {
        self.heap
    }

    #[inline(always)]
    pub fn vm(&self) -> &'s VM {
        self.vm
    }

    #[inline(always)]
    pub fn state(&self) -> &'s ThreadState {
        self.state
    }

    #[inline(always)]
    pub fn stack(&self) -> &'s vm_core::Stack {
        self.state.stack()
    }

    #[inline(always)]
    pub fn frame_base(&self) -> usize {
        self.state.frame_base()
    }

    #[inline(always)]
    pub fn base_anchor(&self) -> usize {
        self.base_anchor
    }

    /// Switch the current frame (after a push, pop, or unwind).
    #[inline(always)]
    pub fn set_frame_base(&self, base: usize) {
        self.state.set_frame_base(base);
    }

    /// The current frame's pc origin (one load; the header chain runs
    /// only at the refresh edges).
    #[inline(always)]
    pub fn code_ptr(&self) -> *const u8 {
        self.code.get()
    }

    /// Reload the pc origin from the current frame's header — after a
    /// point that may have collected and moved the bytecode.
    #[inline(always)]
    pub fn reload_code(&self) {
        let code = {
            let state = self.state;
            let heap = &*self.heap;
            state
                .stack()
                .frame_header(state.frame_base())
                .code(heap)
                .as_ptr()
        };
        self.code.set(code);
    }

    /// Switch to the frame at `base` (pop/unwind) and reload the pc
    /// origin from its header.
    #[inline(always)]
    pub fn switch_frame(&self, base: usize) {
        let code = {
            let state = self.state;
            let heap = &*self.heap;
            state.stack().frame_header(base).code(heap).as_ptr()
        };
        self.state.set_frame_base(base);
        self.code.set(code);
    }

    /// Switch to a freshly pushed frame; `code` is its bytecode data
    /// pointer, derived from the callee's info before the push (which
    /// does not allocate, so it cannot go stale).
    #[inline(always)]
    pub fn switch_frame_code(&self, base: usize, code: *const u8) {
        self.state.set_frame_base(base);
        self.code.set(code);
    }

    /// The current frame's feedback vector, or `None` for functions
    /// without feedback slots.
    #[inline(always)]
    pub fn feedback_ref<'f>(&self, heap: &'f Heap) -> Option<Tagged<'f, FeedbackVector>> {
        self.stack().frame_header(self.frame_base()).feedback(heap)
    }

    #[inline(always)]
    pub fn regs_ptr(&self) -> *mut Register {
        unsafe { self.stack().slots_ptr().add(self.frame_base()) }
    }

    /// One loop back-edge: `true` every `SAFEPOINT_INTERVAL` ticks.
    #[inline(always)]
    /// The `Cell` is deliberate: with plain `&mut` field access LLVM
    /// canonicalizes the decrement into a branchless `csel`, adding
    /// three instructions to every backedge; the interior-mutability
    /// boundary keeps the branchy form (decrement in place, reload
    /// only once per interval).
    pub fn safepoint_tick(&self) -> bool {
        let n = self.safepoints.get();
        if n == 0 {
            self.safepoints.set(SAFEPOINT_INTERVAL);
            true
        } else {
            self.safepoints.set(n - 1);
            false
        }
    }

    #[inline(always)]
    pub fn caller_meta(&self, pc: usize, handler_pc: usize) -> FrameMeta {
        let base = self.frame_base();
        FrameMeta {
            base,
            pc,
            register_count: self.stack().regcount(base),
            handler_pc,
        }
    }

    #[inline(always)]
    pub fn call_runtime(&mut self, rt: NativeIndex, args: Args) -> Tagged<'_, Value> {
        let (vm, state) = (self.vm, self.state);
        let f = vm.runtime(rt);
        let stack = state.stack();
        let saved = stack.top();
        let bump = stack.rooting_top(args);
        if let Some(top) = bump {
            stack.set_top(top);
        }
        let nctx = HostCtx::new(vm, self.heap_mut(), state);
        let v = f(nctx, None, args);
        if bump.is_some() {
            stack.set_top(saved);
        }
        v
    }

    #[inline(always)]
    pub fn raise_tag(&mut self, err: VmError) -> Tagged<'_, Value> {
        let (vm, state) = (self.vm, self.state);
        raise_runtime(vm, self.heap_mut(), state, err)
    }
}

#[derive(Clone, Copy)]
pub struct Acc(Value);

impl Acc {
    #[inline(always)]
    pub fn set(v: Tagged<'_, Value>) -> Self {
        Acc(v.raw())
    }

    #[inline(always)]
    pub fn from_smi_bits(bits: i64) -> Self {
        Acc(Value::from_bits(bits as Word))
    }

    #[inline(always)]
    pub fn get<'h>(self, _heap: &'h Heap) -> Tagged<'h, Value> {
        // Safety: the dispatch-chain freshness invariant.
        unsafe { Tagged::from_value_unchecked(self.0) }
    }
}

pub struct TableArg<'h>(pub &'static [Handler; 256], core::marker::PhantomData<&'h ()>);

impl<'h> TableArg<'h> {
    #[inline(always)]
    pub fn new(table: &'static [Handler; 256]) -> TableArg<'h> {
        TableArg(table, core::marker::PhantomData)
    }

    #[inline(always)]
    pub fn get(&self, op: u8) -> Handler {
        self.0[op as usize]
    }
}

pub type Handler = for<'h> extern "rust-preserve-none" fn(
    *const u8,
    Regs,
    Acc,
    &mut Ctx<'_, 'h>,
    TableArg<'h>,
    RootsArg<'h>,
    FloatReg,
) -> Acc;

/// Dispatch to the handler for the instruction at `$p`.
macro_rules! dispatch {
    ($p:expr, $a:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let h = $t.get(unsafe { *$p });
        become h($p, $r, $a, $x, $t, $k, $f)
    }};
}

/// Advance to next instruction and dispatch.
macro_rules! next {
    ($op:ident, $ip:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident, $a:expr) => {{
        let p = $ip.wrapping_add(
            (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() },
        );
        #[cfg(feature = "star_fusion")]
        {
            if star_lookahead(Opcode::$op) && unsafe { *p } == Opcode::Store as u8 {
                let dst = unsafe { *p.add(1) } as i8 as i32;
                $r.set(dst, $a.get($x.heap()));
                dispatch!(unsafe { p.add(2) }, $a, $r, $x, $t, $k, $f)
            } else {
                dispatch!(p, $a, $r, $x, $t, $k, $f)
            }
        }
        #[cfg(not(feature = "star_fusion"))]
        {
            dispatch!(p, $a, $r, $x, $t, $k, $f)
        }
    }};
}

/// next! but rederive the instruction pointer, must be done when having reached safepoints.
macro_rules! reenter {
    ($op:ident, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let pc = $pc;
        $ctx.reload_code();
        let fault_ip = unsafe { $ctx.code_ptr().add(pc) };
        if $acc.get($ctx.heap()).raw().to_bits()
            == $roots
                .known($ctx.heap())
                .exception
                .as_tagged($ctx.heap())
                .erase()
                .raw()
                .to_bits()
        {
            become throw_dispatch::<STRIDE>(fault_ip, $regs, $acc, $ctx, $table, $roots, $float)
        }
        let p = fault_ip.wrapping_add(
            (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() },
        );
        let regs = unsafe { Regs::new($ctx.regs_ptr()) };
        let h = $table.get(unsafe { *p });
        become h(p, regs, $acc, $ctx, $table, $roots, $float)
    }};
}

/// Tailcall into a named handler
macro_rules! tail {
    ($ip:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr, $func:path) => {
        become $func($ip, $regs, $acc, $ctx, $table, $roots, $float)
    };
}

/// Dispatch into the caller after a frame pop: the register window and
/// code pointer belong to the restored frame now.
macro_rules! dispatch_frame {
    ($pc:expr, $a:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let regs = unsafe { Regs::new($x.regs_ptr()) };
        let p = unsafe { $x.code_ptr().add($pc) };
        let h = $t.get(unsafe { *p });
        become h(p, regs, $a, $x, $t, $k, $f)
    }};
}

enum StagedCall<'a> {
    /// the call completed: the value or the exception sentinel
    Value(Tagged<'a, Value>),
    /// a frame was pushed and made current: enter it at pc 0
    Frame,
    /// a rooted argument window for a runtime callee
    Native { rt: NativeIndex, staged: Args },
    /// a proxy apply over a staged window
    Proxy { callee_reg: i32, staged: Args },
}

/// The call continuation: consume a `*_start` result and enter it.
/// `Frame` dispatches into the pushed callee; everything else resumes
/// the caller (`reenter`'s body: reload, sentinel check, advance).
///
/// A macro (not a fn) because its arms `become` — the expansion must
/// sit inside the handler body.
macro_rules! call_enter {
    ($op:ident, $result:expr, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let (acc, value) = match $result {
            Ok(StagedCall::Frame) => ($acc, false),
            Ok(StagedCall::Value(v)) => (Acc::set(v), true),
            Ok(StagedCall::Native { rt, staged }) => {
                (Acc::set($ctx.call_runtime(rt, staged)), true)
            }
            Ok(StagedCall::Proxy { callee_reg, staged }) => {
                (Acc::set(proxy_apply_staged($ctx, $roots, $regs, callee_reg, staged)), true)
            }
            Err(err) => (Acc::set($ctx.raise_tag(err)), true),
        };
        if value {
            $ctx.reload_code();
            let fault_ip = unsafe { $ctx.code_ptr().add($pc) };
            if acc.get($ctx.heap()).raw().to_bits()
                == $roots
                    .known($ctx.heap())
                    .exception
                    .as_tagged($ctx.heap())
                    .erase()
                    .raw()
                    .to_bits()
            {
                become throw_dispatch::<STRIDE>(fault_ip, $regs, acc, $ctx, $table, $roots, $float)
            }
            let size =
                (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() };
            let p = unsafe { fault_ip.add(size) };
            let regs = unsafe { Regs::new($ctx.regs_ptr()) };
            let h = $table.get(unsafe { *p });
            become h(p, regs, acc, $ctx, $table, $roots, $float)
        }
        let regs = unsafe { Regs::new($ctx.regs_ptr()) };
        let acc = Acc::set(
            $roots
                .known($ctx.heap())
                .undefined
                .as_tagged($ctx.heap())
                .erase(),
        );
        let entry = $ctx.code_ptr();
        let h = $table.get(unsafe { *entry });
        become h(entry, regs, acc, $ctx, $table, $roots, $float)
    }};
}

#[handler]
fn op_trap(frame: Frame) -> Acc {
    let byte = unsafe { *frame.ip() };
    unreachable!("unimplemented opcode {:?}", Opcode::from_byte(byte))
}

#[rustc_align(32)]
extern "rust-preserve-none" fn throw_dispatch<'h, const STRIDE: usize>(
    ip: *const u8,
    _regs: Regs,
    _acc: Acc,
    ctx: &mut Ctx<'_, 'h>,
    table: TableArg<'h>,
    roots: RootsArg<'h>,
    float: FloatReg,
) -> Acc {
    ctx.reload_code();
    let mut pc = ip as usize - ctx.code_ptr() as usize;
    loop {
        let base = ctx.frame_base();
        let handled = if ctx.state().termination().is_some() {
            None
        } else {
            let callable = ctx
                .stack()
                .frame_header(base)
                .callable_slot()
                .get(ctx.heap());
            callable
                .as_heap_object()
                .and_then(|obj| obj.callable_info(ctx.heap()))
                .and_then(|info| info.handlers.get(ctx.heap()))
                .and_then(|handlers| handlers.lookup(pc))
        };
        if let Some(handler_pc) = handled {
            let ex = ctx
                .state()
                .take_pending_exception_tagged(ctx.heap())
                .expect("pending exception must be set while unwinding");
            let acc = Acc::set(ex);
            dispatch_frame!(handler_pc, acc, ctx, table, roots, float)
        }
        if base == ctx.base_anchor() {
            return Acc::set(
                roots
                    .known(ctx.heap())
                    .exception
                    .as_tagged(ctx.heap())
                    .erase(),
            );
        }
        let caller = ctx.stack().pop_frame(base);
        ctx.switch_frame(caller.base);
        pc = caller.handler_pc;
    }
}

#[handler(Load)]
fn op_load(frame: Frame) {
    let r = frame.signed::<0>();
    frame.set_acc(frame.regs().get(r, frame.ctx().heap()));
    next!()
}

#[handler(Store)]
fn op_store(frame: Frame) {
    frame.regs().set(frame.signed::<0>(), frame.acc());
    next!()
}

#[handler(Add)]
fn op_add(frame: Frame) {
    let h = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), h);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(sum) = a.checked_add(b)
    {
        frame.set_acc_smi(sum);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(h), frame.acc().as_number(h)) {
        let sum = a + b;
        if let Some(s) = Smi::from_f64(sum) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(sum) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(sum);
        tail!(slow_box_number)
    }
    tail!(slow_add)
}

#[handler(Add)]
#[cold]
fn slow_box_number(frame: Frame) -> Acc {
    let pc = frame.pc();
    frame.set_acc(frame.ctx().heap_mut().new_number(frame.float()));
    reenter!(pc)
}

#[handler(Add)]
#[cold]
fn slow_add(frame: Frame) -> Acc {
    let pc = frame.pc();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(frame.acc());
        let vm = frame.ctx().vm();
        let heap = frame.ctx().heap_mut();
        let lhs = match Object::to_primitive(vm, heap, state, lhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let rhs = match Object::to_primitive(vm, heap, state, rhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        if lhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
            || rhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
        {
            let a = match Convert::to_string(heap, &scope, lhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            let b = match Convert::to_string(heap, &scope, rhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            DenseString::concat(heap, &scope, a, b)
                .as_tagged(heap)
                .erase()
        } else {
            let a = match lhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            let b = match rhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            let sum = a + b;
            match Smi::from_f64(sum) {
                Some(s) => s.into_tagged(),
                None => heap.new_number(sum),
            }
        }
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallRuntime)]
fn op_call_runtime(frame: Frame) -> Acc {
    let pc = frame.pc();
    let rt = frame.unsigned::<0>();
    let base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let native = vm.runtime(NativeIndex(rt));
    let args = state.stack().window(frame.ctx().frame_base(), base, count);
    let nctx = HostCtx::new(vm, frame.ctx().heap_mut(), state);
    let v = native(nctx, None, args);
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(Jump)]
fn op_jump(frame: Frame) -> Acc {
    dispatch!(frame.ip().wrapping_offset(frame.signed::<0>() as isize))
}

#[handler(JumpIfTruthy)]
fn op_jump_if_truthy(frame: Frame) -> Acc {
    let off = frame.signed::<0>();
    if frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfFalsy)]
fn op_jump_if_falsy(frame: Frame) -> Acc {
    let off = frame.signed::<0>();
    if !frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfNotUndefined)]
fn op_jump_if_not_undefined(frame: Frame) -> Acc {
    let off = frame.signed::<0>();
    let undefined = roots
        .known(frame.ctx().heap())
        .undefined
        .as_tagged(frame.ctx().heap())
        .erase();
    if frame.acc() != undefined {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpLoop)]
fn op_jump_loop(frame: Frame) -> Acc {
    let target = frame.ip().wrapping_offset(frame.signed::<0>() as isize);
    if frame.ctx().safepoint_tick() {
        let pc = target as usize - frame.ctx().code_ptr() as usize;
        let fault = frame.ip() as usize - frame.ctx().code_ptr() as usize;
        let state = frame.ctx().state();
        let acc_slot = state.acc_slot();
        acc_slot.store(frame.acc());
        let shutdown = frame.ctx().heap_mut().safepoint_poll();
        frame.ctx().reload_code();
        if shutdown {
            state.set_termination(vm_core::Termination::Shutdown);
            state.set_pending_exception(
                roots
                    .known(frame.ctx().heap())
                    .undefined
                    .as_tagged(frame.ctx().heap()),
            );
            let fault_ip = unsafe { frame.ctx().code_ptr().add(fault) };
            become throw_dispatch::<STRIDE>(
                fault_ip,
                frame.regs(),
                acc,
                frame.ctx(),
                table,
                roots,
                float,
            )
        }
        let acc = Acc::set(acc_slot.get(frame.ctx().heap()));
        let p = unsafe { frame.ctx().code_ptr().add(pc) };
        let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
        dispatch!(p)
    }
    dispatch!(target)
}

#[handler(Return)]
fn op_return(frame: Frame) -> Acc {
    frame.opcode_check();
    if frame.ctx().frame_base() == frame.ctx().base_anchor() {
        return acc;
    }
    let caller = frame.ctx().stack().pop_frame(frame.ctx().frame_base());
    frame.ctx().switch_frame(caller.base);
    dispatch_frame!(caller.pc)
}

#[handler(CallNoFeedback)]
fn op_call_no_feedback(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let result = call_no_feedback_start::<STRIDE>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.signed::<0>(),
        callee,
        frame.signed::<1>(),
        frame.unsigned::<2>(),
    );
    call_enter!(result, pc)
}

#[handler(Call)]
fn op_call(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let result = call_window_start::<STRIDE>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<3>(),
        frame.signed::<0>(),
        callee,
        frame.signed::<1>(),
        frame.unsigned::<2>(),
    );
    call_enter!(result, pc)
}

#[handler(CallMethod0)]
fn op_call_method0(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let recv = frame.regs().get(frame.signed::<1>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, true>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<2>(),
        frame.signed::<0>(),
        callee,
        recv,
        &[],
    );
    call_enter!(result, pc)
}

#[handler(CallMethod1)]
fn op_call_method1(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let recv = frame.regs().get(frame.signed::<1>(), frame.ctx().heap());
    let arg0 = frame.regs().get(frame.signed::<2>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, true>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<3>(),
        frame.signed::<0>(),
        callee,
        recv,
        &[arg0][..1],
    );
    call_enter!(result, pc)
}

#[handler(CallMethod2)]
fn op_call_method2(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let recv = frame.regs().get(frame.signed::<1>(), frame.ctx().heap());
    let arg0 = frame.regs().get(frame.signed::<2>(), frame.ctx().heap());
    let arg1 = frame.regs().get(frame.signed::<3>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, true>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<4>(),
        frame.signed::<0>(),
        callee,
        recv,
        &[arg0, arg1][..2],
    );
    call_enter!(result, pc)
}

#[handler(CallFunction0)]
fn op_call_function0(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, false>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<1>(),
        frame.signed::<0>(),
        callee,
        roots
            .known(frame.ctx().heap())
            .undefined
            .as_tagged(frame.ctx().heap())
            .erase(),
        &[],
    );
    call_enter!(result, pc)
}

#[handler(CallFunction1)]
fn op_call_function1(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let arg0 = frame.regs().get(frame.signed::<1>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, false>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<2>(),
        frame.signed::<0>(),
        callee,
        roots
            .known(frame.ctx().heap())
            .undefined
            .as_tagged(frame.ctx().heap())
            .erase(),
        &[arg0][..1],
    );
    call_enter!(result, pc)
}

#[handler(CallFunction2)]
fn op_call_function2(frame: Frame) -> Acc {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let arg0 = frame.regs().get(frame.signed::<1>(), frame.ctx().heap());
    let arg1 = frame.regs().get(frame.signed::<2>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, false>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<3>(),
        frame.signed::<0>(),
        callee,
        roots
            .known(frame.ctx().heap())
            .undefined
            .as_tagged(frame.ctx().heap())
            .erase(),
        &[arg0, arg1][..2],
    );
    call_enter!(result, pc)
}

handler_table!(
    op_load,
    op_store,
    op_add,
    op_call_runtime,
    op_jump,
    op_jump_if_truthy,
    op_jump_if_falsy,
    op_jump_if_not_undefined,
    op_jump_loop,
    op_return,
    op_call_no_feedback,
    op_call,
    op_call_method0,
    op_call_method1,
    op_call_method2,
    op_call_function0,
    op_call_function1,
    op_call_function2,
);

/// Push a callee frame (facts from a `CallTarget::Bytecode` destructure
/// or a call-IC hit; the caller is the current frame) and switch to it.
/// Entry is at pc 0 with the accumulator seeded undefined.
#[inline(always)]
fn push_callee_frame<'a>(
    x: &Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    args: Args,
) -> Result<StagedCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = roots.known(heap).undefined.as_tagged(heap).erase();
    // the pushed frame's pc origin, straight from the callee's info:
    // `push_frame` does not allocate, so it cannot go stale
    let code = callee.info.as_ref().bytecode.get(heap).as_ptr();
    let frame = x.stack().push_frame(heap, meta, callee, undefined, args)?;
    x.switch_frame_code(frame.base, code);
    Ok(StagedCall::Frame)
}

/// Push with parameters as values: stored straight into the callee's
/// parameter slots, no intermediate staging window.
#[inline(always)]
fn push_callee_frame_values<'a>(
    x: &Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    values: &[Tagged<'_, Value>],
) -> Result<StagedCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = roots.known(heap).undefined.as_tagged(heap).erase();
    let code = callee.info.as_ref().bytecode.get(heap).as_ptr();
    let frame = x
        .stack()
        .push_frame_values(heap, meta, callee, undefined, values)?;
    x.switch_frame_code(frame.base, code);
    Ok(StagedCall::Frame)
}

/// Stage a value window into rooted slots at the stack top, for
/// callees that may collect (`Native`/`Proxy`).
#[inline(always)]
fn stage_values(x: &Ctx<'_, '_>, values: &[Tagged<'_, Value>]) -> Result<Args, VmError> {
    x.stack().stage_slice(values)
}

/// The IC-miss resolution (cold): walk the callee's target and record
/// the hit; the result feeds `resolve_target` like a probe hit.
#[cold]
#[inline(never)]
fn call_target_miss<'a>(
    heap: &'a Heap,
    feedback: Option<Tagged<'a, FeedbackVector>>,
    fb: Option<usize>,
    callee: Tagged<'a, Value>,
) -> Option<CallTarget<'a>> {
    let target = callee.call_target(heap);
    match &target {
        Some(CallTarget::Native(rt)) => {
            if let Some(fb) = fb {
                InlineCache::call_update(heap, feedback, fb, callee, CallRecord::Native(*rt));
            }
        }
        Some(CallTarget::Bytecode { info, .. }) => {
            if let Some(fb) = fb {
                InlineCache::call_update(heap, feedback, fb, callee, CallRecord::Bytecode(*info));
            }
        }
        None | Some(CallTarget::Proxy(_)) => {}
    }
    target
}

/// Resolve a walked target over an already-staged argument window —
/// the cold path (IC miss, `CallNoFeedback`): class constructors and
/// non-callables raise (as `Err`), bytecode pushes through the shared
/// helper, natives and proxies carry their staged window into the
/// exclusive phase. The hot probe arms bypass this and act on the
/// `CallHit` fields directly.
#[inline(always)]
fn resolve_target<'a>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee_reg: i32,
    target: Option<CallTarget<'a>>,
    staged: Args,
) -> Result<StagedCall<'a>, VmError> {
    match target {
        None => Err(VmError::Type),
        Some(CallTarget::Proxy(_)) => Ok(StagedCall::Proxy { callee_reg, staged }),
        Some(CallTarget::Native(rt)) => Ok(StagedCall::Native { rt, staged }),
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                staged,
            )
        }
    }
}

/// The scattered call start (`CallMethodN` / `CallFunctionN`):
/// eagerly-read operand values, shared context. `has_recv` selects the
/// method form (and its extra operand); everything up to the rooted
/// or consumed words is allocation-free.
#[inline(always)]
fn call_scatter_start<'a, const STRIDE: usize, const RECV: bool>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    fb: usize,
    callee_reg: i32,
    callee: Tagged<'a, Value>,
    recv: Tagged<'a, Value>,
    args: &[Tagged<'a, Value>],
) -> Result<StagedCall<'a>, VmError> {
    // callee, [recv,] args.., feedback — STRIDE/RECV are compile-time;
    // args.len() folds per call site (always a literal there)
    let size = (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * (2 + RECV as usize + args.len());
    // parameter payload: [receiver, args..]
    let mut window = [recv; 3];
    window[1..1 + args.len()].copy_from_slice(args);
    let values = &window[..1 + args.len()];
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame_values(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                values,
            )
        }
        CallProbe::Native(rt) => Ok(StagedCall::Native {
            rt,
            staged: stage_values(x, values)?,
        }),
        CallProbe::Miss => resolve_target(
            x,
            roots,
            pc,
            size,
            callee_reg,
            call_target_miss(x.heap(), x.feedback_ref(x.heap()), Some(fb), callee),
            stage_values(x, values)?,
        ),
    }
}

/// The contiguous-window call start (`Call`): the window lives in the
/// caller's frame (already rooted), so no staging is needed.
#[inline(always)]
fn call_window_start<'a, const STRIDE: usize>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    fb: usize,
    callee_reg: i32,
    callee: Tagged<'a, Value>,
    base: i32,
    count: usize,
) -> Result<StagedCall<'a>, VmError> {
    let size = (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::Call.operands().len() };
    let staged = x.stack().window(x.frame_base(), base, count);
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                staged,
            )
        }
        CallProbe::Native(rt) => Ok(StagedCall::Native { rt, staged }),
        CallProbe::Miss => resolve_target(
            x,
            roots,
            pc,
            size,
            callee_reg,
            call_target_miss(x.heap(), x.feedback_ref(x.heap()), Some(fb), callee),
            staged,
        ),
    }
}

/// `CallNoFeedback`: the uncached `call_target` walk, resolved over
/// the in-frame window.
#[inline(always)]
fn call_no_feedback_start<'a, const STRIDE: usize>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    callee_reg: i32,
    callee: Tagged<'a, Value>,
    base: i32,
    count: usize,
) -> Result<StagedCall<'a>, VmError> {
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallNoFeedback.operands().len() };
    let staged = x.stack().window(x.frame_base(), base, count);
    resolve_target(x, roots, pc, size, callee_reg, callee.call_target(x.heap()), staged)
}

/// Apply a callable proxy over a staged window: root the window for
/// the call, re-read the callee register, restore the stack top.
#[cold]
#[inline(never)]
fn proxy_apply_staged<'a>(
    x: &'a mut Ctx<'_, '_>,
    roots: RootsArg<'_>,
    regs: Regs,
    callee_reg: i32,
    staged: Args,
) -> Tagged<'a, Value> {
    let vm = x.vm();
    let state = x.state();
    let stack = state.stack();
    let saved_top = stack.top();
    if let Some(top) = stack.rooting_top(staged) {
        stack.set_top(top);
    }
    let v = state.handle_scope(|scope| {
        let callee = scope.handle(regs.get(callee_reg, x.heap()));
        match Proxy::apply(vm, x.heap_mut(), state, callee, staged) {
            Ok(Coercion::Threw) => roots.known(x.heap()).exception.as_tagged(x.heap()).erase(),
            Ok(Coercion::Value(v)) => v,
            Err(err) => raise_runtime(vm, x.heap_mut(), state, err),
        }
    });
    stack.set_top(saved_top);
    v
}

fn enter<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'a, Value>>,
) -> Result<Tagged<'a, Value>, VmError> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => Err(VmError::Type),
        // the proxy dispatch above already intercepted these
        Some(CallTarget::Proxy(_)) => Err(VmError::Type),
        Some(CallTarget::Native(rt)) => {
            let f = vm.runtime(rt);
            let nctx = HostCtx::new(vm, heap, state);
            Ok(f(nctx, new_target, args.as_args()))
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if new_target.is_none() && kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            let stack = state.stack();
            let roots = RootsArg::from_heap(heap);
            let new_target_value = match &new_target {
                Some(nt) => nt.as_tagged(heap).erase(),
                None => roots.known(heap).undefined.as_tagged(heap).erase(),
            };
            let staged = stack.stage_slice(args.as_tagged())?;
            let frame = stack.push_frame(
                heap,
                FrameMeta::ROOT,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                new_target_value,
                staged,
            )?;
            state.set_frame_base(frame.base);
            state.set_frame_active(true);

            let mut ctx = Ctx::new(vm, state, heap, frame.base);
            let ip = ctx.code_ptr();
            let regs = unsafe { Regs::new(ctx.regs_ptr()) };
            let table = TableArg::new(&NEW_TABLE_NARROW);
            let roots = RootsArg::from_heap(ctx.heap());
            // the fresh frame is entered at pc 0 with the accumulator
            // seeded undefined (the frame init wrote the slot)
            let acc_slot = state.acc_slot();
            let acc = Acc::set(acc_slot.get(ctx.heap()));
            let acc = table.get(unsafe { *ip })(
                ip,
                regs,
                acc,
                &mut ctx,
                table,
                roots,
                FloatReg::new(f64::NAN),
            );
            let Ctx { heap, .. } = ctx;
            Ok(acc.get(heap))
        }
    }
}

/// `Interpreter` entry: `VM::new::<GC, NewInterpreter>(..)`.
pub fn execute<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'_, Value>>,
) -> Result<Tagged<'a, Value>, VmError> {
    let stack = state.stack();

    let saved_top = stack.top();
    let was_active = state.is_frame_active();
    let outer = was_active.then(|| state.frame_base());

    state.handle_scope(|scope| {
        let result = enter(vm, heap, state, callable, args, new_target)?;
        let rooted = scope.handle(result);
        stack.set_top(saved_top);
        if let Some(outer) = outer {
            state.set_frame_base(outer);
        } else {
            state.set_frame_active(false);
        }
        Ok(rooted.as_tagged(heap))
    })
}

/// The `#[handler]`-based interpreter (work in progress: only the
/// opcodes registered in `NEW_TABLE_NARROW` run; the rest trap).
pub struct NewInterpreter;

impl Interpreter for NewInterpreter {
    const EXECUTE: EntryFn = execute;
}
