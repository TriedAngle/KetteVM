//! Tail-call (nightly, `fast`) backend.

use crate::{FloatReg, Frame, RootsArg};
use bytecode::Opcode;
use handler_derive::{handler, handler_tail_call, tail_call_table};
use vm_core::{
    Acc, CallTarget, Callee, Ctx, EntryFn, ErasedRuntimeState, Errors, FrameMeta, Handle,
    HandleSlice, Heap, Interpreter, NativeIndex, Object, Ops, Regs, Result, Tagged, Termination,
    ThreadState, Thrown, VM, Value, VmError, bail,
};

use crate::handler;
use crate::slow::*;
#[cfg(feature = "star_fusion")]
use crate::star_lookahead;
use crate::utils::*;

/// Instruction Pointer
#[macro_export]
macro_rules! ip {
    ($ip:expr) => {
        $ip
    };
}

/// Insruction Offset
#[macro_export]
macro_rules! pc {
    ($ip:expr, $x:expr) => {{ $ip as usize - $x.code_ptr() as usize }};
}

/// Register Window of the Frame
#[macro_export]
macro_rules! regs {
    ($r:expr) => {
        $r
    };
}

/// Accumulator, must be used before reaching safepoints!
#[macro_export]
macro_rules! acc {
    ($acc:expr, $x:expr) => {{ $acc.get($x.heap()) }};
}

/// Float Register
#[macro_export]
macro_rules! float {
    ($f:expr) => {{ $f.get() }};
}

/// Set the accumulator from a tagged value.
#[macro_export]
macro_rules! set_acc {
    ($acc:ident, $v:expr) => {
        $acc = Acc::set($v)
    };
}

/// Set the accumulator from raw smi bits.
#[macro_export]
macro_rules! set_acc_smi {
    ($acc:ident, $v:expr) => {
        $acc = Acc::from_smi_bits($v)
    };
}

/// Park a float payload in the float register for a slow path.
#[macro_export]
macro_rules! set_float {
    ($f:ident, $v:expr) => {
        $f = FloatReg::new($v)
    };
}

/// Dispatch to the handler for the instruction at `$p`.
#[macro_export]
macro_rules! dispatch {
    ($p:expr, $a:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let h = $t.get(unsafe { *$p });
        become h($p, $r, $a, $x, $t, $k, $f)
    }};
}

/// Advance to next instruction and dispatch.
#[macro_export]
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
#[macro_export]
macro_rules! reenter {
    ($op:ident, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let pc = $pc;
        $ctx.reload_code();
        let fault_ip = unsafe { $ctx.code_ptr().add(pc) };
        if $acc.get($ctx.heap()) == $roots.exception.get($ctx.heap()).erase() {
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
#[macro_export]
macro_rules! tail {
    ($ip:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr, $func:path) => {
        become $func($ip, $regs, $acc, $ctx, $table, $roots, $float)
    };
}

/// Dispatch into the caller after a frame pop. The table is threaded
/// through every exit: one static table is shared by all modes.
#[macro_export]
macro_rules! dispatch_frame {
    ($pc:expr, $a:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let regs = unsafe { Regs::new($x.regs_ptr()) };
        let p = unsafe { $x.code_ptr().add($pc) };
        let h = $t.get(unsafe { *p });
        become h(p, regs, $a, $x, $t, $k, $f)
    }};
}

/// call a callable
#[macro_export]
macro_rules! call_enter {
    ($op:ident, $result:expr, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let (acc, value) = match $result {
            Ok(StagedCall::Frame) => ($acc, false),
            Ok(StagedCall::Native {
                rt,
                callee_reg,
                staged,
            }) => (Acc::set($ctx.call_native(rt, callee_reg, staged)), true),
            Ok(StagedCall::Proxy { callee_reg, staged }) => (
                proxy_apply_staged($ctx, $roots, $regs, callee_reg, staged),
                true,
            ),
            Ok(StagedCall::Bound { callee_reg, staged }) => (
                bound_apply_staged($ctx, $roots, $regs, callee_reg, staged),
                true,
            ),
            Err(fault) => (Acc::set(raise_fault($ctx, fault)), true),
        };
        if value {
            $ctx.reload_code();
            let fault_ip = unsafe { $ctx.code_ptr().add($pc) };
            if acc.get($ctx.heap()) == $roots.exception.get($ctx.heap()).erase() {
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
        let acc = Acc::set($roots.undefined.get($ctx.heap()).erase());
        let entry = $ctx.code_ptr();
        let h = $table.get(unsafe { *entry });
        become h(entry, regs, acc, $ctx, $table, $roots, $float)
    }};
}

#[repr(C)]
pub struct DispatchTable {
    pub narrow: [Handler; 256],
    pub wide: [Handler; 256],
}

pub struct TableArg<'h>(
    pub &'static DispatchTable,
    core::marker::PhantomData<&'h ()>,
);

impl<'h> TableArg<'h> {
    #[inline(always)]
    pub fn new(table: &'static DispatchTable) -> TableArg<'h> {
        TableArg(table, core::marker::PhantomData)
    }

    #[inline(always)]
    pub fn get(&self, op: u8) -> Handler {
        self.0.narrow[op as usize]
    }

    #[inline(always)]
    pub fn wide(&self, op: u8) -> Handler {
        self.0.wide[op as usize]
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

tail_call_table!(
    Wide => op_wide,
    Load => handler::op_load,
    BindThis => handler::op_bind_this,
    Move => handler::op_move,
    Store => handler::op_store,
    Add => handler::op_add,
    CallNative => handler::op_call_native,
    Jump => handler::op_jump,
    JumpIfTruthy => handler::op_jump_if_truthy,
    JumpIfFalsy => handler::op_jump_if_falsy,
    JumpIfNotUndefined => handler::op_jump_if_not_undefined,
    JumpIfNullish => handler::op_jump_if_nullish,
    JumpLoop => op_jump_loop,
    Return => op_return,
    CallNoFeedback => op_call_no_feedback,
    Call => op_call,
    CallMethod0 => op_call_method0,
    CallMethod1 => op_call_method1,
    CallMethod2 => op_call_method2,
    CallFunction0 => op_call_function0,
    CallFunction1 => op_call_function1,
    CallFunction2 => op_call_function2,
    LoadSmi => handler::op_load_smi,
    LoadZero => handler::op_load_zero,
    LoadConstant => handler::op_load_constant,
    LoadUndefined => handler::op_load_undefined,
    LoadNull => handler::op_load_null,
    LoadTrue => handler::op_load_true,
    LoadFalse => handler::op_load_false,
    TailGuard => handler::op_tail_guard,
    TestType => handler::op_test_type,
    LoadLength => handler::op_load_length,
    LoadGlobal => handler::op_load_global,
    Sub => handler::op_sub,
    Mul => handler::op_mul,
    Div => handler::op_div,
    Mod => handler::op_mod,
    Exp => handler::op_exp,
    Negate => handler::op_negate,
    AddImmediate => handler::op_add_immediate,
    SubImmediate => handler::op_sub_immediate,
    MulImmediate => handler::op_mul_immediate,
    DivImmediate => handler::op_div_immediate,
    ModImmediate => handler::op_mod_immediate,
    ExpImmediate => handler::op_exp_immediate,
    AddLoc => handler::op_add_loc,
    SubLoc => handler::op_sub_loc,
    BitwiseOr => handler::op_bitwise_or,
    BitwiseXor => handler::op_bitwise_xor,
    BitwiseAnd => handler::op_bitwise_and,
    ShiftLeft => handler::op_shift_left,
    ShiftRight => handler::op_shift_right,
    ShiftRightLogical => handler::op_shift_right_logical,
    BitwiseOrImmediate => handler::op_bitwise_or_immediate,
    BitwiseXorImmediate => handler::op_bitwise_xor_immediate,
    BitwiseAndImmediate => handler::op_bitwise_and_immediate,
    ShiftLeftImmediate => handler::op_shift_left_immediate,
    ShiftRightImmediate => handler::op_shift_right_immediate,
    ShiftRightLogicalImmediate => handler::op_shift_right_logical_immediate,
    Equal => handler::op_equal,
    LessThan => handler::op_less_than,
    GreaterThan => handler::op_greater_than,
    LessThanOrEqual => handler::op_less_than_or_equal,
    GreaterThanOrEqual => handler::op_greater_than_or_equal,
    EqualStrict => handler::op_equal_strict,
    TestReferenceEqual => handler::op_test_reference_equal,
    TestTypeof => handler::op_test_typeof,
    InstanceOf => handler::op_instance_of,
    CompareJump => handler::op_compare_jump,
    SwitchInt => handler::op_switch_int,
    ArraySlice => handler::op_array_slice,
    IncLoc => handler::op_inc_loc,
    DecLoc => handler::op_dec_loc,
    LoadKeyedProperty => handler::op_load_keyed_property,
    LoadKeyedPropertyOrHole => handler::op_load_keyed_property_or_hole,
    LoadIndexed => handler::op_load_indexed,
    LoadKeyedPropertyReg => handler::op_load_keyed_property_reg,
    LoadElementImm => handler::op_load_element_imm,
    LoadNamedProperty => handler::op_load_named_property,
    LoadNamedPropertyOrHole => handler::op_load_named_property_or_hole,
    StoreNamedProperty => handler::op_store_named_property,
    StoreKeyedProperty => handler::op_store_keyed_property,
    StoreIndexed => handler::op_store_indexed,
    StoreNamedPropertyNoShadow => handler::op_store_named_property_no_shadow,
    StoreKeyedPropertyNoShadow => handler::op_store_keyed_property_no_shadow,
    DefineNamedOwnProperty => handler::op_define_named_own_property,
    DefineKeyedOwnProperty => handler::op_define_keyed_own_property,
    StoreInArrayLiteral => handler::op_store_in_array_literal,
    StoreGlobal => handler::op_store_global,
    CreateClosure => handler::op_create_closure,
    Throw => handler::op_throw,
    AddParent => handler::op_add_parent,
    LoadContextSlot => handler::op_load_context_slot,
    StoreContextSlot => handler::op_store_context_slot,
    PushContext => handler::op_push_context,
    PopContext => handler::op_pop_context,
    CreateFunctionContext => handler::op_create_function_context,
    CreateEmptyArrayLiteral => handler::op_create_empty_array_literal,
    CreateSizedArrayLiteral => handler::op_create_sized_array_literal,
    CreateRestParameter => handler::op_create_rest_parameter,
    Construct => op_construct,
    CreateEmptyObjectLiteral => handler::op_create_empty_object_literal,
    CreateBlockContext => handler::op_create_block_context,
    LoadHole => handler::op_load_hole,
    LoadNewTarget => handler::op_load_new_target,
    LoadContext => handler::op_load_context,
    LoadCurrentClosure => handler::op_load_current_closure,
    ThrowReferenceErrorIfHole => handler::op_throw_reference_error_if_hole,
    LoadGlobalNoThrow => handler::op_load_global_no_throw,
);

pub static TABLE: DispatchTable = DispatchTable {
    narrow: __tail_call_narrow(),
    wide: __tail_call_wide(),
};

// --- entry ---

fn enter<'a, 't>(
    vm: &'a VM,
    runtime: &'t dyn ErasedRuntimeState,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'a, Value>>,
) -> Result<Acc> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => bail!(vm, heap, state, VmError::NotCallable),
        // the proxy dispatch above already intercepted these
        // proxy and bound-function calls are intercepted before `execute`
        Some(CallTarget::Proxy(_)) | Some(CallTarget::Bound { .. }) => {
            bail!(vm, heap, state, VmError::NotCallable)
        }
        Some(CallTarget::Native(rt)) => Ok(Acc::set(call_native_target(
            vm,
            runtime,
            heap,
            state,
            rt,
            callable.erase(),
            args,
            new_target,
        ))),
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
            let ip = ctx.code_ptr();
            let regs = unsafe { Regs::new(ctx.regs_ptr()) };
            let table = TableArg::new(&TABLE);
            let roots = RootsArg::new(vm.known());

            let acc = Acc::set(roots.undefined.get(ctx.heap()).erase());
            let acc = table.get(unsafe { *ip })(
                ip,
                regs,
                acc,
                &mut ctx,
                table,
                roots,
                FloatReg::new(f64::NAN),
            );
            Ok(acc)
        }
    }
}

pub fn execute<'a, 't>(
    vm: &VM,
    runtime: &'t dyn ErasedRuntimeState,
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
        let acc = enter(vm, runtime, heap, state, callable, args, new_target)?;
        let rooted = scope.handle(acc.get(heap));
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

pub struct TailCallInterpreter;

impl Interpreter for TailCallInterpreter {
    const EXECUTE: EntryFn = execute;
}
/// `Wide` prefix comes back through here. The table argument is
/// narrow for the whole execution; only the handler stride varies.
#[handler]
fn op_wide(frame: Frame) {
    let h = table.wide(unsafe { *ip.add(1) });
    become h(ip, regs, acc, ctx, table, roots, float)
}

#[handler]
fn op_trap(frame: Frame) {
    let byte = unsafe { *frame.ip() };
    unreachable!("unimplemented opcode {:?}", Opcode::from_byte(byte))
}

#[rustc_align(32)]
pub extern "rust-preserve-none" fn throw_dispatch<'h, const STRIDE: usize>(
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
            return Acc::set(roots.exception.get(ctx.heap()).erase());
        }
        let caller = ctx.stack().pop_frame(base);
        ctx.switch_frame(caller.base);
        pc = caller.handler_pc;
    }
}

#[handler_tail_call(JumpLoop)]
fn op_jump_loop(frame: Frame) {
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
            state.set_termination(Termination::Shutdown);
            state.set_pending_exception(roots.undefined.get(frame.ctx().heap()));
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

#[handler_tail_call(Return)]
fn op_return(frame: Frame) {
    frame.opcode_check();
    if frame.ctx().frame_base() == frame.ctx().base_anchor() {
        return acc;
    }
    // A frame pushed by `Construct` carries the callee in `new.target`
    let constructing = {
        let heap = frame.ctx().heap();
        let new_target = frame
            .ctx()
            .stack()
            .frame_header(frame.ctx().frame_base())
            .new_target_slot()
            .get(heap);
        new_target != roots.undefined.get(heap).erase()
    };

    if constructing {
        let heap = frame.ctx().heap();
        let val = frame.acc();
        if val.is_primitive(heap) {
            let this_val = frame.regs().get(0, heap);
            if this_val == roots.the_hole.get(heap).erase() {
                // a derived constructor escaped without initializing `this`
                let caller = frame.ctx().stack().pop_frame(frame.ctx().frame_base());
                frame.ctx().switch_frame(caller.base);
                let _ = frame.ctx().raise(VmError::UninitializedVariable);
                let fault_ip = unsafe { frame.ctx().code_ptr().add(caller.pc) };
                become throw_dispatch::<STRIDE>(
                    fault_ip,
                    unsafe { Regs::new(core::ptr::null_mut()) },
                    acc,
                    frame.ctx(),
                    table,
                    roots,
                    float,
                )
            }
            frame.set_acc(this_val);
        } else {
            frame.set_acc(val);
        }
    }
    let caller = frame.ctx().stack().pop_frame(frame.ctx().frame_base());
    frame.ctx().switch_frame(caller.base);
    dispatch_frame!(caller.pc)
}

// --- calls ---

#[handler_tail_call(Call)]
fn op_call(frame: Frame) {
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

#[handler_tail_call(CallNoFeedback)]
fn op_call_no_feedback(frame: Frame) {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let size = (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CallNoFeedback.operands().len() };
    let staged = frame.ctx().stack().window(
        frame.ctx().frame_base(),
        frame.signed::<1>(),
        frame.unsigned::<2>(),
    );
    let result = resolve_target(
        frame.ctx_ref(),
        roots,
        pc,
        size,
        frame.signed::<0>(),
        callee.call_target(frame.ctx().heap()),
        staged,
    );
    call_enter!(result, pc)
}

#[handler_tail_call(CallMethod0)]
fn op_call_method0(frame: Frame) {
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

#[handler_tail_call(CallMethod1)]
fn op_call_method1(frame: Frame) {
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

#[handler_tail_call(CallMethod2)]
fn op_call_method2(frame: Frame) {
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

#[handler_tail_call(CallFunction0)]
fn op_call_function0(frame: Frame) {
    let pc = frame.pc();
    let callee = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let result = call_scatter_start::<STRIDE, false>(
        frame.ctx_ref(),
        roots,
        pc,
        frame.unsigned::<1>(),
        frame.signed::<0>(),
        callee,
        roots.undefined.get(frame.ctx().heap()).erase(),
        &[],
    );
    call_enter!(result, pc)
}

#[handler_tail_call(CallFunction1)]
fn op_call_function1(frame: Frame) {
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
        roots.undefined.get(frame.ctx().heap()).erase(),
        &[arg0][..1],
    );
    call_enter!(result, pc)
}

#[handler_tail_call(CallFunction2)]
fn op_call_function2(frame: Frame) {
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
        roots.undefined.get(frame.ctx().heap()).erase(),
        &[arg0, arg1][..2],
    );
    call_enter!(result, pc)
}

#[handler_tail_call(Construct)]
fn op_construct(frame: Frame) {
    let pc = frame.pc();
    let size =
        (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::Construct.operands().len() };
    let callee_reg = frame.signed::<0>();
    let args_base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    match construct_start(
        frame.ctx(),
        pc,
        size,
        frame.regs(),
        callee_reg,
        args_base,
        count,
    ) {
        Ok(ConstructStart::Frame) => {
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            let acc = Acc::set(roots.undefined.get(frame.ctx().heap()).erase());
            let entry = frame.ctx().code_ptr();
            let h = table.get(unsafe { *entry });
            become h(entry, regs, acc, frame.ctx(), table, roots, float)
        }
        Ok(ConstructStart::Native(rt)) => {
            let val = construct_native(frame.ctx(), rt, callee_reg, args_base, count);
            frame.set_acc(val);
            reenter!(pc)
        }
        Ok(ConstructStart::Slow) => tail!(slow_construct),
        Err(fault) => {
            frame.set_acc(raise_fault(frame.ctx(), fault));
            tail!(throw_dispatch)
        }
    }
}
