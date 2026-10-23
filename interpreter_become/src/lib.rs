#![feature(explicit_tail_calls)]
#![feature(rust_preserve_none_cc)]
#![allow(incomplete_features)]
#![feature(fn_align)]
#![allow(unused_macros, unused_unsafe, unused_variables)]
#![allow(dead_code)]

use core::hint::black_box;
use std::marker::PhantomData;

use handler_derive::{handler, handler_table};

use bytecode::Opcode;
use vm_core::bootstrap::WellKnown;
use vm_core::ic::{ElementHit, Hit, InlineCache, MonoProbe};
use vm_core::{
    Acc, CallTarget, CallableInfoObject, Callee, Compare, ContextInit, ContextObject, Convert, Ctx,
    EntryFn, FixedArray, FrameMeta, Handle, HandleSlice, Heap, HostCtx, Interpreter, NativeIndex,
    Object, Ops, Regs, ScopeInfo, Smi, Tagged, ThreadState, VM, Value, VmError, raise_runtime,
};

pub mod register_probe;
mod macros;
mod slow;
mod utils;

use crate::macros::*;
use crate::slow::*;
use crate::utils::*;

/// A pinned FP-register argument. Rides the float register pool (d0
/// under preserve-none), so it costs no integer argument registers;
/// forwarding it through `become` is free. Numeric slow paths hand
/// their payload through it instead of parking it in a Ctx cell.
#[derive(Clone, Copy)]
pub struct FloatReg(f64);

impl FloatReg {
    #[inline(always)]
    pub fn new(v: f64) -> FloatReg {
        FloatReg(v)
    }

    #[inline(always)]
    pub fn get(self) -> f64 {
        black_box(self.0)
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[derive(Clone, Copy)]
pub struct RootsArg<'a>(PhantomData<&'a ()>);

#[cfg(not(target_arch = "aarch64"))]
impl<'a> RootsArg<'a> {
    #[inline(always)]
    pub fn new() -> RootsArg<'a> {
        RootsArg(PhantomData)
    }

    #[inline(always)]
    pub fn from_heap(_heap: &Heap) -> RootsArg<'a> {
        RootsArg(PhantomData)
    }

    #[inline(always)]
    pub fn known<'h>(&self, heap: &'h Heap) -> &'static WellKnown {
        heap.known()
    }
}

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
pub struct RootsArg<'a>(&'static WellKnown, PhantomData<&'a ()>);

#[cfg(target_arch = "aarch64")]
impl<'a> RootsArg<'a> {
    #[inline(always)]
    pub fn new(known: &'static WellKnown) -> RootsArg<'a> {
        RootsArg(known, PhantomData)
    }

    #[inline(always)]
    pub fn from_heap(heap: &Heap) -> RootsArg<'a> {
        RootsArg(heap.known(), PhantomData)
    }

    #[inline(always)]
    pub fn known<'h>(&self, _heap: &'h Heap) -> &'static WellKnown {
        self.0
    }
}

#[cfg(feature = "star_fusion")]
const fn star_lookahead(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::Load
            | Opcode::LoadSmi
            | Opcode::LoadConstant
            | Opcode::LoadZero
            | Opcode::LoadUndefined
            | Opcode::LoadNull
            | Opcode::LoadTrue
            | Opcode::LoadFalse
            | Opcode::LoadHole
            | Opcode::LoadGlobal
            | Opcode::LoadNamedProperty
            | Opcode::LoadKeyedProperty
            | Opcode::LoadElementImm
            | Opcode::LoadNewTarget
            | Opcode::LoadContextSlot
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Mod
            | Opcode::Exp
            | Opcode::BitwiseOr
            | Opcode::BitwiseXor
            | Opcode::BitwiseAnd
            | Opcode::ShiftLeft
            | Opcode::ShiftRight
            | Opcode::ShiftRightLogical
            | Opcode::AddImmediate
            | Opcode::SubImmediate
            | Opcode::MulImmediate
            | Opcode::DivImmediate
            | Opcode::ModImmediate
            | Opcode::ExpImmediate
            | Opcode::BitwiseOrImmediate
            | Opcode::BitwiseXorImmediate
            | Opcode::BitwiseAndImmediate
            | Opcode::ShiftLeftImmediate
            | Opcode::ShiftRightImmediate
            | Opcode::ShiftRightLogicalImmediate
            | Opcode::AddLoc
            | Opcode::SubLoc
    )
}

/// Placeholder receiver type for `#[handler]` fns.
/// The proc macro rewrites access to it.
#[doc(hidden)]
pub struct Frame;

#[repr(C)]
pub struct DispatchTable {
    pub narrow: [Handler; 256],
    pub wide: [Handler; 256],
}

pub struct TableArg<'h>(pub &'static DispatchTable, core::marker::PhantomData<&'h ()>);

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

// --- trampolines ---

/// Peel the `Wide` prefix and dispatch the stride-2 handler
/// instantiation, keeping the NARROW table: the next instruction
/// (whatever its width) dispatches narrow-first, and a following
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

// --- loads ---

#[handler(Load)]
fn op_load(frame: Frame) {
    let r = frame.signed::<0>();
    frame.set_acc(frame.regs().get(r, frame.ctx().heap()));
    next!()
}

#[handler(Move)]
fn op_move(frame: Frame) {
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let val = frame.regs().get(src, frame.ctx().heap());
    frame.regs().set(dst, val);
    next!()
}

#[handler(LoadSmi)]
fn op_load_smi(frame: Frame) {
    let imm = frame.signed::<0>();
    let val = Smi::new(imm as i64).into_tagged();
    frame.set_acc(val);
    next!()
}

#[handler(LoadZero)]
fn op_load_zero(frame: Frame) {
    frame.set_acc(Smi::new(0).into_tagged());
    next!()
}

#[handler(LoadNull)]
fn op_load_null(frame: Frame) {
    let heap = frame.ctx().heap();
    let val = roots.known(heap).null.as_tagged(heap).erase();
    frame.set_acc(val);
    next!()
}

#[handler(LoadConstant)]
fn op_load_constant(frame: Frame) {
    let heap = frame.ctx().heap();
    let idx = frame.unsigned::<0>();
    let v = frame.ctx().constants_ref(heap).at(heap, idx);
    frame.set_acc(v);
    next!()
}

#[handler(LoadUndefined)]
fn op_load_undefined(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.known(heap).undefined.as_tagged(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadTrue)]
fn op_load_true(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.known(heap).true_object.as_tagged(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadFalse)]
fn op_load_false(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.known(heap).false_object.as_tagged(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadHole)]
fn op_load_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.known(heap).the_hole.as_tagged(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadNewTarget)]
fn op_load_new_target(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .new_target_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

#[handler(LoadContext)]
fn op_load_context(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .context_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

#[handler(LoadCurrentClosure)]
fn op_load_current_closure(frame: Frame) {
    let val = frame
        .ctx()
        .stack()
        .frame_header(frame.ctx().frame_base())
        .callable_slot()
        .get(frame.ctx().heap());
    frame.set_acc(val);
    next!()
}

#[handler(LoadGlobal)]
fn op_load_global(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let global = roots.known(heap).global_object.as_tagged(heap).erase();
    if let Some(Hit::Value(v)) =
        InlineCache::try_load(heap, frame.ctx().feedback_ref(heap), fb, global)
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_global_load)
}

#[handler(LoadGlobalNoThrow)]
fn op_load_global_no_throw(frame: Frame) {
    tail!(slow_global_load_nothrow)
}

#[handler(LoadNamedProperty)]
fn op_load_named_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let fb = frame.unsigned::<2>();
    match InlineCache::probe_mono(heap, frame.ctx().feedback_ref(heap), fb, recv) {
        MonoProbe::Value(v) => {
            frame.set_acc(v);
            next!()
        }
        MonoProbe::Handler(obj, handler) => {
            if let Some(Hit::Value(v)) = InlineCache::apply_mono(heap, obj, handler) {
                frame.set_acc(v);
                next!()
            }
        }
        MonoProbe::Poly {
            obj,
            map,
            pairs,
            start,
        } => {
            if let Some(Hit::Value(v)) = InlineCache::try_load_resume(heap, obj, map, pairs, start)
            {
                frame.set_acc(v);
                next!()
            }
        }
        MonoProbe::Miss | MonoProbe::NotReceiver => {}
    }
    tail!(slow_named_load)
}

#[handler(LoadKeyedProperty)]
fn op_load_keyed_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let fb = frame.unsigned::<1>();
    if let Some(idx) = frame.acc().to_i64()
        && idx >= 0
    {
        if let Some(obj) = recv.as_heap_object()
            && let Some(v) = obj.as_ref().element_value_dense(heap, idx as usize)
        {
            frame.set_acc(v);
            next!()
        }
        if let Some(ElementHit::Value(v)) = InlineCache::try_load_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_load)
}

#[handler(LoadKeyedPropertyReg)]
fn op_load_keyed_property_reg(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        if let Some(obj) = recv.as_heap_object()
            && let Some(v) = obj.as_ref().element_value_dense(heap, idx as usize)
        {
            frame.set_acc(v);
            next!()
        }
        if let Some(ElementHit::Value(v)) = InlineCache::try_load_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_load_reg)
}

#[handler(LoadElementImm)]
fn op_load_element_imm(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let idx = frame.unsigned::<1>();
    let fb = frame.unsigned::<2>();
    if let Some(obj) = recv.as_heap_object()
        && let Some(v) = obj.as_ref().element_value_dense(heap, idx)
    {
        frame.set_acc(v);
        next!()
    }
    if let Some(ElementHit::Value(v)) =
        InlineCache::try_load_element(heap, frame.ctx().feedback_ref(heap), fb, recv, idx)
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_keyed_load_imm)
}

// --- stores ---

#[handler(Store)]
fn op_store(frame: Frame) {
    frame.regs().set(frame.signed::<0>(), frame.acc());
    next!()
}

#[handler(StoreNamedProperty)]
fn op_store_named_property(frame: Frame) {
    let recv = frame.regs().get(frame.signed::<0>(), frame.ctx().heap());
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let name = frame
        .ctx()
        .constants_ref(frame.ctx().heap())
        .at(frame.ctx().heap(), name_idx)
        .as_name();

    if InlineCache::try_store_fast(
        frame.ctx().heap(),
        frame.ctx().feedback_ref(frame.ctx().heap()),
        fb_slot,
        recv,
        name,
        frame.acc(),
    ) {
        next!()
    }
    tail!(slow_store_named)
}

#[handler(StoreKeyedProperty)]
fn op_store_keyed_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        if recv
            .store_array_element_in_place(heap, idx as usize, frame.acc())
            .is_ok()
        {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
            frame.acc(),
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_store)
}

#[handler(StoreKeyedPropertyNoShadow)]
fn op_store_keyed_property_no_shadow(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let key = frame.regs().get(frame.signed::<1>(), heap);
    let fb = frame.unsigned::<2>();
    if let Some(idx) = key.to_i64()
        && idx >= 0
    {
        if recv
            .store_array_element_in_place(heap, idx as usize, frame.acc())
            .is_ok()
        {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx as usize,
            frame.acc(),
        ) {
            frame.set_acc(v);
            next!()
        }
    }
    tail!(slow_keyed_store_no_shadow)
}

#[handler(StoreNamedPropertyNoShadow)]
fn op_store_named_property_no_shadow(frame: Frame) {
    tail!(slow_store_named_no_shadow)
}

#[handler(StoreGlobal)]
fn op_store_global(frame: Frame) {
    tail!(slow_store_global)
}

#[handler(Delete)]
fn op_delete(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let args = frame
        .ctx()
        .state()
        .stack()
        .window(base, frame.signed::<0>(), frame.unsigned::<1>());
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let target = args.get_handle(frame.ctx().heap(), 0);
    let raw_key = args.get_handle(frame.ctx().heap(), 1);
    let v = vm_core::intrinsics::delete_property_operator(
        vm,
        frame.ctx().heap_mut(),
        state,
        target,
        raw_key,
        mode,
    );
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(AddParent)]
fn op_add_parent(frame: Frame) {
    tail!(slow_add_parent)
}

// --- arithmetic ---

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

#[handler(Sub)]
fn op_sub(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(diff) = a.checked_sub(b)
    {
        frame.set_acc_smi(diff);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let diff = a - b;
        if let Some(s) = Smi::from_f64(diff) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(diff) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(diff);
        tail!(slow_box_number_sub)
    }
    tail!(slow_numeric)
}

#[handler(AddImmediate)]
fn op_add_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(sum) = bits.checked_add((imm as i64) << 1)
    {
        frame.set_acc_smi(sum);
        next!()
    }
    tail!(slow_add_immediate)
}

#[handler(SubImmediate)]
fn op_sub_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(diff) = bits.checked_sub((imm as i64) << 1)
    {
        frame.set_acc_smi(diff);
        next!()
    }
    tail!(slow_numeric_immediate)
}

#[handler(Mul)]
fn op_mul(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && let Some(product) = (a >> 1).checked_mul(b)
    {
        frame.set_acc_smi(product);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let product = a * b;
        if let Some(s) = Smi::from_f64(product) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(product) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(product);
        tail!(slow_box_number_mul)
    }
    tail!(slow_mul)
}

#[handler(Div)]
fn op_div(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), frame.acc().smi_bits())
        && b != 0
        && a % b == 0
        && let Some(quotient) = a.checked_div(b)
        && let Some(encoded) = quotient.checked_mul(2)
    {
        frame.set_acc_smi(encoded);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), frame.acc().as_number(heap)) {
        let quotient = a / b;
        if let Some(s) = Smi::from_f64(quotient) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(quotient) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(quotient);
        tail!(slow_box_number_div)
    }
    tail!(slow_div)
}

#[handler(Mod)]
fn op_mod(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64())
        && b != 0
    {
        let val = Smi::new(a % b).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_mod)
}

#[handler(Exp)]
fn op_exp(frame: Frame) {
    let _ = frame.signed::<0>();
    tail!(slow_exp)
}

#[handler(Negate)]
fn op_negate(frame: Frame) {
    if let Some(bits) = frame.acc().smi_bits()
        && bits != 0
        && let Some(neg) = bits.checked_neg()
    {
        frame.set_acc_smi(neg);
        next!()
    }
    if let Some(a) = frame.acc().as_number(frame.ctx().heap()) {
        let neg = -a;
        if let Some(s) = Smi::from_f64(neg) {
            frame.set_acc(s.into_tagged());
            next!()
        }
        if let Some(v) = frame.ctx().heap_mut().try_new_float(neg) {
            frame.set_acc(v);
            next!()
        }
        frame.set_float(neg);
        tail!(slow_box_number_negate)
    }
    tail!(slow_negate)
}

#[handler(MulImmediate)]
fn op_mul_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits()
        && let Some(product) = (bits >> 1).checked_mul(imm as i64)
        && let Some(encoded) = product.checked_mul(2)
    {
        frame.set_acc_smi(encoded);
        next!()
    }
    tail!(slow_mul_immediate)
}

#[handler(DivImmediate)]
fn op_div_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(bits) = lhs.smi_bits() {
        let value = bits >> 1;
        if imm != 0
            && value % imm as i64 == 0
            && let Some(quotient) = value.checked_div(imm as i64)
            && let Some(encoded) = quotient.checked_mul(2)
        {
            frame.set_acc_smi(encoded);
            next!()
        }
    }
    tail!(slow_div_immediate)
}

#[handler(ModImmediate)]
fn op_mod_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if imm != 0
        && let Some(bits) = lhs.smi_bits()
    {
        frame.set_acc_smi(((bits >> 1) % imm as i64) << 1);
        next!()
    }
    tail!(slow_mod_immediate)
}

#[handler(ExpImmediate)]
fn op_exp_immediate(frame: Frame) {
    let _ = (frame.signed::<0>(), frame.signed::<1>());
    tail!(slow_exp_immediate)
}

#[handler(AddLoc)]
fn op_add_loc(frame: Frame) {
    let heap = frame.ctx().heap();
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let lhs = frame.regs().get(dst, heap);
    let rhs = frame.regs().get(src, heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), rhs.smi_bits())
        && let Some(sum) = a.checked_add(b)
    {
        let val = Tagged::from_smi_bits(sum);
        frame.regs().set(dst, val);
        frame.set_acc(val);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), rhs.as_number(heap)) {
        let sum = a + b;
        if let Some(s) = Smi::from_f64(sum) {
            let val = s.into_tagged();
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        if let Some(val) = frame.ctx().heap_mut().try_new_float(sum) {
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        frame.set_float(sum);
        tail!(slow_box_add_loc)
    }
    tail!(slow_add_loc)
}

#[handler(SubLoc)]
fn op_sub_loc(frame: Frame) {
    let heap = frame.ctx().heap();
    let dst = frame.signed::<0>();
    let src = frame.signed::<1>();
    let lhs = frame.regs().get(dst, heap);
    let rhs = frame.regs().get(src, heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), rhs.smi_bits())
        && let Some(diff) = a.checked_sub(b)
    {
        let val = Tagged::from_smi_bits(diff);
        frame.regs().set(dst, val);
        frame.set_acc(val);
        next!()
    }
    if let (Some(a), Some(b)) = (lhs.as_number(heap), rhs.as_number(heap)) {
        let diff = a - b;
        if let Some(s) = Smi::from_f64(diff) {
            let val = s.into_tagged();
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        if let Some(val) = frame.ctx().heap_mut().try_new_float(diff) {
            frame.regs().set(dst, val);
            frame.set_acc(val);
            next!()
        }
        frame.set_float(diff);
        tail!(slow_box_sub_loc)
    }
    tail!(slow_sub_loc)
}

// --- bitwise / shifts ---

#[handler(BitwiseOr)]
fn op_bitwise_or(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 | b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_or)
}

#[handler(BitwiseXor)]
fn op_bitwise_xor(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 ^ b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_xor)
}

#[handler(BitwiseAnd)]
fn op_bitwise_and(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32 & b as i32) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_and)
}

#[handler(ShiftLeft)]
fn op_shift_left(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32).wrapping_shl(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_left)
}

#[handler(ShiftRight)]
fn op_shift_right(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as i32).wrapping_shr(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right)
}

#[handler(ShiftRightLogical)]
fn op_shift_right_logical(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.to_i64(), frame.acc().to_i64()) {
        let val = Smi::new((a as u32).wrapping_shr(b as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_logical)
}

#[handler(BitwiseOrImmediate)]
fn op_bitwise_or_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 | imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_or_immediate)
}

#[handler(BitwiseXorImmediate)]
fn op_bitwise_xor_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 ^ imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_xor_immediate)
}

#[handler(BitwiseAndImmediate)]
fn op_bitwise_and_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32 & imm) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_bitwise_and_immediate)
}

#[handler(ShiftLeftImmediate)]
fn op_shift_left_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32).wrapping_shl(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_left_immediate)
}

#[handler(ShiftRightImmediate)]
fn op_shift_right_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as i32).wrapping_shr(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_immediate)
}

#[handler(ShiftRightLogicalImmediate)]
fn op_shift_right_logical_immediate(frame: Frame) {
    let heap = frame.ctx().heap();
    let lhs = frame.regs().get(frame.signed::<0>(), heap);
    let imm = frame.signed::<1>();
    if let Some(a) = lhs.to_i64() {
        let val = Smi::new((a as u32).wrapping_shr(imm as u32 & 31) as i64).into_tagged();
        frame.set_acc(val);
        next!()
    }
    tail!(slow_shift_right_logical_immediate)
}

// --- comparisons ---

#[handler(Equal)]
fn op_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a == b));
        next!()
    }
    tail!(slow_equal)
}

#[handler(LessThan)]
fn op_less_than(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a < b));
        next!()
    }
    tail!(slow_less_than)
}

#[handler(GreaterThan)]
fn op_greater_than(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a > b));
        next!()
    }
    tail!(slow_greater_than)
}

#[handler(LessThanOrEqual)]
fn op_less_than_or_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a <= b));
        next!()
    }
    tail!(slow_less_than_or_equal)
}

#[handler(GreaterThanOrEqual)]
fn op_greater_than_or_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        frame.set_acc(Convert::boolean(heap, a >= b));
        next!()
    }
    tail!(slow_greater_than_or_equal)
}

#[handler(EqualStrict)]
fn op_equal_strict(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    frame.set_acc(Convert::boolean(heap, Compare::strict_equal(heap, frame.acc(), other)));
    next!()
}

#[handler(TestReferenceEqual)]
fn op_test_reference_equal(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    frame.set_acc(Convert::boolean(heap, other == frame.acc()));
    next!()
}

#[handler(TestTypeof)]
fn op_test_typeof(frame: Frame) {
    let heap = frame.ctx().heap();
    frame.set_acc(Object::type_of(heap, frame.acc()));
    next!()
}

#[handler(InstanceOf)]
fn op_instance_of(frame: Frame) {
    let _ = frame.signed::<0>();
    tail!(slow_instance_of)
}

#[handler(CompareJump)]
fn op_compare_jump(frame: Frame) {
    let heap = frame.ctx().heap();
    let other = frame.regs().get(frame.signed::<0>(), heap);
    let kind = frame.unsigned::<1>();
    let off = frame.signed::<2>();
    let cmp = (kind / 2) as u8;
    let falsy_jump = kind % 2 == 1;
    if let (Some(a), Some(b)) = (frame.acc().smi_bits(), other.smi_bits()) {
        let taken = match cmp {
            0 | 1 => a == b,
            2 => a < b,
            3 => a <= b,
            4 => a > b,
            _ => a >= b,
        };
        frame.set_acc(Convert::boolean(heap, taken));
        if taken != falsy_jump {
            dispatch!(frame.ip().wrapping_offset(off as isize))
        }
        next!()
    }
    tail!(slow_compare_jump)
}

// --- locals ---

#[handler(IncLoc)]
fn op_inc_loc(frame: Frame) {
    let r = frame.signed::<0>();
    if let Some(bits) = frame.regs().get(r, frame.ctx().heap()).smi_bits() {
        if let Some(new) = bits.checked_add(2) {
            frame.regs().set(r, Tagged::from_smi_bits(new));
            frame.set_acc_smi(bits);
            next!()
        }
        tail!(slow_inc_loc)
    }
    let heap = frame.ctx().heap();
    let v = frame.regs().get(r, heap);
    if let Some(n) = v.as_number(heap) {
        let new = n + 1.0;
        if let Some(s) = Smi::from_f64(new) {
            frame.regs().set(r, s.into_tagged());
            frame.set_acc(v);
            next!()
        }
        // acc keeps the old value; park it before the allocating tail
        frame.set_acc(v);
        if let Some(t) = frame.ctx().heap_mut().try_new_float(new) {
            frame.regs().set(r, t);
            next!()
        }
    }
    tail!(slow_inc_loc)
}

#[handler(DecLoc)]
fn op_dec_loc(frame: Frame) {
    let r = frame.signed::<0>();
    if let Some(bits) = frame.regs().get(r, frame.ctx().heap()).smi_bits() {
        if let Some(new) = bits.checked_sub(2) {
            frame.regs().set(r, Tagged::from_smi_bits(new));
            frame.set_acc_smi(bits);
            next!()
        }
        tail!(slow_dec_loc)
    }
    let heap = frame.ctx().heap();
    let v = frame.regs().get(r, heap);
    if let Some(n) = v.as_number(heap) {
        let new = n - 1.0;
        if let Some(s) = Smi::from_f64(new) {
            frame.regs().set(r, s.into_tagged());
            frame.set_acc(v);
            next!()
        }
        // acc keeps the old value; park it before the allocating tail
        frame.set_acc(v);
        if let Some(t) = frame.ctx().heap_mut().try_new_float(new) {
            frame.regs().set(r, t);
            next!()
        }
    }
    tail!(slow_dec_loc)
}

// --- control flow ---

#[handler(Jump)]
fn op_jump(frame: Frame) {
    dispatch!(frame.ip().wrapping_offset(frame.signed::<0>() as isize))
}

#[handler(JumpIfTruthy)]
fn op_jump_if_truthy(frame: Frame) {
    let off = frame.signed::<0>();
    if frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfFalsy)]
fn op_jump_if_falsy(frame: Frame) {
    let off = frame.signed::<0>();
    if !frame.acc().is_truthy(frame.ctx().heap()) {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}

#[handler(JumpIfNotUndefined)]
fn op_jump_if_not_undefined(frame: Frame) {
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
        new_target != roots.known(heap).undefined.as_tagged(heap).erase()
    };

    if constructing {
        let heap = frame.ctx().heap();
        let val = frame.acc();
        if val.is_primitive(heap) {
            let this_val = frame.regs().get(0, heap);
            if this_val == roots.known(heap).the_hole.as_tagged(heap).erase() {
                // a derived constructor escaped without initializing `this`
                let caller = frame.ctx().stack().pop_frame(frame.ctx().frame_base());
                frame.ctx().switch_frame(caller.base);
                let _ = frame.ctx().raise_tag(VmError::Reference);
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

#[handler(Call)]
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

#[handler(CallNoFeedback)]
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

#[handler(CallRuntime)]
fn op_call_runtime(frame: Frame) {
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

#[handler(CallMethod0)]
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

#[handler(CallMethod1)]
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

#[handler(CallMethod2)]
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

#[handler(CallFunction0)]
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
        roots
            .known(frame.ctx().heap())
            .undefined
            .as_tagged(frame.ctx().heap())
            .erase(),
        &[arg0, arg1][..2],
    );
    call_enter!(result, pc)
}

// --- literals ---

#[handler(CreateClosure)]
fn op_create_closure(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let info_idx = frame.unsigned::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let Some(info) = scope.cast::<CallableInfoObject>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(&*heap)
                .at(&*heap, info_idx),
        ) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let context = scope
            .cast::<ContextObject>(heap, state.stack().frame_header(base).context_slot().get(heap))
            .expect("frame context slot holds a ContextObject");
        match Object::create_closure(heap, &scope, info, context) {
            Ok(obj) => obj.erase(),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CreateEmptyArrayLiteral)]
fn op_create_empty_array_literal(frame: Frame) {
    if let Some(v) = frame.ctx().heap_mut().try_new_array_literal() {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let map = roots.known(heap).js_array_map;
        heap.new_object(&scope, map, HandleSlice::EMPTY).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(CreateSizedArrayLiteral)]
fn op_create_sized_array_literal(frame: Frame) {
    let capacity = frame.unsigned::<0>();
    if let Some(v) = frame.ctx().heap_mut().try_new_array_with_capacity(capacity) {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        frame
            .ctx()
            .heap_mut()
            .new_array_with_capacity(&scope, capacity)
            .erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CreateEmptyObjectLiteral)]
fn op_create_empty_object_literal(frame: Frame) {
    if let Some(v) = frame.ctx().heap_mut().try_new_empty_object() {
        frame.set_acc(v.erase());
        next!()
    }
    let pc = frame.pc();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let map = roots.known(heap).object_initial_map;
        heap.new_object(&scope, map, HandleSlice::EMPTY).erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CreateBlockContext)]
fn op_create_block_context(frame: Frame) {
    tail!(slow_create_block_context)
}

// --- contexts ---

#[handler(LoadContextSlot)]
fn op_load_context_slot(frame: Frame) {
    let slot = frame.unsigned::<0>();
    let depth = frame.unsigned::<1>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    if depth == 0 {
        let Some(context) = frame
            .ctx()
            .stack()
            .frame_header(base)
            .context_slot()
            .get(heap)
            .get_as::<ContextObject>(heap)
        else {
            let _ = frame.ctx().raise_tag(VmError::Type);
            tail!(throw_dispatch)
        };
        let v = context
            .as_ref()
            .slots
            .get(heap)
            .as_ref()
            .element_slot(slot)
            .get(heap);
        frame.set_acc(v);
        next!()
    }
    let Some(mut context) = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap)
        .get_as::<ContextObject>(heap)
    else {
        let _ = frame.ctx().raise_tag(VmError::Type);
        tail!(throw_dispatch)
    };
    for _ in 0..depth {
        context = match context.as_ref().outer.get(heap) {
            Some(context) => context,
            None => {
                let _ = frame.ctx().raise_tag(VmError::Type);
                tail!(throw_dispatch)
            }
        };
    }
    let v = context
        .as_ref()
        .slots
        .get(heap)
        .as_ref()
        .element_slot(slot)
        .get(heap);
    frame.set_acc(v);
    next!()
}

#[handler(StoreContextSlot)]
fn op_store_context_slot(frame: Frame) {
    let slot = frame.unsigned::<0>();
    let depth = frame.unsigned::<1>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    let Some(mut context) = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap)
        .get_as::<ContextObject>(heap)
    else {
        let _ = frame.ctx().raise_tag(VmError::Type);
        tail!(throw_dispatch)
    };
    for _ in 0..depth {
        context = match context.as_ref().outer.get(heap) {
            Some(context) => context,
            None => {
                let _ = frame.ctx().raise_tag(VmError::Type);
                tail!(throw_dispatch)
            }
        };
    }
    let host = context.erase();
    let value = frame.acc();
    context
        .as_ref()
        .slots
        .get(heap)
        .as_ref()
        .element_slot(slot)
        .set(heap, host, value);
    next!()
}

#[handler(PushContext)]
fn op_push_context(frame: Frame) {
    let r = frame.signed::<0>();
    let base = frame.ctx().frame_base();
    let heap = frame.ctx().heap();
    let old = frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .get(heap);
    frame.regs().set(r, old);
    if frame.acc().get_as::<ContextObject>(heap).is_none() {
        let _ = frame.ctx().raise_tag(VmError::Type);
        tail!(throw_dispatch)
    }
    let value = frame.acc();
    frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .store(value);
    next!()
}

#[handler(PopContext)]
fn op_pop_context(frame: Frame) {
    let r = frame.signed::<0>();
    let base = frame.ctx().frame_base();
    let context = frame.regs().get(r, frame.ctx().heap());
    if context
        .get_as::<ContextObject>(frame.ctx().heap())
        .is_none()
    {
        let _ = frame.ctx().raise_tag(VmError::Type);
        tail!(throw_dispatch)
    }
    frame
        .ctx()
        .stack()
        .frame_header(base)
        .context_slot()
        .store(context);
    next!()
}

#[handler(CreateFunctionContext)]
fn op_create_function_context(frame: Frame) {
    // allocation is the operation and there is no faster path to tail
    // into: the whole body lives here
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let scope_idx = frame.unsigned::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let outer = scope
            .cast::<ContextObject>(heap, state.stack().frame_header(base).context_slot().get(heap))
            .expect("frame context slot holds a ContextObject");
        let Some(count) = state
            .stack()
            .frame_header(base)
            .constants(&*heap)
            .at(&*heap, scope_idx)
            .get_as::<ScopeInfo>(heap)
            .map(|r| r.as_ref().names.get(heap).len())
        else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let Some(scope_info) = scope.cast::<ScopeInfo>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(&*heap)
                .at(&*heap, scope_idx),
        ) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let slots = if count == 0 {
            roots.known(heap).empty_fixed_array
        } else {
            let values =
                scope.stage(&vec![roots.known(heap).the_hole.as_tagged(heap).erase(); count]);
            heap.allocate_handle::<FixedArray>(values, &scope)
        };
        let ctx_obj = heap.allocate::<ContextObject>(ContextInit {
            outer: Some(outer),
            slots,
            scope_info,
        });
        ctx_obj.erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- exceptions ---

#[handler(Throw)]
fn op_throw(frame: Frame) {
    frame.ctx().state().set_pending_exception(frame.acc());
    tail!(throw_dispatch)
}

#[handler(Construct)]
fn op_construct(frame: Frame) {
    let pc = frame.pc();
    let size =
        (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::Construct.operands().len() };
    let callee_reg = frame.signed::<0>();
    let args_base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    match construct_start(frame.ctx(), pc, size, frame.regs(), callee_reg, args_base, count) {
        Ok(ConstructStart::Frame) => {
            let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
            let acc = Acc::set(
                roots
                    .known(frame.ctx().heap())
                    .undefined
                    .as_tagged(frame.ctx().heap())
                    .erase(),
            );
            let entry = frame.ctx().code_ptr();
            let h = table.get(unsafe { *entry });
            become h(entry, regs, acc, frame.ctx(), table, roots, float)
        }
        Ok(ConstructStart::Native(rt)) => {
            let val = construct_native(frame.ctx(), rt, callee_reg, args_base, count);
            frame.set_acc(val);
            reenter!(pc)
        }
        Ok(ConstructStart::Threw) => tail!(throw_dispatch),
        Ok(ConstructStart::Slow) => tail!(slow_construct),
        Err(e) => {
            let _ = frame.ctx().raise_tag(e);
            tail!(throw_dispatch)
        }
    }
}

#[handler(ThrowReferenceErrorIfHole)]
fn op_throw_reference_error_if_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    if frame.acc() == roots.known(heap).the_hole.as_tagged(heap).erase() {
        let _ = frame.ctx().raise_tag(VmError::Reference);
        tail!(throw_dispatch)
    }
    next!()
}

handler_table!(
    op_wide,
    op_load,
    op_move,
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
    op_load_smi,
    op_load_zero,
    op_load_constant,
    op_load_undefined,
    op_load_null,
    op_load_true,
    op_load_false,
    op_load_global,
    op_sub,
    op_mul,
    op_div,
    op_mod,
    op_exp,
    op_negate,
    op_add_immediate,
    op_sub_immediate,
    op_mul_immediate,
    op_div_immediate,
    op_mod_immediate,
    op_exp_immediate,
    op_add_loc,
    op_sub_loc,
    op_bitwise_or,
    op_bitwise_xor,
    op_bitwise_and,
    op_shift_left,
    op_shift_right,
    op_shift_right_logical,
    op_bitwise_or_immediate,
    op_bitwise_xor_immediate,
    op_bitwise_and_immediate,
    op_shift_left_immediate,
    op_shift_right_immediate,
    op_shift_right_logical_immediate,
    op_equal,
    op_less_than,
    op_greater_than,
    op_less_than_or_equal,
    op_greater_than_or_equal,
    op_equal_strict,
    op_test_reference_equal,
    op_test_typeof,
    op_instance_of,
    op_compare_jump,
    op_inc_loc,
    op_dec_loc,
    op_load_keyed_property,
    op_load_keyed_property_reg,
    op_load_element_imm,
    op_load_named_property,
    op_store_named_property,
    op_store_keyed_property,
    op_store_named_property_no_shadow,
    op_store_keyed_property_no_shadow,
    op_store_global,
    op_delete,
    op_create_closure,
    op_throw,
    op_add_parent,
    op_load_context_slot,
    op_store_context_slot,
    op_push_context,
    op_pop_context,
    op_create_function_context,
    op_create_empty_array_literal,
    op_create_sized_array_literal,
    op_construct,
    op_create_empty_object_literal,
    op_create_block_context,
    op_load_hole,
    op_load_new_target,
    op_load_context,
    op_load_current_closure,
    op_throw_reference_error_if_hole,
    op_load_global_no_throw,
);

pub static TABLE: DispatchTable = DispatchTable {
    narrow: __table_narrow(),
    wide: __table_wide(),
};

// --- entry ---

fn enter<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'a, Value>>,
) -> Result<Acc, VmError> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => Err(VmError::Type),
        // the proxy dispatch above already intercepted these
        Some(CallTarget::Proxy(_)) => Err(VmError::Type),
        Some(CallTarget::Native(rt)) => {
            let f = vm.runtime(rt);
            let nctx = HostCtx::new(vm, heap, state);
            Ok(Acc::set(f(nctx, new_target, args.as_args())))
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
            let ip = ctx.code_ptr();
            let regs = unsafe { Regs::new(ctx.regs_ptr()) };
            let table = TableArg::new(&TABLE);
            let roots = RootsArg::from_heap(ctx.heap());

            let acc = Acc::set(
                roots
                    .known(ctx.heap())
                    .undefined
                    .as_tagged(ctx.heap())
                    .erase(),
            );
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
        let acc = enter(vm, heap, state, callable, args, new_target)?;
        let rooted = scope.handle(acc.get(heap));
        stack.set_top(saved_top);
        if let Some(outer) = outer {
            state.set_frame_base(outer);
        } else {
            state.set_frame_active(false);
        }
        Ok(rooted.as_tagged(heap))
    })
}

pub struct BecomeInterpreter;

impl Interpreter for BecomeInterpreter {
    const EXECUTE: EntryFn = execute;
}
