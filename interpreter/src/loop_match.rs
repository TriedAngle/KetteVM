use crate::RootsArg;
use crate::handler;
use crate::slow::match_slow_construct;
use crate::utils::{
    CallFault, ConstructStart, StagedCall, bound_apply_staged, call_native_target,
    call_scatter_start, call_scatter_start_sized, call_window_start, construct_native,
    construct_start, load_operator, proxy_apply_staged, raise_fault, resolve_target,
};

use bytecode::Opcode;
use handler_derive::match_step;
use vm_core::{
    Acc, CallTarget, Callee, Ctx, EntryFn, ErasedRuntimeState, Errors, FrameMeta, Handle,
    HandleSlice, Heap, Interpreter, Object, Ops, Regs, Result, Tagged, Termination, ThreadState,
    Thrown, Unwind, VM, Value, VmError, bail, interpreter::base, interpreter::unwind, raise,
};

/// A dispatcher entry for the shared frame setup.
pub type DispatchFn = for<'h> fn(&mut Ctx<'h>) -> Result<Acc>;

#[cfg(not(debug_assertions))]
pub struct LoopMatchInterpreter;

#[cfg(not(debug_assertions))]
impl Interpreter for LoopMatchInterpreter {
    const EXECUTE: EntryFn = execute;
}

#[inline(always)]
pub fn offset(ctx: &Ctx<'_>, pc: usize) -> *const u8 {
    unsafe { ctx.code_ptr().add(pc) }
}

/// Successor sentinel meaning "this instruction threw"
#[inline(always)]
pub fn throw_ptr() -> *const u8 {
    usize::MAX as *const u8
}

/// Decode the current instruction into caller-named locals
macro_rules! decode {
    ($op:ident, $ip:expr, $ops:ident, $next:ident) => {
        let $ops = unsafe { Ops::<STRIDE>::new($ip, Opcode::$op) };
        let $next =
            unsafe { $ip.add(base::<STRIDE>() + STRIDE * const { Opcode::$op.operands().len() }) };
    };
    ($op:ident, $ip:expr, $ctx:expr, $ops:ident, $next:ident, $pc:ident, $next_pc:ident) => {
        decode!($op, $ip, $ops, $next);
        let $pc = $ip as usize - $ctx.code_ptr() as usize;
        let $next_pc = $pc + ($next as usize - $ip as usize);
    };
}

/// The current and successor byte offsets
macro_rules! offsets {
    ($ip:expr, $next:expr, $ctx:expr) => {{
        let pc = $ip as usize - $ctx.code_ptr() as usize;
        (pc, pc + ($next as usize - $ip as usize))
    }};
}

/// Exit to the decoded successor pointer.
macro_rules! next {
    ($next:expr) => {
        return $next
    };
}

/// Exit to an explicit instruction pointer (a jump target).
macro_rules! dispatch {
    ($p:expr) => {
        return $p
    };
}

/// Read the accumulator.
macro_rules! acc {
    ($acc:expr, $ctx:expr) => {
        $acc.get($ctx.heap())
    };
}

macro_rules! set_acc {
    ($acc:expr, $v:expr) => {
        *$acc = Acc::set($v)
    };
}

macro_rules! set_acc_smi {
    ($acc:expr, $bits:expr) => {
        *$acc = Acc::from_smi_bits($bits)
    };
}

/// call into a cold handler, here its an actual call, but same name to have one way to write it.
macro_rules! tail {
    ($slow:ident, $ip:expr, $regs:expr, $acc:expr, $ctx:expr, $roots:expr) => {
        return $slow::<STRIDE>($ip, $regs, $acc, $ctx, $roots)
    };
}

/// reenter as a cold
macro_rules! reenter {
    ($ctx:expr, $acc:expr, $pc:expr, $next_pc:expr, $v:expr) => {{
        let v = $v;
        let raw = v.raw().to_bits();
        // Consume the value (and with it the heap borrow) before touching
        // `ctx` again; the sentinel word is then read through a fresh
        // shared borrow.
        *$acc = Acc::set(v);
        let sentinel = $ctx.vm().known().exception.get($ctx.heap()).raw().to_bits();
        $ctx.reload_code();
        if raw == sentinel {
            $ctx.set_throw_pc($pc);
            throw_ptr()
        } else {
            offset($ctx, $next_pc)
        }
    }};
}

/// Raise a fault, re-deriving the code pointer (materializing the
/// exception may collect), and re-enter at the faulting instruction.
macro_rules! raise {
    ($ctx:expr, $acc:expr, $pc:expr, $err:expr) => {{
        *$acc = Acc::set(raise_fault($ctx, $err));
        $ctx.reload_code();
        $ctx.set_throw_pc($pc);
        throw_ptr()
    }};
}

#[cfg(not(debug_assertions))]
pub fn execute<'a>(
    vm: &VM,
    runtime: &dyn ErasedRuntimeState,
    heap: &'a mut Heap,
    state: &ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'_, Value>>,
) -> Result<Tagged<'a, Value>> {
    execute_with(
        vm,
        runtime,
        heap,
        state,
        callable,
        args,
        new_target,
        match_dispatch,
    )
}

pub fn execute_with<'a>(
    vm: &VM,
    runtime: &dyn ErasedRuntimeState,
    heap: &'a mut Heap,
    state: &ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'_, Value>>,
    dispatch: DispatchFn,
) -> Result<Tagged<'a, Value>> {
    if state.rust_stack_exhausted() {
        bail!(vm, heap, state, VmError::StackOverflow);
    }
    let stack = state.stack();

    let saved_top = stack.top();
    let was_active = state.is_frame_active();
    let outer = was_active.then(|| state.frame_base());

    state.handle_scope(|scope| {
        let result = start(
            vm, runtime, heap, state, callable, args, new_target, dispatch,
        )?;
        let rooted = scope.handle(result);
        let threw = rooted.as_tagged(heap) == heap.known().exception.get(heap);

        stack.set_top(saved_top);
        if let Some(outer) = outer {
            state.set_frame_base(outer);
        } else {
            state.set_frame_active(false);
        }
        if threw {
            Err(Thrown)
        } else {
            Ok(rooted.as_tagged(heap))
        }
    })
}

fn start<'b>(
    vm: &'b VM,
    runtime: &dyn ErasedRuntimeState,
    heap: &'b mut Heap,
    state: &'b ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'b, Value>>,
    dispatch: DispatchFn,
) -> Result<Tagged<'b, Value>> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => bail!(vm, heap, state, VmError::NotCallable),
        // proxy and bound-function calls are intercepted before `execute`
        Some(CallTarget::Proxy(_)) | Some(CallTarget::Bound { .. }) => {
            bail!(vm, heap, state, VmError::NotCallable)
        }
        Some(CallTarget::Native(rt)) => Ok(call_native_target(
            vm,
            runtime,
            heap,
            state,
            rt,
            callable.erase(),
            args,
            new_target,
        )),
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if new_target.is_none() && kind.is_class_constructor() {
                bail!(vm, heap, state, VmError::ClassConstructorCalled);
            }
            let stack = state.stack();
            let new_target_value = match &new_target {
                Some(nt) => nt.as_tagged(heap).erase(),
                None => vm.known().undefined.get(heap).erase(),
            };
            let frame = match stack.push_frame_values(
                heap,
                FrameMeta::ROOT,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                new_target_value,
                args.as_tagged(),
            ) {
                Some(frame) => frame,
                None => {
                    bail!(vm, heap, state, VmError::StackOverflow);
                }
            };
            state.set_frame_base(frame.base);
            state.set_frame_active(true);

            let mut ctx = Ctx::new(vm, state, heap, frame.base);
            let value = dispatch(&mut ctx)?;
            Ok(value.get(heap))
        }
    }
}

#[cfg(not(debug_assertions))]
fn match_dispatch<'h>(ctx: &mut Ctx<'h>) -> Result<Acc> {
    let mut ip = ctx.code_ptr();
    let mut regs = unsafe { Regs::new(ctx.regs_ptr()) };
    let roots = RootsArg::new(ctx.vm().known());
    let mut acc = Acc::set(roots.undefined.get(ctx.heap()).erase());
    let mut frame_base = ctx.frame_base();

    loop {
        step::<1>(ctx, &mut ip, &mut regs, &mut acc, &mut frame_base, roots);

        // A null successor is become's `return acc`: the execution is done.
        if ip.is_null() {
            return Ok(acc);
        }

        // The throw sentinel is become's `throw_dispatch`: unwind from
        // the faulting pc recorded by the handler. Two pointer compares
        // are the whole per-instruction flow cost.
        if ip == throw_ptr() {
            match unwind(ctx, ctx.throw_pc()) {
                Unwind::Caught { pc, ex } => {
                    acc = Acc::set(ex);
                    frame_base = ctx.frame_base();
                    regs = unsafe { Regs::new(ctx.regs_ptr()) };
                    ctx.reload_code();
                    ip = offset(ctx, pc);
                }
                Unwind::Escaped => {
                    return Ok(Acc::set(roots.exception.get(ctx.heap()).erase()));
                }
            }
        }
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn step<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h>,
    ip: &mut *const u8,
    regs: &mut Regs,
    acc: &mut Acc,
    frame_base: &mut usize,
    roots: RootsArg<'h>,
) {
    let op = unsafe { Opcode::from_byte_unchecked(*(*ip).add(if STRIDE == 2 { 1 } else { 0 })) };

    match_step!(
        Load => handler::match_op_load::<STRIDE>,
        Move => handler::match_op_move::<STRIDE>,
        Store => handler::match_op_store::<STRIDE>,
        Add => handler::match_op_add::<STRIDE>,
        CallNative => regs handler::match_op_call_native::<STRIDE>,
        Jump => handler::match_op_jump::<STRIDE>,
        JumpIfTruthy => handler::match_op_jump_if_truthy::<STRIDE>,
        JumpIfFalsy => handler::match_op_jump_if_falsy::<STRIDE>,
        JumpIfNotUndefined => handler::match_op_jump_if_not_undefined::<STRIDE>,
        JumpIfNullish => handler::match_op_jump_if_nullish::<STRIDE>,
        BindThis => handler::match_op_bind_this::<STRIDE>,
        JumpLoop => op_jump_loop::<STRIDE>,
        Return => regs step_return::<STRIDE>,
        CallNoFeedback => regs op_call_no_feedback::<STRIDE>,
        Call => regs op_call::<STRIDE>,
        CallMethod0 => regs step_call_method0::<STRIDE>,
        CallMethod1 => regs step_call_method1::<STRIDE>,
        CallMethod2 => regs step_call_method2::<STRIDE>,
        BinaryOp => regs step_binary_op::<STRIDE>,
        UnaryOp => regs step_unary_op::<STRIDE>,
        CallFunction0 => regs step_call_function0::<STRIDE>,
        CallFunction1 => regs step_call_function1::<STRIDE>,
        CallFunction2 => regs step_call_function2::<STRIDE>,
        Construct => regs op_construct::<STRIDE>,
        Wide => wide,
        LoadSmi => handler::match_op_load_smi::<STRIDE>,
        LoadZero => handler::match_op_load_zero::<STRIDE>,
        LoadConstant => handler::match_op_load_constant::<STRIDE>,
        LoadUndefined => handler::match_op_load_undefined::<STRIDE>,
        LoadNull => handler::match_op_load_null::<STRIDE>,
        LoadTrue => handler::match_op_load_true::<STRIDE>,
        LoadFalse => handler::match_op_load_false::<STRIDE>,
        TailGuard => handler::match_op_tail_guard::<STRIDE>,
        TestType => handler::match_op_test_type::<STRIDE>,
        LoadLength => handler::match_op_load_length::<STRIDE>,
        LoadGlobal => handler::match_op_load_global::<STRIDE>,
        Sub => handler::match_op_sub::<STRIDE>,
        Mul => handler::match_op_mul::<STRIDE>,
        Div => handler::match_op_div::<STRIDE>,
        Mod => handler::match_op_mod::<STRIDE>,
        Exp => handler::match_op_exp::<STRIDE>,
        Negate => handler::match_op_negate::<STRIDE>,
        AddImmediate => handler::match_op_add_immediate::<STRIDE>,
        SubImmediate => handler::match_op_sub_immediate::<STRIDE>,
        MulImmediate => handler::match_op_mul_immediate::<STRIDE>,
        DivImmediate => handler::match_op_div_immediate::<STRIDE>,
        ModImmediate => handler::match_op_mod_immediate::<STRIDE>,
        ExpImmediate => handler::match_op_exp_immediate::<STRIDE>,
        AddLoc => handler::match_op_add_loc::<STRIDE>,
        SubLoc => handler::match_op_sub_loc::<STRIDE>,
        BitwiseOr => handler::match_op_bitwise_or::<STRIDE>,
        BitwiseXor => handler::match_op_bitwise_xor::<STRIDE>,
        BitwiseAnd => handler::match_op_bitwise_and::<STRIDE>,
        ShiftLeft => handler::match_op_shift_left::<STRIDE>,
        ShiftRight => handler::match_op_shift_right::<STRIDE>,
        ShiftRightLogical => handler::match_op_shift_right_logical::<STRIDE>,
        BitwiseOrImmediate => handler::match_op_bitwise_or_immediate::<STRIDE>,
        BitwiseXorImmediate => handler::match_op_bitwise_xor_immediate::<STRIDE>,
        BitwiseAndImmediate => handler::match_op_bitwise_and_immediate::<STRIDE>,
        ShiftLeftImmediate => handler::match_op_shift_left_immediate::<STRIDE>,
        ShiftRightImmediate => handler::match_op_shift_right_immediate::<STRIDE>,
        ShiftRightLogicalImmediate => handler::match_op_shift_right_logical_immediate::<STRIDE>,
        Equal => handler::match_op_equal::<STRIDE>,
        LessThan => handler::match_op_less_than::<STRIDE>,
        GreaterThan => handler::match_op_greater_than::<STRIDE>,
        LessThanOrEqual => handler::match_op_less_than_or_equal::<STRIDE>,
        GreaterThanOrEqual => handler::match_op_greater_than_or_equal::<STRIDE>,
        EqualStrict => handler::match_op_equal_strict::<STRIDE>,
        TestReferenceEqual => handler::match_op_test_reference_equal::<STRIDE>,
        TestTypeof => handler::match_op_test_typeof::<STRIDE>,
        InstanceOf => handler::match_op_instance_of::<STRIDE>,
        CompareJump => handler::match_op_compare_jump::<STRIDE>,
        SwitchInt => handler::match_op_switch_int::<STRIDE>,
        SwitchAtom => handler::match_op_switch_atom::<STRIDE>,
        ArraySlice => handler::match_op_array_slice::<STRIDE>,
        IncLoc => handler::match_op_inc_loc::<STRIDE>,
        DecLoc => handler::match_op_dec_loc::<STRIDE>,
        LoadKeyedProperty => handler::match_op_load_keyed_property::<STRIDE>,
        LoadIndexed => handler::match_op_load_indexed::<STRIDE>,
        LoadIndexedOrHole => handler::match_op_load_indexed_or_hole::<STRIDE>,
        LoadKeyedPropertyReg => handler::match_op_load_keyed_property_reg::<STRIDE>,
        LoadElementImm => handler::match_op_load_element_imm::<STRIDE>,
        LoadNamedProperty => handler::match_op_load_named_property::<STRIDE>,
        LoadOwnSlotOrHole => handler::match_op_load_own_slot_or_hole::<STRIDE>,
        OwnSlotCount => handler::match_op_own_slot_count::<STRIDE>,
        StoreNamedProperty => handler::match_op_store_named_property::<STRIDE>,
        StoreKeyedProperty => handler::match_op_store_keyed_property::<STRIDE>,
        StoreIndexed => handler::match_op_store_indexed::<STRIDE>,
        StoreNamedPropertyNoShadow => handler::match_op_store_named_property_no_shadow::<STRIDE>,
        StoreKeyedPropertyNoShadow => handler::match_op_store_keyed_property_no_shadow::<STRIDE>,
        StoreGlobal => handler::match_op_store_global::<STRIDE>,
        CreateClosure => handler::match_op_create_closure::<STRIDE>,
        Throw => handler::match_op_throw::<STRIDE>,
        AddParent => handler::match_op_add_parent::<STRIDE>,
        DefineNamedOwnProperty => handler::match_op_define_named_own_property::<STRIDE>,
        DefineKeyedOwnProperty => handler::match_op_define_keyed_own_property::<STRIDE>,
        StoreInArrayLiteral => handler::match_op_store_in_array_literal::<STRIDE>,
        LoadContextSlot => handler::match_op_load_context_slot::<STRIDE>,
        StoreContextSlot => handler::match_op_store_context_slot::<STRIDE>,
        PushContext => handler::match_op_push_context::<STRIDE>,
        PopContext => handler::match_op_pop_context::<STRIDE>,
        CreateFunctionContext => handler::match_op_create_function_context::<STRIDE>,
        CreateEmptyArrayLiteral => handler::match_op_create_empty_array_literal::<STRIDE>,
        CreateSizedArrayLiteral => handler::match_op_create_sized_array_literal::<STRIDE>,
        CreateRestParameter => handler::match_op_create_rest_parameter::<STRIDE>,
        CreateEmptyObjectLiteral => handler::match_op_create_empty_object_literal::<STRIDE>,
        CreateBlockContext => handler::match_op_create_block_context::<STRIDE>,
        LoadHole => handler::match_op_load_hole::<STRIDE>,
        LoadNewTarget => handler::match_op_load_new_target::<STRIDE>,
        LoadContext => handler::match_op_load_context::<STRIDE>,
        LoadCurrentClosure => handler::match_op_load_current_closure::<STRIDE>,
        ThrowReferenceErrorIfHole => handler::match_op_throw_reference_error_if_hole::<STRIDE>,
        LoadGlobalNoThrow => handler::match_op_load_global_no_throw::<STRIDE>,
    );
}

/// The `call_enter!` body as a function.
pub fn call_enter<'h>(
    staged: Result<StagedCall, CallFault>,
    pc: usize,
    next_pc: usize,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    match staged {
        Err(e) => raise!(ctx, acc, pc, e),
        Ok(StagedCall::Frame) => {
            set_acc!(acc, roots.undefined.get(ctx.heap()).erase());
            ctx.code_ptr()
        }
        Ok(StagedCall::Native {
            rt,
            callee_reg,
            staged,
        }) => {
            let v = ctx.call_native(rt, callee_reg, staged);
            reenter!(ctx, acc, pc, next_pc, v)
        }
        Ok(StagedCall::Proxy { callee_reg, staged }) => {
            let v = proxy_apply_staged(ctx, roots, regs, callee_reg, staged);
            reenter!(ctx, acc, pc, next_pc, v.get(ctx.heap()))
        }
        Ok(StagedCall::Bound { callee_reg, staged }) => {
            let v = bound_apply_staged(ctx, roots, regs, callee_reg, staged);
            reenter!(ctx, acc, pc, next_pc, v.get(ctx.heap()))
        }
    }
}

macro_rules! call_enter {
    ($ctx:expr, $regs:expr, $acc:expr, $pc:expr, $next_pc:expr, $roots:expr, $staged:expr) => {
        return call_enter($staged, $pc, $next_pc, $regs, $acc, $ctx, $roots)
    };
}

#[inline(always)]
pub fn op_jump_loop<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(JumpLoop, ip, ctx, ops, _next, pc, _next_pc);
    let off = ops.signed::<0>();
    let target_pc = (pc as isize + off as isize) as usize;
    if ctx.safepoint_tick() {
        let state = ctx.state();
        // The accumulator is a raw local: park it in the rooted slot
        // before the poll, which may collect and move its payload.
        state.acc_slot().store(acc!(acc, ctx));
        let shutdown = ctx.heap_mut().safepoint_poll();
        ctx.reload_code();
        set_acc!(acc, state.acc_slot().get(ctx.heap()));
        if shutdown {
            state.set_termination(Termination::Shutdown);
            state.set_pending_exception(roots.undefined.get(ctx.heap()).erase());
            ctx.set_throw_pc(pc);
            return throw_ptr();
        }
        return offset(ctx, target_pc);
    }
    dispatch!(unsafe { ip.offset(off as isize) })
}

#[inline(always)]
pub fn op_call<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(Call, ip, ctx, ops, _next, pc, next_pc);
    let callee = regs.get(ops.signed::<0>(), ctx.heap());
    let staged = call_window_start::<STRIDE>(
        ctx,
        roots,
        pc,
        ops.unsigned::<3>(),
        ops.signed::<0>(),
        callee,
        ops.signed::<1>(),
        ops.unsigned::<2>(),
    );
    call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
}

#[inline(always)]
pub fn op_call_no_feedback<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(CallNoFeedback, ip, ctx, ops, _next, pc, next_pc);
    let callee = regs.get(ops.signed::<0>(), ctx.heap());
    let staged = ctx
        .stack()
        .window(ctx.frame_base(), ops.signed::<1>(), ops.unsigned::<2>());
    let size = base::<STRIDE>() + STRIDE * const { Opcode::CallNoFeedback.operands().len() };
    let result = resolve_target(
        ctx,
        roots,
        pc,
        size,
        ops.signed::<0>(),
        callee.call_target(ctx.heap()),
        staged,
    );
    call_enter!(ctx, regs, acc, pc, next_pc, roots, result)
}
#[inline(always)]
pub fn op_call_function<'h, const STRIDE: usize, const ARGC: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    match ARGC {
        0 => {
            decode!(CallFunction0, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, false>(
                ctx,
                roots,
                pc,
                ops.unsigned::<1>(),
                ops.signed::<0>(),
                callee,
                roots.undefined.get(ctx.heap()).erase(),
                &[],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
        1 => {
            decode!(CallFunction1, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let arg0 = regs.get(ops.signed::<1>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, false>(
                ctx,
                roots,
                pc,
                ops.unsigned::<2>(),
                ops.signed::<0>(),
                callee,
                roots.undefined.get(ctx.heap()).erase(),
                &[arg0][..1],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
        _ => {
            decode!(CallFunction2, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let arg0 = regs.get(ops.signed::<1>(), ctx.heap());
            let arg1 = regs.get(ops.signed::<2>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, false>(
                ctx,
                roots,
                pc,
                ops.unsigned::<3>(),
                ops.signed::<0>(),
                callee,
                roots.undefined.get(ctx.heap()).erase(),
                &[arg0, arg1][..2],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
    }
}

#[inline(always)]
pub fn op_call_method<'h, const STRIDE: usize, const ARGC: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    match ARGC {
        0 => {
            decode!(CallMethod0, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let recv = regs.get(ops.signed::<1>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, true>(
                ctx,
                roots,
                pc,
                ops.unsigned::<2>(),
                ops.signed::<0>(),
                callee,
                recv,
                &[],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
        1 => {
            decode!(CallMethod1, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let recv = regs.get(ops.signed::<1>(), ctx.heap());
            let arg0 = regs.get(ops.signed::<2>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, true>(
                ctx,
                roots,
                pc,
                ops.unsigned::<3>(),
                ops.signed::<0>(),
                callee,
                recv,
                &[arg0][..1],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
        _ => {
            decode!(CallMethod2, ip, ctx, ops, _next, pc, next_pc);
            let callee = regs.get(ops.signed::<0>(), ctx.heap());
            let recv = regs.get(ops.signed::<1>(), ctx.heap());
            let arg0 = regs.get(ops.signed::<2>(), ctx.heap());
            let arg1 = regs.get(ops.signed::<3>(), ctx.heap());
            let staged = call_scatter_start::<STRIDE, true>(
                ctx,
                roots,
                pc,
                ops.unsigned::<4>(),
                ops.signed::<0>(),
                callee,
                recv,
                &[arg0, arg1][..2],
            );
            call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
        }
    }
}

#[inline(always)]
pub fn op_return<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    let _ = ip;
    let _ = STRIDE;
    if ctx.frame_base() == ctx.base_anchor() {
        return core::ptr::null();
    }

    // A frame pushed by `Construct` carries the callee in `new.target`.
    let constructing = {
        let heap = ctx.heap();
        let new_target = ctx
            .stack()
            .frame_header(ctx.frame_base())
            .new_target_slot()
            .get(heap);
        new_target != roots.undefined.get(heap).erase()
    };

    if constructing {
        let heap = ctx.heap();
        let val = acc.get(heap);
        if val.is_primitive(heap) {
            let this_val = regs.get(0, heap);
            if this_val == roots.the_hole.get(heap).erase() {
                // A derived constructor escaped without initializing `this`.
                let caller = ctx.stack().pop_frame(ctx.frame_base());
                ctx.switch_frame(caller.base);
                let _ = ctx.raise(VmError::UninitializedVariable);
                ctx.set_throw_pc(caller.pc);
                return throw_ptr();
            }
            set_acc!(acc, this_val);
        } else {
            set_acc!(acc, val);
        }
    }

    let caller = ctx.stack().pop_frame(ctx.frame_base());
    ctx.switch_frame(caller.base);
    offset(ctx, caller.pc)
}

#[inline(always)]
pub fn step_return<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_return::<STRIDE>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_method0<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_method::<STRIDE, 0>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_method1<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_method::<STRIDE, 1>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_method2<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_method::<STRIDE, 2>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_function0<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_function::<STRIDE, 0>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_function1<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_function::<STRIDE, 1>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_call_function2<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    op_call_function::<STRIDE, 2>(ip, regs, acc, ctx, roots)
}

#[inline(always)]
pub fn step_binary_op<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(BinaryOp, ip, ctx, ops, _next, pc, next_pc);
    let callee_reg = ops.signed::<0>();
    let recv_reg = ops.signed::<1>();
    let arg_reg = ops.signed::<2>();
    let staged = load_operator(
        ctx,
        regs,
        recv_reg,
        ops.unsigned::<3>(),
        ops.unsigned::<4>(),
        callee_reg,
    )
    .and_then(|()| {
        let heap = ctx.heap();
        let callee = regs.get(callee_reg, heap);
        let recv = regs.get(recv_reg, heap);
        let arg = regs.get(arg_reg, heap);
        call_scatter_start_sized::<STRIDE, true, false>(
            ctx,
            roots,
            pc,
            next_pc - pc,
            0,
            callee_reg,
            callee,
            recv,
            &[arg],
        )
    });
    call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
}

#[inline(always)]
pub fn step_unary_op<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(UnaryOp, ip, ctx, ops, _next, pc, next_pc);
    let callee_reg = ops.signed::<0>();
    let recv_reg = ops.signed::<1>();
    let staged = load_operator(
        ctx,
        regs,
        recv_reg,
        ops.unsigned::<2>(),
        ops.unsigned::<3>(),
        callee_reg,
    )
    .and_then(|()| {
        let heap = ctx.heap();
        let callee = regs.get(callee_reg, heap);
        let recv = regs.get(recv_reg, heap);
        call_scatter_start_sized::<STRIDE, true, false>(
            ctx,
            roots,
            pc,
            next_pc - pc,
            0,
            callee_reg,
            callee,
            recv,
            &[],
        )
    });
    call_enter!(ctx, regs, acc, pc, next_pc, roots, staged)
}

#[inline(always)]
pub fn op_construct<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    decode!(Construct, ip, ctx, ops, _next, pc, next_pc);
    let callee_reg = ops.signed::<0>();
    let args_base = ops.signed::<1>();
    let count = ops.unsigned::<2>();
    let size = base::<STRIDE>() + STRIDE * const { Opcode::Construct.operands().len() };

    match construct_start(ctx, pc, size, regs, callee_reg, args_base, count) {
        Ok(ConstructStart::Frame) => {
            set_acc!(acc, roots.undefined.get(ctx.heap()).erase());
            ctx.code_ptr()
        }
        Ok(ConstructStart::Native(rt)) => {
            let v = construct_native(ctx, rt, callee_reg, args_base, count);
            reenter!(ctx, acc, pc, next_pc, v)
        }
        Ok(ConstructStart::Slow) => {
            // The receiver synthesis may have collected; re-derive the
            // instruction pointer before the slow body decodes operands.
            ctx.reload_code();
            let ip = offset(ctx, pc);
            match_slow_construct::<STRIDE>(ip, regs, acc, ctx, roots, f64::NAN)
        }
        Err(e) => raise!(ctx, acc, pc, e),
    }
}
