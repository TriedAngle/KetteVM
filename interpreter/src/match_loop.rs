//! Sketch of the next match-loop interpreter: one stride-agnostic loop,
//! `step::<1>` for narrow instructions and `step::<2>` for wide ones.
//! Flow-free: hot state stays in locals; only `Ctx` owns interpreter state.

use bytecode::Opcode;

use vm_core::interpreter::{base, unwind};

use crate::RootsArg;
use crate::utils::{
    ConstructStart, StagedCall, bound_apply_staged, call_scatter_start, call_window_start,
    construct_native, construct_start, proxy_apply_staged, resolve_target,
};
use vm_core::{
    Acc, CallTarget, Callee, Ctx, EntryFn, FrameMeta, Handle, HandleSlice, Heap, HostCtx,
    Interpreter, Object, Ops, Regs, Tagged, Termination, ThreadState, Unwind, VM, Value, VmError,
};

pub struct MatchInterpreter;

impl Interpreter for MatchInterpreter {
    const EXECUTE: EntryFn = execute;
}

#[inline(always)]
pub fn offset(code: *const u8, pc: usize) -> *const u8 {
    unsafe { code.add(pc) }
}

/// Successor sentinel meaning "this instruction threw"
#[inline(always)]
pub fn throw_ptr() -> *const u8 {
    usize::MAX as *const u8
}

/// Decode the current instruction into caller-named locals
macro_rules! decode {
    ($op:ident, $ip:expr, $ops:ident, $next:ident) => {
        let $ops = Ops::<STRIDE>::new($ip, Opcode::$op);
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
            offset($ctx.code_ptr(), $next_pc)
        }
    }};
}

/// Raise a `VmError`, re-deriving the code pointer (materializing the
/// exception may collect), and re-enter at the faulting instruction.
macro_rules! raise {
    ($ctx:expr, $acc:expr, $pc:expr, $err:expr) => {{
        *$acc = Acc::set($ctx.raise_tag($err));
        $ctx.reload_code();
        $ctx.set_throw_pc($pc);
        throw_ptr()
    }};
}

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
        let result = start(vm, heap, state, callable, args, new_target)?;
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

fn start<'b>(
    vm: &'b VM,
    heap: &'b mut Heap,
    state: &'b ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'b, Value>>,
) -> Result<Tagged<'b, Value>, VmError> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => Err(VmError::Type),
        // proxy and bound-function calls are intercepted before `execute`
        Some(CallTarget::Proxy(_)) | Some(CallTarget::Bound { .. }) => Err(VmError::Type),
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
                None => vm.known().undefined.get(heap).erase(),
            };
            let frame = stack.push_frame_values(
                heap,
                FrameMeta::ROOT,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                new_target_value,
                args.as_tagged(),
            )?;
            state.set_frame_base(frame.base);
            state.set_frame_active(true);

            let mut ctx = Ctx::new(vm, state, heap, frame.base);
            let value = dispatch(&mut ctx)?;
            // Safety: `value` came from the rooted accumulator slot and `heap`
            // is still borrowed here.
            Ok(unsafe { Tagged::from_value_unchecked(value) })
        }
    }
}

fn dispatch<'h>(ctx: &mut Ctx<'h, 'h>) -> Result<Value, VmError> {
    let mut ip = ctx.code_ptr();
    let mut regs = unsafe { Regs::new(ctx.regs_ptr()) };
    let roots = RootsArg::new(ctx.vm().known());
    let mut acc = Acc::set(roots.undefined.get(ctx.heap()).erase());
    let mut frame_base = ctx.frame_base();

    loop {
        step::<1>(ctx, &mut ip, &mut regs, &mut acc, &mut frame_base, roots);

        // A null successor is become's `return acc`: the execution is done.
        if ip.is_null() {
            return Ok(acc.get(ctx.heap()).raw());
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
                    ip = offset(ctx.code_ptr(), pc);
                }
                Unwind::Escaped => {
                    return Ok(roots.exception.get(ctx.heap()).erase().raw());
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
        Opcode::Load => crate::match_op_load::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Move => crate::match_op_move::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Store => crate::match_op_store::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Add => crate::match_op_add::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CallRuntime => {
            let next = crate::match_op_call_runtime::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::Jump => crate::match_op_jump::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpIfTruthy => {
            crate::match_op_jump_if_truthy::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::JumpIfFalsy => crate::match_op_jump_if_falsy::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::JumpIfNotUndefined => {
            crate::match_op_jump_if_not_undefined::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
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
        Opcode::LoadSmi => crate::match_op_load_smi::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadZero => crate::match_op_load_zero::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadConstant => {
            crate::match_op_load_constant::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadUndefined => {
            crate::match_op_load_undefined::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadNull => crate::match_op_load_null::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadTrue => crate::match_op_load_true::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadFalse => crate::match_op_load_false::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadGlobal => crate::match_op_load_global::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Sub => crate::match_op_sub::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Mul => crate::match_op_mul::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Div => crate::match_op_div::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Mod => crate::match_op_mod::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Exp => crate::match_op_exp::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Negate => crate::match_op_negate::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::AddImmediate => {
            crate::match_op_add_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::SubImmediate => {
            crate::match_op_sub_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::MulImmediate => {
            crate::match_op_mul_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::DivImmediate => {
            crate::match_op_div_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ModImmediate => {
            crate::match_op_mod_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ExpImmediate => {
            crate::match_op_exp_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::AddLoc => crate::match_op_add_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::SubLoc => crate::match_op_sub_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseOr => crate::match_op_bitwise_or::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseXor => crate::match_op_bitwise_xor::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::BitwiseAnd => crate::match_op_bitwise_and::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftLeft => crate::match_op_shift_left::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftRight => crate::match_op_shift_right::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::ShiftRightLogical => {
            crate::match_op_shift_right_logical::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseOrImmediate => {
            crate::match_op_bitwise_or_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseXorImmediate => {
            crate::match_op_bitwise_xor_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::BitwiseAndImmediate => {
            crate::match_op_bitwise_and_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftLeftImmediate => {
            crate::match_op_shift_left_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftRightImmediate => {
            crate::match_op_shift_right_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ShiftRightLogicalImmediate => {
            crate::match_op_shift_right_logical_immediate::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Equal => crate::match_op_equal::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LessThan => crate::match_op_less_than::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::GreaterThan => crate::match_op_greater_than::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LessThanOrEqual => {
            crate::match_op_less_than_or_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::GreaterThanOrEqual => {
            crate::match_op_greater_than_or_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::EqualStrict => crate::match_op_equal_strict::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::TestReferenceEqual => {
            crate::match_op_test_reference_equal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::TestTypeof => crate::match_op_test_typeof::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::InstanceOf => crate::match_op_instance_of::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CompareJump => crate::match_op_compare_jump::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::IncLoc => crate::match_op_inc_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::DecLoc => crate::match_op_dec_loc::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadKeyedProperty => {
            crate::match_op_load_keyed_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadKeyedPropertyReg => {
            crate::match_op_load_keyed_property_reg::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadElementImm => {
            crate::match_op_load_element_imm::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadNamedProperty => {
            crate::match_op_load_named_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreNamedProperty => {
            crate::match_op_store_named_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreKeyedProperty => {
            crate::match_op_store_keyed_property::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreNamedPropertyNoShadow => {
            crate::match_op_store_named_property_no_shadow::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreKeyedPropertyNoShadow => {
            crate::match_op_store_keyed_property_no_shadow::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreGlobal => crate::match_op_store_global::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::Delete => crate::match_op_delete::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CreateClosure => {
            crate::match_op_create_closure::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Throw => crate::match_op_throw::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::AddParent => crate::match_op_add_parent::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadContextSlot => {
            crate::match_op_load_context_slot::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::StoreContextSlot => {
            crate::match_op_store_context_slot::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::PushContext => crate::match_op_push_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::PopContext => crate::match_op_pop_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::CreateFunctionContext => {
            crate::match_op_create_function_context::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateEmptyArrayLiteral => {
            crate::match_op_create_empty_array_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateSizedArrayLiteral => {
            crate::match_op_create_sized_array_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::Construct => {
            let next = op_construct::<STRIDE>(*ip, *regs, acc, ctx, roots);
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *frame_base = ctx.frame_base();
            next
        }
        Opcode::CreateEmptyObjectLiteral => {
            crate::match_op_create_empty_object_literal::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::CreateBlockContext => {
            crate::match_op_create_block_context::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadHole => crate::match_op_load_hole::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadNewTarget => {
            crate::match_op_load_new_target::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadContext => crate::match_op_load_context::<STRIDE>(*ip, *regs, acc, ctx, roots),
        Opcode::LoadCurrentClosure => {
            crate::match_op_load_current_closure::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::ThrowReferenceErrorIfHole => {
            crate::match_op_throw_reference_error_if_hole::<STRIDE>(*ip, *regs, acc, ctx, roots)
        }
        Opcode::LoadGlobalNoThrow => {
            crate::match_op_load_global_no_throw::<STRIDE>(*ip, *regs, acc, ctx, roots)
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
                let v = $ctx.call_runtime(rt, staged);
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
        return offset(ctx.code_ptr(), target_pc);
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
                let _ = ctx.raise_tag(VmError::Reference);
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
    offset(ctx.code_ptr(), caller.pc)
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
        Ok(ConstructStart::Threw) => {
            ctx.set_throw_pc(pc);
            throw_ptr()
        }
        Ok(ConstructStart::Slow) => {
            // The receiver synthesis may have collected; re-derive the
            // instruction pointer before the slow body decodes operands.
            ctx.reload_code();
            let ip = offset(ctx.code_ptr(), pc);
            crate::slow::match_slow_construct::<STRIDE>(ip, regs, acc, ctx, roots, f64::NAN)
        }
        Err(e) => raise!(ctx, acc, pc, e),
    }
}
