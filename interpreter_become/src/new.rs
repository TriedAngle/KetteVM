#![allow(dead_code)]

use core::cell::Cell;
use handler_derive::{handler, handler_table};

use bytecode::Opcode;
use vm_core::ic::{ElementHit, Hit, InlineCache, MonoProbe};
use vm_core::{
    Args, CallTarget, CallableInfoObject, Callee, ContextInit, ContextObject, Convert, EntryFn,
    FeedbackVector, FixedArray, FrameMeta, Handle, HandleSlice, Heap, HostCtx, Interpreter,
    NativeIndex, Object, Register, ScopeInfo, Smi, Tagged, ThreadState, VM, Value, VmError, Word,
    interpreter::SAFEPOINT_INTERVAL, raise_runtime,
};

use crate::new_macros::*;
use crate::new_slow::*;
use crate::new_utils::*;
#[cfg(feature = "star_fusion")]
use crate::star_lookahead;
use crate::{FloatReg, Ops, Regs, RootsArg};

/// Placeholder receiver type for `#[handler]` fns.
/// The proc macro rewrites access to it.
#[doc(hidden)]
pub struct Frame;

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

    /// Switch to the frame at `base` (pop/unwind)
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

    #[inline(always)]
    pub fn switch_frame_code(&self, base: usize, code: *const u8) {
        self.state.set_frame_base(base);
        self.code.set(code);
    }

    /// The current frame's feedback vector
    #[inline(always)]
    pub fn feedback_ref<'f>(&self, heap: &'f Heap) -> Option<Tagged<'f, FeedbackVector>> {
        self.stack().frame_header(self.frame_base()).feedback(heap)
    }

    /// The current frame's constant pool.
    #[inline(always)]
    pub fn constants_ref<'f>(&self, heap: &'f Heap) -> Tagged<'f, FixedArray> {
        self.stack().frame_header(self.frame_base()).constants(heap)
    }

    #[inline(always)]
    pub fn regs_ptr(&self) -> *mut Register {
        unsafe { self.stack().slots_ptr().add(self.frame_base()) }
    }

    #[inline(always)]
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

// --- trampolines ---

/// Peel the `Wide` prefix and dispatch the stride-2 handler
/// instantiation, keeping the NARROW table: the next instruction
/// (whatever its width) dispatches narrow-first, and a following
/// `Wide` prefix comes back through here. The table argument is
/// narrow for the whole execution; only the handler stride varies.
#[handler]
fn op_wide(frame: Frame) -> Acc {
    let h = TableArg::new(&NEW_TABLE_WIDE).get(unsafe { *ip.add(1) });
    become h(ip, regs, acc, ctx, table, roots, float)
}

#[handler]
fn op_trap(frame: Frame) -> Acc {
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

#[handler(CompareJump)]
fn op_compare_jump(frame: Frame) -> Acc {
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

// --- calls ---

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

#[handler(CallNoFeedback)]
fn op_call_no_feedback(frame: Frame) -> Acc {
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

// --- literals ---

#[handler(CreateClosure)]
fn op_create_closure(frame: Frame) -> Acc {
    // `Object::create_closure` is more than an allocation (length/name
    // property transitions, a fresh per-closure prototype for `normal`
    // functions): no single-bump fast path exists until its final shape
    // is cached per callable info. The whole body lives here — there is
    // no faster path to tail into.
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
fn op_create_empty_array_literal(frame: Frame) -> Acc {
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

// --- contexts ---

#[handler(LoadContextSlot)]
fn op_load_context_slot(frame: Frame) -> Acc {
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
fn op_store_context_slot(frame: Frame) -> Acc {
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
fn op_push_context(frame: Frame) -> Acc {
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
fn op_pop_context(frame: Frame) -> Acc {
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
fn op_create_function_context(frame: Frame) -> Acc {
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
fn op_throw(frame: Frame) -> Acc {
    frame.ctx().state().set_pending_exception(frame.acc());
    tail!(throw_dispatch)
}

#[handler(ThrowReferenceErrorIfHole)]
fn op_throw_reference_error_if_hole(frame: Frame) -> Acc {
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
    op_compare_jump,
    op_inc_loc,
    op_dec_loc,
    op_load_keyed_property,
    op_load_keyed_property_reg,
    op_load_element_imm,
    op_store_keyed_property,
    op_load_named_property,
    op_create_closure,
    op_throw,
    op_store_named_property,
    op_load_context_slot,
    op_store_context_slot,
    op_push_context,
    op_pop_context,
    op_create_function_context,
    op_create_empty_array_literal,
    op_load_hole,
    op_load_new_target,
    op_load_context,
    op_load_current_closure,
    op_throw_reference_error_if_hole,
);

// --- entry ---

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
            let table = TableArg::new(&NEW_TABLE_NARROW);
            let roots = RootsArg::from_heap(ctx.heap());
            // the fresh frame is entered at pc 0 with the accumulator
            // seeded undefined
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
