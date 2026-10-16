#![allow(dead_code)]

use core::cell::Cell;
use handler_derive::{handler, handler_table};

use bytecode::Opcode;
use vm_core::proxy::Proxy;
use vm_core::{
    Args, CallTarget, Callee, Coercion, Convert, DenseString, EntryFn, FrameMeta, Handle,
    HandleSlice, Heap, Hint, HostCtx, Interpreter, NativeIndex, Object, Register, Smi, Tagged,
    ThreadState, VM, Value, VmError, Word, interpreter::SAFEPOINT_INTERVAL, raise_runtime,
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

/// Overwrite the accumulator with an already-encoded word.
macro_rules! set_acc_raw {
    ($acc:ident, $v:expr) => {
        $acc = $v
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
    pub fn call_runtime(&mut self, rt: NativeIndex, args: Args) -> Acc {
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
        Acc::set(v)
    }

    #[inline(always)]
    pub fn raise_tag(&mut self, err: VmError) -> Acc {
        let (vm, state) = (self.vm, self.state);
        Acc::set(raise_runtime(vm, self.heap_mut(), state, err))
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

/// Tail into a cold helper with operand access. The callee owns its
/// continuation: it computes a result and re-enters with
/// `set_acc_raw` + `reenter!` (a plain `return` would end the frame).
macro_rules! slow {
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
            let ex = Acc::set(
                frame
                    .ctx()
                    .state()
                    .take_pending_exception_tagged(frame.ctx().heap())
                    .expect("pending exception must be set while unwinding"),
            );
            frame.set_acc_raw(ex);
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
        slow!(slow_box_number)
    }
    slow!(slow_add)
}

#[handler(Add)]
#[cold]
fn slow_box_number(frame: Frame) -> Acc {
    let pc = frame.pc();
    frame.set_acc_raw(Acc::set(frame.ctx().heap_mut().new_number(frame.float())));
    reenter!(pc)
}

#[handler(Add)]
#[cold]
fn slow_add(frame: Frame) -> Acc {
    let pc = frame.pc();
    let state = frame.ctx().state();
    let v: Acc = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(frame.acc());
        let vm = frame.ctx().vm();
        let heap = frame.ctx().heap_mut();
        let lhs = match Object::to_primitive(vm, heap, state, lhs, Hint::Default) {
            Ok(Coercion::Threw) => return Acc::set(heap.known().exception.as_tagged(heap).erase()),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
        };
        let rhs = match Object::to_primitive(vm, heap, state, rhs, Hint::Default) {
            Ok(Coercion::Threw) => return Acc::set(heap.known().exception.as_tagged(heap).erase()),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
        };
        if lhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
            || rhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
        {
            let a = match Convert::to_string(heap, &scope, lhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
            };
            let b = match Convert::to_string(heap, &scope, rhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
            };
            Acc::set(
                DenseString::concat(heap, &scope, a, b)
                    .as_tagged(heap)
                    .erase(),
            )
        } else {
            let a = match lhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
            };
            let b = match rhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
            };
            let sum = a + b;
            match Smi::from_f64(sum) {
                Some(s) => Acc::set(s.into_tagged()),
                None => Acc::set(heap.new_number(sum)),
            }
        }
    });
    frame.set_acc_raw(v);
    reenter!(pc)
}

#[handler(CallNoFeedback)]
#[cold]
fn slow_proxy_apply(frame: Frame) -> Acc {
    let pc = frame.pc();
    let args_base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let heap = frame.ctx().heap_mut();
    let v: Acc = state.handle_scope(|scope| {
        let callee = scope.handle(frame.regs().get(frame.signed::<0>(), heap));
        let staged = state.stack().args(state.frame_base(), args_base, count);
        match Proxy::apply(vm, heap, state, callee, staged) {
            Ok(Coercion::Threw) => Acc::set(heap.known().exception.as_tagged(heap).erase()),
            Ok(Coercion::Value(v)) => Acc::set(v),
            Err(err) => Acc::set(raise_runtime(vm, heap, state, err)),
        }
    });
    frame.set_acc_raw(v);
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
            frame.set_acc_raw(frame.ctx().raise_tag(VmError::Type));
            throw!()
        }
        Some(CallTarget::Proxy(_)) => {
            slow!(slow_proxy_apply)
        }
        Some(CallTarget::Native(rt)) => {
            let v = frame.ctx().call_runtime(
                rt,
                frame
                    .ctx()
                    .stack()
                    .window(frame.ctx().frame_base(), base_r, count),
            );
            frame.set_acc_raw(v);
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
                frame.set_acc_raw(frame.ctx().raise_tag(VmError::Type));
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
                Err(err) => return frame.ctx().raise_tag(err),
            };
            state.set_frame_base(pushed.base);
            let entry = state.stack().frame_header(pushed.base).code(heap).as_ptr();
            let regs = unsafe { Regs::new(state.stack().slots_ptr().add(pushed.base)) };
            frame.set_acc(undefined);
            dispatch!(entry)
        }
    }
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
);

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
