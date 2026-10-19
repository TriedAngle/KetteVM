#![allow(dead_code)]

use core::cell::Cell;
use handler_derive::{handler, handler_table};

use bytecode::Opcode;
use vm_core::ic::{CallHit, CallProbe, CallRecord, InlineCache};
use vm_core::proxy::Proxy;
use vm_core::{
    Args, CallTarget, Callee, Coercion, Convert, DenseString, EntryFn, FeedbackVector, FrameMeta,
    Handle, HandleSlice, Heap, Hint, HostCtx, Interpreter, NativeIndex, Object, Recv, Register,
    Smi, Tagged, ThreadState, VM, Value, VmError, Word, interpreter::SAFEPOINT_INTERVAL,
    raise_runtime,
};

#[cfg(feature = "star_fusion")]
use crate::star_lookahead;
use crate::{FloatReg, Ops, Regs, RootsArg};

/// Placeholder receiver type for `#[handler]` fns. The macro rewrites
/// every `frame.method(..)` call away before compilation (into one of
/// the documented `macro_rules!` below or a `ctx` call), so a value of
/// this type is never constructed; it exists so the handwritten
/// signatures stay greppable.
#[doc(hidden)]
pub struct Frame;

// The `frame.` reads and writes, in macro form for the same reason as
// the exits below: documented once here, and resolvable by the IDE
// through #[handler]'s span mapping. `#[handler]` rewrites
// `frame.m(..)` into `m!(<params>, ..)`, with the generated
// parameters passed explicitly (macro hygiene hides locals).

/// The instruction pointer: the first byte of this instruction.
macro_rules! ip {
    ($ip:expr) => {
        $ip
    };
}

/// The frame's register window.
macro_rules! regs {
    ($r:expr) => {
        $r
    };
}

/// The accumulator as a view under the current epoch. The view is
/// invalidated by the next allocation.
macro_rules! acc {
    ($acc:expr, $x:expr) => {{ $acc.get($x.heap()) }};
}

/// The accumulator as the raw, epoch-free word.
macro_rules! acc_raw {
    ($acc:expr) => {
        $acc
    };
}

/// The float-register payload handed over by a slow path.
macro_rules! float {
    ($f:expr) => {{ $f.get() }};
}

/// The instruction pointer as an offset into the current code.
macro_rules! pc {
    ($ip:expr, $x:expr) => {{ $ip as usize - $x.code_ptr() as usize }};
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
    safepoints: Cell<u32>,
}

impl<'s, 'h> Ctx<'s, 'h> {
    #[inline(always)]
    pub fn new(vm: &'s VM, state: &'s ThreadState, heap: &'h mut Heap, base_anchor: usize) -> Self {
        Ctx {
            vm,
            state,
            heap,
            base_anchor,
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

    #[inline(always)]
    pub fn code_ptr(&self) -> *const u8 {
        self.stack()
            .frame_header(self.frame_base())
            .code(self.heap())
            .as_ptr()
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

    /// Get the accumulator as a view under the current epoch.
    /// Acc becomes invalid after the first allocation!
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

/// Dispatch to the handler for the instruction at `$p`. `$p` must be
/// pure: it is evaluated twice.
macro_rules! dispatch {
    ($p:expr, $a:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let h = $t.get(unsafe { *$p });
        become h($p, $r, $a, $x, $t, $k, $f)
    }};
}

/// Advance past this instruction and dispatch. The size is baked at
/// compile time from the opcode and `STRIDE`; `star_fusion` peeks one
/// byte ahead and folds a following `Store` of the acc.
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

/// Re-take the (possibly switched) frame and continue at the next
/// instruction, diverting to `throw_dispatch` if the acc is the
/// exception sentinel.
macro_rules! reenter {
    ($op:ident, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let fault_ip = unsafe { $ctx.code_ptr().add($pc) };
        if $acc.get($ctx.heap()).raw().to_bits()
            == $ctx
                .heap()
                .known()
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

/// Tail into a named handler fn with the full dispatch ABI.
macro_rules! tail {
    ($ip:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr, $slow:path) => {
        become $slow($ip, $regs, $acc, $ctx, $table, $roots, $float)
    };
}

/// Dispatch to the instruction at `$ip + $off` — the jump-target form
/// of `dispatch!` for a signed operand offset.
macro_rules! jump {
    ($ip:expr, $off:expr, $a:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {
        dispatch!($ip.wrapping_offset($off as isize), $a, $r, $x, $t, $k, $f)
    };
}

/// Raise the pending exception: tail into the unwinder with the
/// current instruction as the fault site.
macro_rules! throw {
    ($ip:expr, $r:expr, $a:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {
        become throw_dispatch::<STRIDE>($ip, $r, $a, $x, $t, $k, $f)
    };
}

/// A loop back-edge: every `SAFEPOINT_INTERVAL` ticks park the acc,
/// poll the collector, and re-enter through the handler table.
macro_rules! backedge {
    ($ip:expr, $target:expr, $r:expr, $a:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        if $x.safepoint_tick() {
            let state = $x.state;
            let acc_slot = state.acc_slot();
            acc_slot.store($a.get($x.heap()));
            if $x.heap_mut().safepoint_poll() {
                state.set_termination(vm_core::Termination::Shutdown);
                state.set_pending_exception($x.heap().known().undefined.as_tagged($x.heap()));
                become throw_dispatch::<STRIDE>($ip, $r, $a, $x, $t, $k, $f)
            }
            let pc = $target as usize - $x.code_ptr() as usize;
            let acc = Acc::set(acc_slot.get($x.heap()));
            let p = unsafe { $x.code_ptr().add(pc) };
            let regs = unsafe { Regs::new($x.regs_ptr()) };
            dispatch!(p, acc, regs, $x, $t, $k, $f)
        }
        dispatch!($target, $a, $r, $x, $t, $k, $f)
    }};
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

#[handler]
fn op_trap(frame: Frame) -> Acc {
    let byte = unsafe { *frame.ip() };
    unreachable!("unimplemented opcode {:?}", Opcode::from_byte(byte))
}

#[handler]
fn throw_dispatch(frame: Frame) {
    let mut pc = frame.pc();
    loop {
        let base = frame.ctx().frame_base();
        let handled = if frame.ctx().state().termination().is_some() {
            // A termination is not an exception: no handler (catch or
            // finally in any frame) may observe or intercept it.
            None
        } else {
            let callable = frame
                .ctx()
                .stack()
                .frame_header(base)
                .callable_slot()
                .get(frame.ctx().heap());
            callable
                .as_heap_object()
                .and_then(|obj| obj.callable_info(frame.ctx().heap()))
                .and_then(|info| info.handlers.get(frame.ctx().heap()))
                .and_then(|handlers| handlers.lookup(pc))
        };
        if let Some(handler_pc) = handled {
            // park the word before the continuation re-takes ctx
            // exclusively: an Acc carries no heap borrow
            let ex = frame
                .ctx()
                .state()
                .take_pending_exception_tagged(frame.ctx().heap())
                .expect("pending exception must be set while unwinding");
            frame.set_acc(ex);
            dispatch_frame!(handler_pc)
        }
        if base == frame.ctx().base_anchor() {
            return Acc::set(
                frame
                    .ctx()
                    .heap()
                    .known()
                    .exception
                    .as_tagged(frame.ctx().heap())
                    .erase(),
            );
        }
        let caller = frame.ctx().stack().pop_frame(base);
        frame.ctx().set_frame_base(caller.base);
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
            Ok(Coercion::Threw) => return heap.known().exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let rhs = match Object::to_primitive(vm, heap, state, rhs, Hint::Default) {
            Ok(Coercion::Threw) => return heap.known().exception.as_tagged(heap).erase(),
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

#[handler(CallNoFeedback)]
#[cold]
fn slow_proxy_apply(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_window_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        frame.signed::<1>(),
        frame.unsigned::<2>(),
    );
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
    let off = frame.signed::<0>();
    jump!(off)
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
    if frame.acc()
        != frame
            .ctx()
            .heap()
            .known()
            .undefined
            .as_tagged(frame.ctx().heap())
            .erase()
    {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpLoop)]
fn op_jump_loop(frame: Frame) -> Acc {
    let target = frame.ip().wrapping_offset(frame.signed::<0>() as isize);
    backedge!(target)
}

#[handler(Return)]
fn op_return(frame: Frame) -> Acc {
    frame.opcode_check();
    if frame.ctx().frame_base() == frame.ctx().base_anchor() {
        return frame.acc_raw();
    }
    let caller = frame.ctx().stack().pop_frame(frame.ctx().frame_base());
    frame.ctx().set_frame_base(caller.base);
    dispatch_frame!(caller.pc)
}

#[handler(CallNoFeedback)]
fn op_call_no_feedback(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallNoFeedback.operands().len() };
    let callee_r = frame.signed::<0>();
    let base_r = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let ct = frame
        .regs()
        .get(callee_r, frame.ctx().heap())
        .call_target(frame.ctx().heap());
    match ct {
        None => {
            frame.set_acc(frame.ctx().raise_tag(VmError::Type));
            throw!()
        }
        Some(CallTarget::Proxy(_)) => {
            tail!(slow_proxy_apply)
        }
        Some(CallTarget::Native(rt)) => {
            let v = frame.ctx().call_runtime(
                rt,
                frame
                    .ctx()
                    .stack()
                    .window(frame.ctx().frame_base(), base_r, count),
            );
            frame.set_acc(v);
            reenter!(pc)
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if kind.is_class_constructor() {
                frame.set_acc(frame.ctx().raise_tag(VmError::Type));
                throw!()
            }
            let state = frame.ctx().state();
            let meta = frame.ctx().caller_meta(pc + size, pc);
            let args = state.stack().window(state.frame_base(), base_r, count);
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let pushed = match state.stack().push_frame(
                heap,
                meta,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                undefined,
                args,
            ) {
                Ok(pushed) => pushed,
                Err(err) => return Acc::set(frame.ctx().raise_tag(err)),
            };
            state.set_frame_base(pushed.base);
            let entry = state.stack().frame_header(pushed.base).code(heap).as_ptr();
            let regs = unsafe { Regs::new(state.stack().slots_ptr().add(pushed.base)) };
            frame.set_acc(undefined);
            dispatch!(entry)
        }
    }
}

#[handler(Call)]
fn op_call(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::Call.operands().len() };
    let mc = match call_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        frame.signed::<1>(),
        frame.unsigned::<2>(),
        frame.unsigned::<3>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_proxy),
    }
}

#[handler(CallMethod0)]
fn op_call_method0(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallMethod0.operands().len() };
    let mc = match call_method_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        frame.signed::<1>(),
        [0, 0],
        0,
        frame.unsigned::<2>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_method0_proxy),
    }
}

#[handler(CallMethod1)]
fn op_call_method1(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallMethod1.operands().len() };
    let mc = match call_method_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        frame.signed::<1>(),
        [frame.signed::<2>(), 0],
        1,
        frame.unsigned::<3>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_method1_proxy),
    }
}

#[handler(CallMethod2)]
fn op_call_method2(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallMethod2.operands().len() };
    let mc = match call_method_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        frame.signed::<1>(),
        [frame.signed::<2>(), frame.signed::<3>()],
        2,
        frame.unsigned::<4>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_method2_proxy),
    }
}

#[handler(CallFunction0)]
fn op_call_function0(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallFunction0.operands().len() };
    let mc = match call_function_start(
        frame.ctx(), frame.regs(), frame.signed::<0>(), pc, size, [0, 0], 0, frame.unsigned::<1>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_function0_proxy),
    }
}

#[handler(CallFunction1)]
fn op_call_function1(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallFunction1.operands().len() };
    let mc = match call_function_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        [frame.signed::<1>(), 0],
        1,
        frame.unsigned::<2>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_function1_proxy),
    }
}

#[handler(CallFunction2)]
fn op_call_function2(frame: Frame) -> Acc {
    let pc = frame.pc();
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallFunction2.operands().len() };
    let mc = match call_function_start(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        pc,
        size,
        [frame.signed::<1>(), frame.signed::<2>()],
        2,
        frame.unsigned::<3>(),
    ) {
        Ok(mc) => mc,
        Err(err) => {
            frame.set_acc(frame.ctx().raise_tag(err));
            throw!()
        }
    };
    match mc {
        MethodCall::Value(v) => {
            frame.set_acc(v);
            reenter!(pc)
        }
        MethodCall::Frame(_) => {
            let heap = frame.ctx().heap();
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            frame.set_acc(undefined);
            dispatch!(frame.ctx().code_ptr())
        }
        MethodCall::Proxy => tail!(slow_call_function2_proxy),
    }
}

#[handler(Call)]
#[cold]
fn slow_call_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_window_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        frame.signed::<1>(),
        frame.unsigned::<2>(),
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallMethod0)]
#[cold]
fn slow_call_method0_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Reg(frame.signed::<1>()),
        [0, 0],
        0,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallMethod1)]
#[cold]
fn slow_call_method1_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Reg(frame.signed::<1>()),
        [frame.signed::<2>(), 0],
        1,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallMethod2)]
#[cold]
fn slow_call_method2_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Reg(frame.signed::<1>()),
        [frame.signed::<2>(), frame.signed::<3>()],
        2,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallFunction0)]
#[cold]
fn slow_call_function0_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Undefined,
        [0, 0],
        0,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallFunction1)]
#[cold]
fn slow_call_function1_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Undefined,
        [frame.signed::<1>(), 0],
        1,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CallFunction2)]
#[cold]
fn slow_call_function2_proxy(frame: Frame) -> Acc {
    let pc = frame.pc();
    let v = proxy_apply_scattered_slow(
        frame.ctx(),
        frame.regs(),
        frame.signed::<0>(),
        Recv::Undefined,
        [frame.signed::<1>(), frame.signed::<2>()],
        2,
    );
    frame.set_acc(v);
    reenter!(pc)
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

enum MethodCall<'a> {
    /// the call completed (runtime callee): the value or the exception
    /// sentinel
    Value(Tagged<'a, Value>),
    /// a bytecode frame was pushed and made current: enter it through
    /// the handler table
    Frame(FrameMeta),
    /// a callable proxy: the caller tail-calls the slow proxy dispatch
    Proxy,
}

/// Invoke a runtime callee over scattered call-site registers.
#[inline(always)]
fn dispatch_runtime_scattered<'a>(
    x: &'a mut Ctx<'_, '_>,
    rt: NativeIndex,
    recv: Recv,
    args: [i32; 2],
    argc: usize,
) -> Tagged<'a, Value> {
    match x
        .stack()
        .stage_scattered(x.heap(), x.frame_base(), recv, args, argc)
    {
        Ok(a) => x.call_runtime(rt, a),
        Err(err) => x.raise_tag(err),
    }
}

/// Push a callee frame (facts from a `CallTarget::Bytecode` destructure
/// or a call-IC hit; the caller is the current frame) and switch to it.
/// Entry is at pc 0 with the accumulator seeded undefined.
#[inline(always)]
fn push_callee_frame<'a>(
    x: &Ctx<'_, '_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    args: Args,
) -> Result<MethodCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = heap.known().undefined.as_tagged(heap).erase();
    let frame = x.stack().push_frame(heap, meta, callee, undefined, args)?;
    x.set_frame_base(frame.base);
    Ok(MethodCall::Frame(frame))
}

/// Push a callee frame from scattered call-site registers (one copy
/// per word).
#[inline(always)]
fn push_scattered_frame<'a>(
    x: &Ctx<'_, '_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    recv: Recv,
    args: [i32; 2],
    argc: usize,
) -> Result<MethodCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = heap.known().undefined.as_tagged(heap).erase();
    let frame =
        x.stack()
            .push_scattered_frame(heap, meta, callee, undefined, recv, args, argc)?;
    x.set_frame_base(frame.base);
    Ok(MethodCall::Frame(frame))
}

#[inline(always)]
fn call_method_start<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    recv: i32,
    args: [i32; 2],
    argc: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee_word) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            push_scattered_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                Recv::Reg(recv),
                args,
                argc,
            )
        }
        CallProbe::Native(rt) => Ok(MethodCall::Value(dispatch_runtime_scattered(
            x,
            rt,
            Recv::Reg(recv),
            args,
            argc,
        ))),
        CallProbe::Miss => slow_call_method_miss(x, regs, callee_reg, pc, size, recv, args, argc, fb),
    }
}

#[inline(always)]
fn call_function_start<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    args: [i32; 2],
    argc: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee_word) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            push_scattered_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                Recv::Undefined,
                args,
                argc,
            )
        }
        CallProbe::Native(rt) => Ok(MethodCall::Value(dispatch_runtime_scattered(
            x,
            rt,
            Recv::Undefined,
            args,
            argc,
        ))),
        CallProbe::Miss => slow_call_function_miss(x, regs, callee_reg, pc, size, args, argc, fb),
    }
}

/// `Call` (contiguous `[receiver, args...]` window) with the same call IC.
#[inline(always)]
fn call_start<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    base: i32,
    count: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee_word) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            push_callee_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                x.stack().window(x.frame_base(), base, count),
            )
        }
        CallProbe::Native(rt) => Ok(MethodCall::Value(
            x.call_runtime(rt, x.stack().window(x.frame_base(), base, count)),
        )),
        CallProbe::Miss => slow_call_miss(x, regs, callee_reg, pc, size, base, count, fb),
    }
}

#[cold]
#[inline(never)]
fn slow_call_method_miss<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    recv: i32,
    args: [i32; 2],
    argc: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match callee_word.call_target(x.heap()) {
        None => Ok(MethodCall::Value(x.raise_tag(VmError::Type))),
        Some(CallTarget::Proxy(_)) => Ok(MethodCall::Proxy),
        Some(CallTarget::Native(rt)) => {
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Native(rt),
            );
            Ok(MethodCall::Value(dispatch_runtime_scattered(
                x,
                rt,
                Recv::Reg(recv),
                args,
                argc,
            )))
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Bytecode(info),
            );
            push_scattered_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                Recv::Reg(recv),
                args,
                argc,
            )
        }
    }
}

#[cold]
#[inline(never)]
fn slow_call_function_miss<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    args: [i32; 2],
    argc: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match callee_word.call_target(x.heap()) {
        None => Ok(MethodCall::Value(x.raise_tag(VmError::Type))),
        Some(CallTarget::Proxy(_)) => Ok(MethodCall::Proxy),
        Some(CallTarget::Native(rt)) => {
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Native(rt),
            );
            Ok(MethodCall::Value(dispatch_runtime_scattered(
                x,
                rt,
                Recv::Undefined,
                args,
                argc,
            )))
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Bytecode(info),
            );
            push_scattered_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                Recv::Undefined,
                args,
                argc,
            )
        }
    }
}

#[cold]
#[inline(never)]
fn slow_call_miss<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    pc: usize,
    size: usize,
    base: i32,
    count: usize,
    fb: usize,
) -> Result<MethodCall<'a>, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    match callee_word.call_target(x.heap()) {
        None => Ok(MethodCall::Value(x.raise_tag(VmError::Type))),
        Some(CallTarget::Proxy(_)) => Ok(MethodCall::Proxy),
        Some(CallTarget::Native(rt)) => {
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Native(rt),
            );
            Ok(MethodCall::Value(
                x.call_runtime(rt, x.stack().window(x.frame_base(), base, count)),
            ))
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Ok(MethodCall::Value(x.raise_tag(VmError::Type)));
            }
            InlineCache::call_update(
                x.heap(),
                x.feedback_ref(x.heap()),
                fb,
                callee_word,
                CallRecord::Bytecode(info),
            );
            push_callee_frame(
                x,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                x.stack().window(x.frame_base(), base, count),
            )
        }
    }
}

/// Apply a callable proxy over a windowed call site (`Call`/
/// `CallNoFeedback`): the window is already rooted inside the frame.
#[cold]
#[inline(never)]
fn proxy_apply_window_slow<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    args_base: i32,
    count: usize,
) -> Tagged<'a, Value> {
    let vm = x.vm();
    let state = x.state();
    let staged = state.stack().args(x.frame_base(), args_base, count);
    let heap = x.heap_mut();
    let v = state.handle_scope(|scope| {
        let callee = scope.handle(regs.get(callee_reg, heap));
        match Proxy::apply(vm, heap, state, callee, staged) {
            Ok(Coercion::Threw) => heap.known().exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => v,
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    });
    v
}

/// Apply a callable proxy over scattered call-site registers: stage
/// `[receiver, args...]` at the top, root it for the call, and restore
/// the top afterwards.
#[cold]
#[inline(never)]
fn proxy_apply_scattered_slow<'a>(
    x: &'a mut Ctx<'_, '_>,
    regs: Regs,
    callee_reg: i32,
    recv: Recv,
    args: [i32; 2],
    argc: usize,
) -> Tagged<'a, Value> {
    let vm = x.vm();
    let state = x.state();
    let base = x.frame_base();
    let heap = x.heap_mut();
    let staged = match state.stack().stage_scattered(heap, base, recv, args, argc) {
        Ok(a) => a,
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    let saved_top = state.stack().top();
    if let Some(top) = state.stack().rooting_top(staged) {
        state.stack().set_top(top);
    }
    let v = state.handle_scope(|scope| {
        let callee = scope.handle(regs.get(callee_reg, heap));
        match Proxy::apply(vm, heap, state, callee, staged) {
            Ok(Coercion::Threw) => heap.known().exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => v,
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    });
    state.stack().set_top(saved_top);
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
            let new_target_value = match &new_target {
                Some(nt) => nt.as_tagged(heap).erase(),
                None => heap.known().undefined.as_tagged(heap).erase(),
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
