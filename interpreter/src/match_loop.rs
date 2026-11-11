//! Sketch of the next match-loop interpreter: one stride-agnostic loop,
//! `step::<1>` for narrow instructions and `step::<2>` for wide ones.
//! Flow-free: hot state stays in locals; only `Ctx` owns interpreter state.

use crate::{
    RootsArg, match_op_add, match_op_add_immediate, match_op_add_loc, match_op_add_parent,
    match_op_bind_this, match_op_bitwise_and, match_op_bitwise_and_immediate, match_op_bitwise_or,
    match_op_bitwise_or_immediate, match_op_bitwise_xor, match_op_bitwise_xor_immediate,
    match_op_call_native, match_op_compare_jump, match_op_create_block_context,
    match_op_create_closure, match_op_create_empty_array_literal,
    match_op_create_empty_object_literal, match_op_create_function_context,
    match_op_create_sized_array_literal, match_op_dec_loc, match_op_div, match_op_div_immediate,
    match_op_equal, match_op_equal_strict, match_op_exp, match_op_exp_immediate,
    match_op_greater_than, match_op_greater_than_or_equal, match_op_inc_loc, match_op_instance_of,
    match_op_jump, match_op_jump_if_falsy, match_op_jump_if_not_undefined,
    match_op_jump_if_nullish, match_op_jump_if_truthy, match_op_less_than,
    match_op_less_than_or_equal, match_op_load, match_op_load_constant, match_op_load_context,
    match_op_load_context_slot, match_op_load_current_closure, match_op_load_element_imm,
    match_op_load_false, match_op_load_global, match_op_load_global_no_throw, match_op_load_hole,
    match_op_load_keyed_property, match_op_load_keyed_property_reg, match_op_load_named_property,
    match_op_load_new_target, match_op_load_null, match_op_load_smi, match_op_load_true,
    match_op_load_undefined, match_op_load_zero, match_op_mod, match_op_mod_immediate,
    match_op_move, match_op_mul, match_op_mul_immediate, match_op_negate, match_op_pop_context,
    match_op_push_context, match_op_shift_left, match_op_shift_left_immediate,
    match_op_shift_right, match_op_shift_right_immediate, match_op_shift_right_logical,
    match_op_shift_right_logical_immediate, match_op_store, match_op_store_context_slot,
    match_op_store_global, match_op_store_keyed_property, match_op_store_keyed_property_no_shadow,
    match_op_store_named_property, match_op_store_named_property_no_shadow, match_op_sub,
    match_op_sub_immediate, match_op_sub_loc, match_op_test_reference_equal, match_op_test_typeof,
    match_op_throw, match_op_throw_reference_error_if_hole, slow::match_slow_construct,
    utils::CallFault, utils::ConstructStart, utils::StagedCall, utils::bound_apply_staged,
    utils::call_scatter_start, utils::call_window_start, utils::construct_native,
    utils::construct_start, utils::proxy_apply_staged, utils::raise_fault, utils::resolve_target,
};
use bytecode::Opcode;
use vm_core::{
    Acc, CallTarget, Callee, Ctx, EntryFn, ErasedRuntimeState, Errors, FrameMeta, Handle,
    HandleSlice, Heap, HostCtx, Interpreter, Object, Ops, Regs, Result, Tagged, Termination,
    ThreadState, Thrown, Unwind, VM, Value, VmError, bail, interpreter::base, interpreter::unwind,
    raise,
};

pub struct MatchInterpreter;

impl Interpreter for MatchInterpreter {
    const EXECUTE: EntryFn = execute;
}

#[inline(always)]
pub fn offset(ctx: &Ctx<'_, '_>, pc: usize) -> *const u8 {
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

pub fn execute<'a>(
    vm: &VM,
    runtime: &dyn ErasedRuntimeState,
    heap: &'a mut Heap,
    state: &ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'_, Value>>,
) -> Result<Tagged<'a, Value>> {
    if state.rust_stack_exhausted() {
        bail!(vm, heap, state, VmError::StackOverflow);
    }
    let stack = state.stack();

    let saved_top = stack.top();
    let was_active = state.is_frame_active();
    let outer = was_active.then(|| state.frame_base());

    state.handle_scope(|scope| {
        let result = start(vm, runtime, heap, state, callable, args, new_target)?;
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

fn start<'b, 't>(
    vm: &'b VM,
    runtime: &'t dyn ErasedRuntimeState,
    heap: &'b mut Heap,
    state: &'b ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'b, Value>>,
) -> Result<Tagged<'b, Value>> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => bail!(vm, heap, state, VmError::NotCallable),
        // proxy and bound-function calls are intercepted before `execute`
        Some(CallTarget::Proxy(_)) | Some(CallTarget::Bound { .. }) => {
            bail!(vm, heap, state, VmError::NotCallable)
        }
        Some(CallTarget::Native(rt)) => {
            let f = vm.native(rt);
            // Safety: this is safe
            let runtime: &'b dyn ErasedRuntimeState = unsafe { core::mem::transmute(runtime) };
            let nctx = HostCtx::new(vm, runtime, heap, state);
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

fn dispatch<'h>(ctx: &mut Ctx<'h, 'h>) -> Result<Acc> {
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

#[inline(always)]
fn step<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: &mut *const u8,
    regs: &mut Regs,
    acc: &mut Acc,
    frame_base: &mut usize,
    roots: RootsArg<'h>,
) {
    let op = unsafe { Opcode::from_byte_unchecked(*(*ip).add(if STRIDE == 2 { 1 } else { 0 })) };

    *ip = match op {
        Opcode::Load => match_op_load::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Move => match_op_move::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Store => match_op_store::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Add => match_op_add::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CallNative => {
            let next = match_op_call_native::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::Jump => match_op_jump::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpIfTruthy => match_op_jump_if_truthy::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpIfFalsy => match_op_jump_if_falsy::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpIfNotUndefined => {
            match_op_jump_if_not_undefined::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::JumpIfNullish => match_op_jump_if_nullish::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BindThis => match_op_bind_this::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpLoop => op_jump_loop::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Return => {
            let next = op_return(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallNoFeedback => {
            let next = op_call_no_feedback::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::Call => {
            let next = op_call::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallMethod0 => {
            let next = op_call_method::<STRIDE, 0>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallMethod1 => {
            let next = op_call_method::<STRIDE, 1>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallMethod2 => {
            let next = op_call_method::<STRIDE, 2>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallFunction0 => {
            let next = op_call_function::<STRIDE, 0>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallFunction1 => {
            let next = op_call_function::<STRIDE, 1>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CallFunction2 => {
            let next = op_call_function::<STRIDE, 2>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::LoadSmi => match_op_load_smi::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadZero => match_op_load_zero::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadConstant => match_op_load_constant::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadUndefined => match_op_load_undefined::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadNull => match_op_load_null::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadTrue => match_op_load_true::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadFalse => match_op_load_false::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadGlobal => match_op_load_global::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Sub => match_op_sub::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Mul => match_op_mul::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Div => match_op_div::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Mod => match_op_mod::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Exp => match_op_exp::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Negate => match_op_negate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::AddImmediate => match_op_add_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::SubImmediate => match_op_sub_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::MulImmediate => match_op_mul_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::DivImmediate => match_op_div_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ModImmediate => match_op_mod_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ExpImmediate => match_op_exp_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::AddLoc => match_op_add_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::SubLoc => match_op_sub_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseOr => match_op_bitwise_or::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseXor => match_op_bitwise_xor::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseAnd => match_op_bitwise_and::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftLeft => match_op_shift_left::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftRight => match_op_shift_right::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftRightLogical => {
            match_op_shift_right_logical::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseOrImmediate => {
            match_op_bitwise_or_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseXorImmediate => {
            match_op_bitwise_xor_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseAndImmediate => {
            match_op_bitwise_and_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftLeftImmediate => {
            match_op_shift_left_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftRightImmediate => {
            match_op_shift_right_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftRightLogicalImmediate => {
            match_op_shift_right_logical_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Equal => match_op_equal::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LessThan => match_op_less_than::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::GreaterThan => match_op_greater_than::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LessThanOrEqual => {
            match_op_less_than_or_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::GreaterThanOrEqual => {
            match_op_greater_than_or_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::EqualStrict => match_op_equal_strict::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::TestReferenceEqual => {
            match_op_test_reference_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::TestTypeof => match_op_test_typeof::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::InstanceOf => match_op_instance_of::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CompareJump => match_op_compare_jump::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::IncLoc => match_op_inc_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::DecLoc => match_op_dec_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadKeyedProperty => {
            match_op_load_keyed_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadKeyedPropertyReg => {
            match_op_load_keyed_property_reg::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadElementImm => match_op_load_element_imm::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadNamedProperty => {
            match_op_load_named_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreNamedProperty => {
            match_op_store_named_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreKeyedProperty => {
            match_op_store_keyed_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreNamedPropertyNoShadow => {
            match_op_store_named_property_no_shadow::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreKeyedPropertyNoShadow => {
            match_op_store_keyed_property_no_shadow::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreGlobal => match_op_store_global::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CreateClosure => match_op_create_closure::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Throw => match_op_throw::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::AddParent => match_op_add_parent::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadContextSlot => {
            match_op_load_context_slot::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreContextSlot => {
            match_op_store_context_slot::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::PushContext => match_op_push_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::PopContext => match_op_pop_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CreateFunctionContext => {
            match_op_create_function_context::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateEmptyArrayLiteral => {
            match_op_create_empty_array_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateSizedArrayLiteral => {
            match_op_create_sized_array_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Construct => {
            let next = op_construct::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CreateEmptyObjectLiteral => {
            match_op_create_empty_object_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateBlockContext => {
            match_op_create_block_context::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadHole => match_op_load_hole::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadNewTarget => match_op_load_new_target::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadContext => match_op_load_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadCurrentClosure => {
            match_op_load_current_closure::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ThrowReferenceErrorIfHole => {
            match_op_throw_reference_error_if_hole::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadGlobalNoThrow => {
            match_op_load_global_no_throw::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Wide => {
            step::<2>(ctx, ip, regs, acc, frame_base, roots);
            *ip
        }
    };
}

macro_rules! call_enter {
    (
        $ctx:expr,
        $regs:expr,
        $acc:expr,
        $pc:expr,
        $next_pc:expr,
        $roots:expr,
        $staged:expr
    ) => {{
        match $staged {
            Err(e) => raise!($ctx, $acc, $pc, e),
            Ok(StagedCall::Value(v)) => reenter!($ctx, $acc, $pc, $next_pc, v),
            Ok(StagedCall::Frame) => {
                *$acc = Acc::set($roots.undefined.get($ctx.heap()).erase());
                $ctx.code_ptr()
            }
            Ok(StagedCall::Native { rt, staged }) => {
                let v = $ctx.call_native(rt, staged);
                reenter!($ctx, $acc, $pc, $next_pc, v)
            }
            Ok(StagedCall::Proxy { callee_reg, staged }) => {
                let v = proxy_apply_staged($ctx, $roots, $regs, callee_reg, staged);
                reenter!($ctx, $acc, $pc, $next_pc, v.get($ctx.heap()))
            }
            Ok(StagedCall::Bound { callee_reg, staged }) => {
                let v = bound_apply_staged($ctx, $roots, $regs, callee_reg, staged);
                reenter!($ctx, $acc, $pc, $next_pc, v.get($ctx.heap()))
            }
        }
    }};
}

#[inline(always)]
fn op_jump_loop<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
fn op_call<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
fn op_call_no_feedback<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
fn op_call_function<'h, const STRIDE: usize, const ARGC: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
fn op_call_method<'h, const STRIDE: usize, const ARGC: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
fn op_return<'h>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    let _ = ip;
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
fn op_construct<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
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
