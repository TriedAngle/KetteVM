//! The per-execution interpreter context shared by the interpreter
//! implementations. Hot accessors are `#[inline(always)]` so they
//! keep inlining into interpreter crates across the crate boundary;
//! raise paths stay cold and out of line.

use core::cell::Cell;

use bytecode::Opcode;

use crate::heap::{Heap, Register};
use crate::stack::{FrameMeta, Stack};
use crate::value::{Tagged, Value};
use crate::{
    Args, FeedbackVector, FixedArray, HostCtx, LanguageMode, NativeIndex, ThreadState, VM, VmError,
    Word, raise_runtime,
};

/// Loop back-edge ticks between safepoint polls.
pub const SAFEPOINT_INTERVAL: u32 = 1 << 12;

/// The frame's register window: signed indices are anchor-relative
/// slot offsets (negative = locals below the frame header, positive =
/// parameters above it, receiver = 0).
#[derive(Clone, Copy)]
pub struct Regs(*mut Register);

impl Regs {
    /// # Safety
    /// `base` is a rooted window of the running frame; no GC before the
    /// last use of this handle or anything loaded through it.
    #[inline(always)]
    pub unsafe fn new(base: *mut Register) -> Regs {
        Regs(base)
    }

    /// Safe by the register-file invariant: slots hold strong, valid
    /// values from frame setup until the frame pops (the collector
    /// updates them in place). The `&Heap` argument anchors the returned
    /// value to the GC epoch, like `Register::get(heap)`.
    #[inline(always)]
    pub fn get<'h>(&self, i: i32, heap: &'h Heap) -> Tagged<'h, Value> {
        let _ = heap;
        // Safety: the invariant above.
        unsafe { Tagged::from_value_unchecked((*self.0.offset(i as isize)).raw()) }
    }

    #[inline(always)]
    pub fn set(&self, i: i32, v: Tagged<'_, Value>) {
        // Safety: the window invariant covers this slot.
        unsafe {
            (*self.0.offset(i as isize)).store(v);
        }
    }
}

/// A const-stride operand cursor over one instruction's operand stream.
/// `STRIDE` is 1 for narrow instructions and 2 for `Wide`-prefixed ones;
/// for wide instructions `ip` points at the `Wide` prefix.
pub struct Ops<const STRIDE: usize> {
    ip: *const u8,
    #[cfg(debug_assertions)]
    op: Opcode,
}

impl<const STRIDE: usize> Ops<STRIDE> {
    #[inline(always)]
    pub fn new(ip: *const u8, op: Opcode) -> Ops<STRIDE> {
        debug_assert_eq!(
            if STRIDE == 2 {
                unsafe { *ip.add(1) }
            } else {
                unsafe { *ip }
            },
            op as u8,
            "handler dispatched for the wrong opcode",
        );
        Ops {
            ip,
            #[cfg(debug_assertions)]
            op,
        }
    }

    #[inline(always)]
    pub fn signed<const I: usize>(&self) -> i32 {
        #[cfg(debug_assertions)]
        debug_assert!(
            I < self.op.operands().len() && self.op.operands()[I].is_signed(),
            "operand {I} of {:?} is not a signed operand",
            self.op
        );
        let off = base::<STRIDE>() + I * STRIDE;
        unsafe {
            if STRIDE == 1 {
                *self.ip.add(off) as i8 as i32
            } else {
                i16::from_le_bytes([*self.ip.add(off), *self.ip.add(off + 1)]) as i32
            }
        }
    }

    #[inline(always)]
    pub fn unsigned<const I: usize>(&self) -> usize {
        #[cfg(debug_assertions)]
        debug_assert!(
            I < self.op.operands().len() && !self.op.operands()[I].is_signed(),
            "operand {I} of {:?} is not an unsigned operand",
            self.op
        );
        let off = base::<STRIDE>() + I * STRIDE;
        unsafe {
            if STRIDE == 1 {
                *self.ip.add(off) as usize
            } else {
                u16::from_le_bytes([*self.ip.add(off), *self.ip.add(off + 1)]) as usize
            }
        }
    }
}

#[inline(always)]
pub const fn base<const STRIDE: usize>() -> usize {
    if STRIDE == 2 { 2 } else { 1 }
}

/// The interpreter accumulator carried through dispatch.
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

pub struct Ctx<'s, 'h> {
    vm: &'s VM,
    state: &'s ThreadState,
    heap: &'h mut Heap,
    /// Anchor of the frame `execute` entered
    base_anchor: usize,
    /// Data pointer of the current frame's bytecode (`ip - code`)
    code: Cell<*const u8>,
    safepoints: Cell<u32>,
}

impl<'s, 'h> Ctx<'s, 'h> {
    #[inline(always)]
    pub fn new(vm: &'s VM, state: &'s ThreadState, heap: &'h mut Heap, base_anchor: usize) -> Self {
        let code = state
            .stack()
            .frame_header(base_anchor)
            .code(&*heap)
            .as_ptr();
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

    /// Shared access to the heap for the match-loop interpreter's cold
    /// helpers, which hold a heap reference while calling other `Ctx`
    /// methods. This is transitional: once those call paths use exclusive
    /// `&mut Ctx` reborrows, this accessor should be deleted.
    ///
    /// # Safety
    /// The caller must uphold the interpreter's heap-access discipline;
    /// this exists only for the match-loop shell's shared cold helpers.
    #[inline(always)]
    pub unsafe fn heap_shared(&self) -> &'h Heap {
        let ptr = &*self.heap as *const Heap;
        unsafe { &*ptr }
    }

    /// Shared access to the heap for the match-loop interpreter's cold
    /// helpers, which hold a heap reference while calling other `Ctx`
    /// methods. This is transitional: once those call paths use exclusive
    /// `&mut Ctx` reborrows, this accessor should be deleted.
    ///
    /// # Safety
    /// The caller must uphold the interpreter's heap-access discipline;
    /// this exists only for the match-loop shell's shared cold helpers.
    #[inline(always)]
    pub unsafe fn heap_mut_shared(&self) -> &'h mut Heap {
        let ptr = core::ptr::addr_of!(*self.heap).cast_mut();
        unsafe { &mut *ptr }
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
    pub fn stack(&self) -> &'s Stack {
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
        let state = self.state;
        let heap = &*self.heap;
        let code = state.stack().frame_header(state.frame_base()).code(heap);
        self.code.set(code.as_ptr());
    }

    /// Switch to the frame at `base` (pop/unwind)
    #[inline(always)]
    pub fn switch_frame(&self, base: usize) {
        let state = self.state;
        let heap = &*self.heap;
        let code = state.stack().frame_header(base).code(heap);
        self.state.set_frame_base(base);
        self.code.set(code.as_ptr());
    }

    /// Switch to a freshly pushed frame, whose pc origin the caller
    /// already read from the callee's info.
    #[inline(always)]
    pub fn switch_frame_code(&self, base: usize, code: *const u8) {
        self.state.set_frame_base(base);
        self.code.set(code);
    }

    /// The current frame's language mode, read from its callable info.
    /// Only mode-sensitive handlers (delete, store slow paths) call this;
    /// the hot store fast paths never consult it.
    #[inline(always)]
    pub fn language_mode(&self) -> LanguageMode {
        debug_assert!(
            self.state.is_frame_active(),
            "language mode read without an active frame"
        );
        let heap: &Heap = self.heap;
        let header = self.state.stack().frame_header(self.state.frame_base());
        let callable = header.callable(heap);
        let info = callable
            .callable_info(heap)
            .expect("a bytecode frame's callable must have callable info");
        info.mode()
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

/// The exception-dispatch result: a handler was found (its entry pc
/// rides in `Caught` along with the exception, the current frame is the
/// handler's) or the exception escaped this execution.
pub enum Unwind<'h> {
    Caught { pc: usize, ex: Tagged<'h, Value> },
    Escaped,
}

/// Search the handler tables from the faulting frame outward, popping
/// frames until a catch handler covers `fault_pc` or the execution's
/// base anchor escapes. On `Caught` the pending exception is consumed
/// and the current frame is the handler's.
#[cold]
#[inline(never)]
pub fn unwind<'h>(ctx: &Ctx<'_, 'h>, fault_pc: usize) -> Unwind<'h> {
    // Safety: unwind only reads the heap.
    let heap = unsafe { ctx.heap_shared() };
    let mut pc = fault_pc;
    loop {
        let base = ctx.frame_base();
        let handled = if ctx.state().termination().is_some() {
            // A termination is not an exception: no handler (catch or
            // finally in any frame) may observe or intercept it.
            None
        } else {
            let callable = ctx.stack().frame_header(base).callable_slot().get(heap);
            callable
                .as_heap_object()
                .and_then(|obj| obj.callable_info(heap))
                .and_then(|info| info.handlers.get(heap))
                .and_then(|handlers| handlers.lookup(pc))
        };
        if let Some(handler_pc) = handled {
            let ex = ctx
                .state()
                .take_pending_exception_tagged(heap)
                .expect("pending exception must be set while unwinding");
            return Unwind::Caught { pc: handler_pc, ex };
        }
        if base == ctx.base_anchor() {
            return Unwind::Escaped;
        }
        let caller = ctx.stack().pop_frame(base);
        ctx.set_frame_base(caller.base);
        pc = caller.handler_pc;
    }
}
