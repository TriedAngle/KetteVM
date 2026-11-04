#![cfg_attr(
    feature = "fast",
    feature(explicit_tail_calls, rust_preserve_none_cc, fn_align)
)]
#![allow(incomplete_features)]
#![allow(unused_imports, unused_macros, unused_unsafe, unused_variables)]
#![allow(dead_code)]

use core::hint::black_box;
use std::marker::PhantomData;

#[cfg(not(feature = "fast"))]
use handler_derive::handler_match as handler;
#[cfg(feature = "fast")]
use handler_derive::{handler_become as handler, handler_table};

use bytecode::Opcode;
use vm_core::bootstrap::WellKnown;
use vm_core::ic::{ElementHit, GlobalLexicalHit, InlineCache, LoadHit, MonoProbe};
use vm_core::{
    Acc, CallTarget, CallableInfoObject, Callee, Compare, ContextInit, ContextObject, Convert, Ctx,
    FeedbackVector, FixedArray, HandleSlice, Heap, HostCtx, NativeIndex, Object, Ops, Regs,
    ScopeInfo, Smi, Tagged, Value, VmError, raise_runtime,
};

#[cfg(feature = "fast")]
#[path = "become.rs"]
pub mod become_backend;
#[cfg(not(feature = "fast"))]
pub mod match_loop;
#[cfg(feature = "fast")]
pub mod register_probe;

#[cfg(feature = "fast")]
pub use become_backend::{BecomeInterpreter, DispatchTable, Handler};
#[cfg(feature = "fast")]
use become_backend::{TableArg, throw_dispatch};
#[cfg(not(feature = "fast"))]
pub use match_loop::MatchInterpreter;
pub use vm_core::{EntryFn, Interpreter};

#[cfg(feature = "fast")]
pub type DefaultInterpreter = become_backend::BecomeInterpreter;
#[cfg(not(feature = "fast"))]
pub type DefaultInterpreter = match_loop::MatchInterpreter;
mod slow;
mod utils;

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

#[derive(Clone, Copy)]
pub struct RootsArg<'s>(&'s WellKnown);

impl<'s> RootsArg<'s> {
    #[inline(always)]
    pub fn new(known: &'s WellKnown) -> RootsArg<'s> {
        RootsArg(known)
    }

    #[inline(always)]
    pub fn known(&self, _heap: &Heap) -> &'s WellKnown {
        self.0
    }
}

impl core::ops::Deref for RootsArg<'_> {
    type Target = WellKnown;

    #[inline(always)]
    fn deref(&self) -> &WellKnown {
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
    let val = roots.null.get(heap).erase();
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
    let v = roots.undefined.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadTrue)]
fn op_load_true(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.true_object.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadFalse)]
fn op_load_false(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.false_object.get(heap).erase();
    frame.set_acc(v);
    next!()
}

#[handler(LoadHole)]
fn op_load_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    let v = roots.the_hole.get(heap).erase();
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

enum GlobalLoadFast<'a> {
    Value(Tagged<'a, Value>),
    Getter(Tagged<'a, Value>),
    Uninitialized,
    Miss,
}

fn global_load_fast<'a>(
    heap: &'a Heap,
    vector: Option<Tagged<'a, FeedbackVector>>,
    fb_slot: usize,
    global: Tagged<'a, Value>,
) -> GlobalLoadFast<'a> {
    let hit = match InlineCache::probe_mono(heap, vector, fb_slot, global) {
        MonoProbe::Value(v) => Some(LoadHit::Value(v)),
        MonoProbe::Handler(obj, handler) => InlineCache::apply_mono(heap, obj, handler),
        MonoProbe::Poly {
            obj,
            map,
            pairs,
            start,
        } => InlineCache::try_load_resume(heap, obj, map, pairs, start),
        MonoProbe::Miss | MonoProbe::NotReceiver => None,
    };
    if let Some(hit) = hit {
        match hit {
            LoadHit::Value(v) => return GlobalLoadFast::Value(v),
            LoadHit::Getter(getter) => return GlobalLoadFast::Getter(getter),
            _ => {}
        }
    }
    match InlineCache::try_load_global_lexical(heap, vector, fb_slot) {
        Some(GlobalLexicalHit::Value(v)) => GlobalLoadFast::Value(v),
        Some(GlobalLexicalHit::Uninitialized) => GlobalLoadFast::Uninitialized,
        None => GlobalLoadFast::Miss,
    }
}

#[handler(LoadGlobal)]
fn op_load_global(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let global = roots.global_object.get(heap).erase();
    match global_load_fast(heap, frame.ctx().feedback_ref(heap), fb, global) {
        GlobalLoadFast::Value(v) => {
            frame.set_acc(v);
            next!()
        }
        GlobalLoadFast::Getter(getter) => {
            frame.set_acc(getter);
            tail!(slow_global_getter)
        }
        GlobalLoadFast::Uninitialized => {
            let _ = frame.ctx().raise_tag(VmError::Reference);
            tail!(throw_dispatch)
        }
        GlobalLoadFast::Miss => {
            tail!(slow_global_load)
        }
    }
}

#[handler(LoadGlobalNoThrow)]
fn op_load_global_no_throw(frame: Frame) {
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let global = roots.global_object.get(heap).erase();
    match global_load_fast(heap, frame.ctx().feedback_ref(heap), fb, global) {
        GlobalLoadFast::Value(v) => {
            frame.set_acc(v);
            next!()
        }
        GlobalLoadFast::Getter(getter) => {
            frame.set_acc(getter);
            tail!(slow_global_getter)
        }
        GlobalLoadFast::Uninitialized => {
            let _ = frame.ctx().raise_tag(VmError::Reference);
            tail!(throw_dispatch)
        }
        GlobalLoadFast::Miss => {
            tail!(slow_global_load_no_throw)
        }
    }
}

#[handler(LoadNamedProperty)]
fn op_load_named_property(frame: Frame) {
    let heap = frame.ctx().heap();
    let recv = frame.regs().get(frame.signed::<0>(), heap);
    let fb = frame.unsigned::<2>();
    let hit = match InlineCache::probe_mono(heap, frame.ctx().feedback_ref(heap), fb, recv) {
        MonoProbe::Value(v) => Some(LoadHit::Value(v)),
        MonoProbe::Handler(obj, handler) => InlineCache::apply_mono(heap, obj, handler),
        MonoProbe::Poly {
            obj,
            map,
            pairs,
            start,
        } => InlineCache::try_load_resume(heap, obj, map, pairs, start),
        MonoProbe::Miss | MonoProbe::NotReceiver => None,
    };
    if let Some(hit) = hit {
        match hit {
            LoadHit::Value(v) => {
                frame.set_acc(v);
                next!()
            }
            LoadHit::Getter(getter) => {
                frame.set_acc(getter);
                tail!(slow_named_getter)
            }
            _ => {}
        }
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
        let idx = idx as usize;
        if recv
            .store_array_element_in_place(heap, idx, frame.acc())
            .is_ok()
        {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx,
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
        let idx = idx as usize;
        if recv
            .store_array_element_in_place(heap, idx, frame.acc())
            .is_ok()
        {
            next!()
        }
        if let Some(v) = InlineCache::try_store_element(
            heap,
            frame.ctx().feedback_ref(heap),
            fb,
            recv,
            idx,
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
    let heap = frame.ctx().heap();
    let fb = frame.unsigned::<1>();
    let value = frame.acc();
    let name_idx = frame.unsigned::<0>();
    let name = frame
        .ctx()
        .constants_ref(heap)
        .at(heap, name_idx)
        .erase()
        .as_name();
    let global = roots.global_object.get(heap).erase();
    if InlineCache::try_store_fast(
        heap,
        frame.ctx().feedback_ref(heap),
        fb,
        global,
        name,
        value,
    ) {
        frame.set_acc(value);
        next!()
    }
    if let Some(v) =
        InlineCache::try_store_global_lexical(heap, frame.ctx().feedback_ref(heap), fb, value)
    {
        frame.set_acc(v);
        next!()
    }
    tail!(slow_store_global)
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
    frame.set_acc(Convert::boolean(
        heap,
        Compare::strict_equal(heap, frame.acc(), other),
    ));
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
    let undefined = roots.undefined.get(frame.ctx().heap()).erase();
    if frame.acc() != undefined {
        dispatch!(frame.ip().wrapping_offset(off as isize))
    }
    next!()
}
#[handler(CallNative)]
fn op_call_native(frame: Frame) {
    let pc = frame.pc();
    let rt = frame.unsigned::<0>();
    let base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let native = vm.native(NativeIndex(rt));
    let args = state.stack().window(frame.ctx().frame_base(), base, count);
    let nctx = HostCtx::new(vm, frame.ctx().runtime(), frame.ctx().heap_mut(), state);
    let v = native(nctx, None, args);
    frame.set_acc(v);
    reenter!(pc)
}
// --- literals ---

#[handler(CreateClosure)]
fn op_create_closure(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let info_idx = frame.unsigned::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| -> Acc {
        let heap = frame.ctx().heap_mut();
        let Some(info) = scope.cast::<CallableInfoObject>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, info_idx),
        ) else {
            return Acc::set(raise_runtime(vm, heap, state, VmError::Type));
        };
        let context = scope
            .cast::<ContextObject>(
                heap,
                state.stack().frame_header(base).context_slot().get(heap),
            )
            .expect("frame context slot holds a ContextObject");
        match Object::create_closure(heap, &scope, info, context) {
            Ok(obj) => Acc::set(obj.erase()),
            Err(e) => Acc::set(raise_runtime(vm, heap, state, e)),
        }
    });
    frame.set_acc(v.get(frame.ctx().heap()));
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
        let map = scope.handle(roots.js_array_map.get(heap));
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
        let map = scope.handle(roots.object_initial_map.get(heap));
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
            .cast::<ContextObject>(
                heap,
                state.stack().frame_header(base).context_slot().get(heap),
            )
            .expect("frame context slot holds a ContextObject");
        let Some(count) = state
            .stack()
            .frame_header(base)
            .constants(heap)
            .at(heap, scope_idx)
            .get_as::<ScopeInfo>(heap)
            .map(|r| r.local_count())
        else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let Some(scope_info) = scope.cast::<ScopeInfo>(
            heap,
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, scope_idx),
        ) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let slots = if count == 0 {
            scope.handle(roots.empty_fixed_array.get(heap))
        } else {
            let values = scope.stage(&vec![roots.the_hole.get(heap).erase(); count]);
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
#[handler(ThrowReferenceErrorIfHole)]
fn op_throw_reference_error_if_hole(frame: Frame) {
    let heap = frame.ctx().heap();
    if frame.acc() == roots.the_hole.get(heap).erase() {
        let _ = frame.ctx().raise_tag(VmError::Reference);
        tail!(throw_dispatch)
    }
    next!()
}
