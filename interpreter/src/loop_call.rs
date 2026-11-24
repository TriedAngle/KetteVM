use crate::RootsArg;
use crate::handler;
use crate::loop_match::{
    DispatchFn, execute_with, offset, op_call, op_call_no_feedback, op_construct, op_jump_loop,
    step_call_function0, step_call_function1, step_call_function2, step_call_method0,
    step_call_method1, step_call_method2, step_return, throw_ptr,
};
use bytecode::Opcode;
use handler_derive::loop_call_table;
use vm_core::{
    Acc, Ctx, EntryFn, ErasedRuntimeState, Handle, HandleSlice, Heap, Interpreter, Object, Regs,
    Result, Tagged, ThreadState, Unwind, VM, Value, interpreter::unwind,
};

type DebugHandler =
    for<'h> fn(*const u8, Regs, &mut Acc, &mut Ctx<'h, 'h>, RootsArg<'h>) -> *const u8;

#[inline(always)]
fn debug_trap<'h, const STRIDE: usize>(
    _ip: *const u8,
    _regs: Regs,
    _acc: &mut Acc,
    _ctx: &mut Ctx<'_, '_>,
    _roots: RootsArg<'_>,
) -> *const u8 {
    unreachable!("invalid opcode")
}

#[inline(always)]
fn step_wide<'h, const STRIDE: usize>(
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    ctx: &mut Ctx<'h, 'h>,
    roots: RootsArg<'h>,
) -> *const u8 {
    let op = unsafe { *ip.add(1) };
    WIDE[op as usize](ip, regs, acc, ctx, roots)
}

loop_call_table!(
    Load => handler::match_op_load,
    Move => handler::match_op_move,
    Store => handler::match_op_store,
    Add => handler::match_op_add,
    CallNative => handler::match_op_call_native,
    Jump => handler::match_op_jump,
    JumpIfTruthy => handler::match_op_jump_if_truthy,
    JumpIfFalsy => handler::match_op_jump_if_falsy,
    JumpIfNotUndefined => handler::match_op_jump_if_not_undefined,
    JumpIfNullish => handler::match_op_jump_if_nullish,
    BindThis => handler::match_op_bind_this,
    JumpLoop => op_jump_loop,
    Return => step_return,
    CallNoFeedback => op_call_no_feedback,
    Call => op_call,
    CallMethod0 => step_call_method0,
    CallMethod1 => step_call_method1,
    CallMethod2 => step_call_method2,
    CallFunction0 => step_call_function0,
    CallFunction1 => step_call_function1,
    CallFunction2 => step_call_function2,
    LoadSmi => handler::match_op_load_smi,
    LoadZero => handler::match_op_load_zero,
    LoadConstant => handler::match_op_load_constant,
    LoadUndefined => handler::match_op_load_undefined,
    LoadNull => handler::match_op_load_null,
    LoadTrue => handler::match_op_load_true,
    LoadFalse => handler::match_op_load_false,
    TailGuard => handler::match_op_tail_guard,
    TestType => handler::match_op_test_type,
    LoadLength => handler::match_op_load_length,
    LoadGlobal => handler::match_op_load_global,
    Sub => handler::match_op_sub,
    Mul => handler::match_op_mul,
    Div => handler::match_op_div,
    Mod => handler::match_op_mod,
    Exp => handler::match_op_exp,
    Negate => handler::match_op_negate,
    AddImmediate => handler::match_op_add_immediate,
    SubImmediate => handler::match_op_sub_immediate,
    MulImmediate => handler::match_op_mul_immediate,
    DivImmediate => handler::match_op_div_immediate,
    ModImmediate => handler::match_op_mod_immediate,
    ExpImmediate => handler::match_op_exp_immediate,
    AddLoc => handler::match_op_add_loc,
    SubLoc => handler::match_op_sub_loc,
    BitwiseOr => handler::match_op_bitwise_or,
    BitwiseXor => handler::match_op_bitwise_xor,
    BitwiseAnd => handler::match_op_bitwise_and,
    ShiftLeft => handler::match_op_shift_left,
    ShiftRight => handler::match_op_shift_right,
    ShiftRightLogical => handler::match_op_shift_right_logical,
    BitwiseOrImmediate => handler::match_op_bitwise_or_immediate,
    BitwiseXorImmediate => handler::match_op_bitwise_xor_immediate,
    BitwiseAndImmediate => handler::match_op_bitwise_and_immediate,
    ShiftLeftImmediate => handler::match_op_shift_left_immediate,
    ShiftRightImmediate => handler::match_op_shift_right_immediate,
    ShiftRightLogicalImmediate => handler::match_op_shift_right_logical_immediate,
    Equal => handler::match_op_equal,
    LessThan => handler::match_op_less_than,
    GreaterThan => handler::match_op_greater_than,
    LessThanOrEqual => handler::match_op_less_than_or_equal,
    GreaterThanOrEqual => handler::match_op_greater_than_or_equal,
    EqualStrict => handler::match_op_equal_strict,
    TestReferenceEqual => handler::match_op_test_reference_equal,
    TestTypeof => handler::match_op_test_typeof,
    InstanceOf => handler::match_op_instance_of,
    CompareJump => handler::match_op_compare_jump,
    SwitchInt => handler::match_op_switch_int,
    ArraySlice => handler::match_op_array_slice,
    IncLoc => handler::match_op_inc_loc,
    DecLoc => handler::match_op_dec_loc,
    LoadKeyedProperty => handler::match_op_load_keyed_property,
    LoadKeyedPropertyOrHole => handler::match_op_load_keyed_property_or_hole,
    LoadIndexed => handler::match_op_load_indexed,
    LoadKeyedPropertyReg => handler::match_op_load_keyed_property_reg,
    LoadElementImm => handler::match_op_load_element_imm,
    LoadNamedProperty => handler::match_op_load_named_property,
    LoadNamedPropertyOrHole => handler::match_op_load_named_property_or_hole,
    StoreNamedProperty => handler::match_op_store_named_property,
    StoreKeyedProperty => handler::match_op_store_keyed_property,
    StoreIndexed => handler::match_op_store_indexed,
    StoreNamedPropertyNoShadow => handler::match_op_store_named_property_no_shadow,
    StoreKeyedPropertyNoShadow => handler::match_op_store_keyed_property_no_shadow,
    StoreGlobal => handler::match_op_store_global,
    CreateClosure => handler::match_op_create_closure,
    Throw => handler::match_op_throw,
    AddParent => handler::match_op_add_parent,
    DefineNamedOwnProperty => handler::match_op_define_named_own_property,
    DefineKeyedOwnProperty => handler::match_op_define_keyed_own_property,
    StoreInArrayLiteral => handler::match_op_store_in_array_literal,
    LoadContextSlot => handler::match_op_load_context_slot,
    StoreContextSlot => handler::match_op_store_context_slot,
    PushContext => handler::match_op_push_context,
    PopContext => handler::match_op_pop_context,
    CreateFunctionContext => handler::match_op_create_function_context,
    CreateEmptyArrayLiteral => handler::match_op_create_empty_array_literal,
    CreateSizedArrayLiteral => handler::match_op_create_sized_array_literal,
    CreateRestParameter => handler::match_op_create_rest_parameter,
    Construct => op_construct,
    CreateEmptyObjectLiteral => handler::match_op_create_empty_object_literal,
    CreateBlockContext => handler::match_op_create_block_context,
    LoadHole => handler::match_op_load_hole,
    LoadNewTarget => handler::match_op_load_new_target,
    LoadContext => handler::match_op_load_context,
    LoadCurrentClosure => handler::match_op_load_current_closure,
    ThrowReferenceErrorIfHole => handler::match_op_throw_reference_error_if_hole,
    LoadGlobalNoThrow => handler::match_op_load_global_no_throw,
    Wide => step_wide,
);

pub struct LoopCallInterpreter;

impl Interpreter for LoopCallInterpreter {
    const EXECUTE: EntryFn = execute;
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
    execute_with(
        vm, runtime, heap, state, callable, args, new_target, dispatch,
    )
}

pub fn dispatch<'h>(ctx: &mut Ctx<'h, 'h>) -> Result<Acc> {
    let mut ip = ctx.code_ptr();
    let mut regs = unsafe { Regs::new(ctx.regs_ptr()) };
    let roots = RootsArg::new(ctx.vm().known());
    let mut acc = Acc::set(roots.undefined.get(ctx.heap()).erase());

    loop {
        let op = unsafe { *ip };
        ip = NARROW[op as usize](ip, regs, &mut acc, ctx, roots);
        regs = unsafe { Regs::new(ctx.regs_ptr()) };

        if ip.is_null() {
            return Ok(acc);
        }
        if ip == throw_ptr() {
            match unwind(ctx, ctx.throw_pc()) {
                Unwind::Caught { pc, ex } => {
                    acc = Acc::set(ex);
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
