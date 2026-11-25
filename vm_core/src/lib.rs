use crate::continuation::Continuation;
use crate::threading::{ThreadRegistry, create_thread_object};
use core::{
    cell::{Cell as StdCell, UnsafeCell},
    ptr::NonNull,
};
use std::time::Duration;
use std::{
    sync::Arc, sync::Condvar, sync::Mutex, sync::Weak, sync::atomic::AtomicBool,
    sync::atomic::AtomicI32, sync::atomic::Ordering,
};

pub mod access;
pub mod api;
pub mod bootstrap;
pub mod compare;
pub mod continuation;
pub mod tsan;

/// Native stack reserved for one task.
/// TODO: make this dynamic
pub const TASK_STACK_SIZE: usize = 2 * 1024 * 1024;
pub mod bit_program;
pub mod convert;
pub mod errors;
pub mod ffi;
pub mod handle;
pub mod heap;
pub mod ic;
pub mod interner;
pub mod interpreter;
pub mod intrinsics;
pub mod json;
pub mod language;
pub mod lookup;
pub mod materialize;
pub mod native;
pub mod objects;
pub mod prototype;
pub mod stack;
pub mod threading;
pub mod tools;
pub mod transition;
pub mod value;

pub use objects::object::SetApplied;
pub use objects::proxy;
pub use objects::script_context::{eval_global_context, global_lexical_get, global_lexical_set};
pub use objects::string;

pub use ffi::FfiState;

pub use access::{OwnProperty, OwnPropertyLocation, ShouldThrow, StoreSemantics};
pub use api::{EntryFn, ErasedRuntimeState, Interpreter, Runtime, RuntimeId};
pub use bootstrap::{
    WellKnown, WellKnownStrings, bootstrap_basics, bootstrap_well_known, intern_well_known_strings,
    smi_slot,
};
pub use compare::Compare;
pub use convert::{Convert, Hint};
pub use errors::{ErrorKind, Errors, VmError};
pub use handle::{EscapableHandleScope, Handle, HandleData, HandleScope, HandleSlice};

/// Marker error for VM operations that throw guest code exceptions.
///
/// `Err(Thrown)` always means `ThreadState::has_pending_exception()` is
/// true. The exception value itself lives in the thread's rooted
/// pending-exception register, not in this marker.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Thrown;

/// A host-side runtime installation failure.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct SetupError;

/// VM operation result.
///
/// `Err(Thrown)` means an exception is pending on the executing thread.
/// `Ok(None)` means a normal absence and never an error. Functions that
/// cannot throw should return their value directly.
pub type Result<T, E = Thrown> = core::result::Result<T, E>;
pub use heap::{
    AllocToken, AtomicGcSlot, AtomicMaybeWeakGcSlot, AtomicOptionGcSlot, EdgeVisitable, GcSlot,
    GlobalHeap, Heap, MaybeWeakGcSlot, OptionGcSlot, Register, WordType,
};
pub use interner::StringInterner;
pub use interpreter::{Acc, Ctx, Ops, Regs, Unwind};
pub use language::{LanguageMode, MODE_SLOPPY, MODE_STRICT, mode_from_strict};

pub use intrinsics::VmCore;
pub use native::{
    CallInfo, HostCtx, NativeFn, NativeIndex, NativeRegistry, frame_context_value, raise_runtime,
};
pub use objects::{
    AccessorInfo, AccessorInfoInit, AccessorPair, ArgumentsCreate, Atom, AtomicI64Object,
    AtomicI64ObjectInit, BitArray, BitArrayInit, CValue, CValueFlags, CValueInit, CallTarget,
    CallableInfoInit, CallableInfoObject, Cell, CellInit, Code, CodeInit, ContextInit,
    ContextObject, DETAILS_ACCESSOR, DETAILS_CONFIGURABLE, DETAILS_DATA, DETAILS_ENUMERABLE,
    DETAILS_WRITABLE, DataHandler, DataHandlerInit, DenseString, ElementsKind, Encoding,
    ExternalString, ExternalStringInit, FeedbackVector, FeedbackVectorInit, FixedArray,
    FixedByteArray, Float, ForeignLibrary, ForeignLibraryInit, FunctionKind, GlobalLexicalBinding,
    HandlerEntry, HandlerEntryInit, HandlerTable, HandlerTableInit, Header, HeapObject,
    INFO_GLOBAL, INFO_MODE_MASK, Key, MODE_CLASS, MODE_CONST, MODE_LET, MODE_VAR, Map, MapInit,
    MapKind, NameToIndexHashTable, NameToIndexHashTableInit, NumberDictionary,
    NumberDictionaryInit, Object, ObjectInit, ObjectKind, ObjectSlotsInit, PrototypeInfo,
    ProxyInit, ProxyObject, Range, RangeInit, SCOPE_DECLARATION, SCOPE_REPL, SCOPE_SCRIPT,
    ScopeInfo, ScopeInfoInit, ScriptContextTable, ScriptContextTableInit, SiteRead, SlotDescriptor,
    SlotFlags, SlotName, StringData, StringOwn, StringRef, Symbol, ThreadKind, ThreadObject,
    ThreadObjectInit, ThreadStatus, TypeInfo, TypeInfoInit, Utf8String, WeakFixedArray,
    WeakFixedArrayInit, WrapperKind, decode_wtf8, flatten, info_is_immutable, info_is_lexical,
    info_mode, object_kind, object_layout, string_content_hash, string_content_hash_units,
    visit_object,
};
pub use prototype::{Prototype, PrototypeRegistry, ShapeChange};
pub use stack::{Args, Callee, FrameMeta, RUST_STACK_BUDGET, STACK_SLOTS, Stack};
pub use tools::{KetteTools, Termination, request_shutdown};
pub use transition::{Change, PartialDescriptor, PropertyDescriptor, SiblingChange, Transition};
pub use value::{
    MaybeWeak, MaybeWeakStore, PTR_BIT, STRONG_PTR, Smi, TAG_MASK, TAG_SMI, Tagged, Value,
    WEAK_BIT, WEAK_PTR, Word,
};

pub use heap_api::{
    AllocError, CellRef, GcHost, HeapBackend, HeapPtr, HeapStats, LocalHeap, ObjectAddr, RawCell,
    SharedHeap, Tlab, Visitor,
};

pub type Local<'scope, T> = Handle<'scope, T>;

/// A bytecode frontend, selectable by type at the call site
/// (`vm.eval::<JavascriptCompiler>(..)`).
pub trait Compiler {
    /// Parse and lower `source` for `vm`: the native-function ids the code
    /// emits (`CallNative`) come from the VM's native registry.
    fn compile(
        vm: &VM,
        source: &str,
        mode: bytecode::SourceMode,
    ) -> Result<bytecode::Program, bytecode::FrontendError>;

    /// The runtime this front end's programs belong to.
    const RUNTIME: RuntimeId;
}

/// A source language registered for indirect eval via `KetteTools.eval`.
pub struct EvalLanguage {
    pub name: &'static str,
    pub compile:
        fn(&VM, &str, bytecode::SourceMode) -> Result<bytecode::Program, bytecode::FrontendError>,
    pub runtime: RuntimeId,
}

/// Number of slots per handle block.
pub const HANDLE_BLOCK_SIZE: usize = 1024;

const _: () = {
    use core::mem::size_of;
    assert!(size_of::<Value>() == size_of::<Word>());
    assert!(size_of::<Tagged<Value>>() == size_of::<Word>());
    assert!(size_of::<Tagged<DenseString>>() == size_of::<Word>());
    assert!(size_of::<GcSlot>() == size_of::<Word>());
    assert!(size_of::<MaybeWeakGcSlot>() == size_of::<Word>());
    assert!(size_of::<OptionGcSlot<FixedArray>>() == size_of::<Word>());
    assert!(size_of::<GcSlot<Smi>>() == size_of::<Word>());
    assert!(size_of::<GcSlot<DenseString>>() == size_of::<Word>());
    assert!(size_of::<Register>() == size_of::<Word>());
    // SlotName is a newtype over the erased name word; names travel as
    // `Tagged<'_, SlotName>`
    assert!(size_of::<SlotName>() == size_of::<Word>());
};

pub struct SharedVM {
    heap: GlobalHeap,
    /// Core C FFI: type table, call plans, callbacks.
    ffi: FfiState,
    known: WellKnown,
    /// Live thread states, id allocation, and the per-OS-thread attach cache.
    threads: ThreadRegistry,
    interner: StringInterner,
    prototype_registry: PrototypeRegistry,
    natives: NativeRegistry,
    /// Installed runtimes in `add` order; a [`RuntimeId`] indexes this.
    runtimes: Vec<Box<dyn ErasedRuntimeState>>,
    /// Source languages available to `KetteTools.eval`.
    eval_languages: Vec<EvalLanguage>,
    /// Weak slots the GC clears when their targets die; used for tests and
    /// the seed of a weak-registry feature.
    weak_slots: Mutex<Vec<RawCell>>,
    /// Informational only: set once `KetteTools.shutdown()` ran. Cancels
    /// live on the safepoint nodes, not here — the VM stays usable after a
    /// shutdown.
    shutdown_requested: AtomicBool,
    /// Process exit status reported by `os.exit`; `0` unless a program asked
    /// to exit with another code.
    exit_code: AtomicI32,
    /// Wakes host-blocked mutators (`join`, `sleep`) on thread lifecycle
    /// events and shutdown.
    thread_event_lock: Mutex<()>,
    thread_event: Condvar,
    /// Interpreter entry ([[Call]]/[[Construct]] on bytecode callables);
    /// set once at construction from the `I: Interpreter` type parameter.
    execute: EntryFn,
}

pub struct VM {
    shared: Arc<SharedVM>,
}

/// First-run data carried into a task's stack.
pub struct TaskRun {
    vm: VM,
    state: *const ThreadState,
    callable: Value,
    args: Vec<Value>,
    runtime: RuntimeId,
    outcome: Option<Result<Value, Thrown>>,
}

#[derive(Default)]
struct ParkState {
    permit: bool,
    blocked: bool,
    /// Set by the carrier once the continuation has actually suspended.
    parked: bool,
    timed_out: bool,
}

pub struct TaskStart {
    callable: Value,
    args: Vec<Value>,
    runtime: RuntimeId,
}

pub enum ResumeOutcome {
    Suspended,
    Finished(Result<Value, Thrown>),
}

pub fn prepare_task(state: &ThreadState, vm: &VM, start: TaskStart) -> *mut TaskRun {
    if state.is_continuation_started() {
        return state.task_run();
    }
    let run = Box::new(TaskRun {
        vm: vm.clone(),
        state: state as *const ThreadState,
        callable: start.callable,
        args: start.args,
        runtime: start.runtime,
        outcome: None,
    });
    let run = Box::into_raw(run);
    state.set_task_run(run);
    state.start_continuation(run as *mut ());
    run
}

pub fn task_outcome(state: &ThreadState) -> Option<Result<Value, Thrown>> {
    let run = state.task_run();
    if run.is_null() {
        return None;
    }
    // Safety: the run is alive while the task is started.
    unsafe { (*run).outcome.take() }
}

pub fn release_task_run(state: &ThreadState) {
    let run = state.take_task_run();
    if !run.is_null() {
        // Safety: the task is finished and will not resume.
        unsafe { drop(Box::from_raw(run)) };
    }
    state.release_continuation();
}

#[unsafe(no_mangle)]
extern "C" fn kette_task_entry(arg: *mut ()) -> ! {
    let run = unsafe { &mut *(arg as *mut TaskRun) };
    let state = unsafe { &*run.state };
    // The mounter swapped the carrier's allocator into our stable view.
    let heap = state.task_heap();
    let outcome = state.handle_scope(|scope| {
        // Safety: the callable and arguments are rooted by the caller or
        // by the task's thread object; no collection ran since they were
        // copied.
        let callable = unsafe { Tagged::<Value>::from_value_unchecked(run.callable) };
        let Some(callable) = callable.get_as::<Object>(heap) else {
            return Err(Thrown);
        };
        let callable = scope.handle(callable);
        let args = scope.stage(
            &run.args
                .iter()
                .map(|value| unsafe { Tagged::<Value>::from_value_unchecked(*value) })
                .collect::<Vec<_>>(),
        );
        let runtime = run.vm.runtime_by_id(run.runtime);
        run.vm
            .execute(runtime, heap, state, callable, args, None)
            .map(|value| value.raw())
    });
    run.outcome = Some(if state.termination().is_some() {
        Ok(run.vm.known().undefined.get(heap).raw())
    } else {
        outcome
    });
    // Safety: the carrier continuation is the active target.
    tsan::switch_to(state.resume_fiber());
    unsafe { continuation::switch(state.continuation_ptr(), state.resume_target()) };
    unreachable!()
}

/// Parking, join and waiter synchronization for one task.
#[derive(Default)]
struct SyncState {
    finished: Mutex<bool>,
    finished_event: Condvar,
    /// Tasks blocked on this thread's join.
    waiters: Mutex<Vec<AtomicOptionGcSlot<ThreadObject>>>,
    /// Parker state: one permit bit plus whether the task is parked.
    park_state: Mutex<ParkState>,
    park_event: Condvar,
}

impl Drop for ThreadState {
    fn drop(&mut self) {
        tsan::destroy_fiber(self.fiber);
    }
}

pub struct ThreadState {
    handles: HandleData,
    stack: Stack,
    /// Saved registers and stack of this task.
    continuation: UnsafeCell<continuation::Continuation>,
    continuation_started: StdCell<bool>,
    /// Continuation resumed by `suspend`.
    resume_target: StdCell<*const continuation::Continuation>,
    /// Whether the resume target is another task (guest mounter).
    mounted_by_task: StdCell<bool>,
    /// First-run data while the task is alive.
    task_run: StdCell<*mut TaskRun>,
    /// The current (innermost) interpreter frame's anchor. Frames are
    /// self-describing (see [`Stack`]): this cell is the only shadow
    /// state, written by push/pop/unwind frame switches.
    frame_base: StdCell<usize>,
    /// Whether an interpreted frame is current (natives inspecting the
    /// running frame check this).
    frame_active: StdCell<bool>,
    /// The accumulator cell: rooted, the GC updates it in place.
    acc: Register,
    pending_exception: Register,
    has_pending_exception: StdCell<bool>,
    /// A guest callback threw during an FFI call on this thread; the FFI
    /// trampoline re-raises once the C call has returned.
    callback_threw: StdCell<bool>,
    /// Why this thread's execution terminated, if it did. Set exactly once,
    /// right before the uncatchable unwind; guest code can never observe or
    /// intercept it.
    termination: StdCell<Option<Termination>>,
    /// Rust stack position of the bottom-most `execute` (0 before the first
    /// call): the anchor for [`ThreadState::rust_stack_exhausted`].
    rust_stack_base: StdCell<usize>,
    /// Construct-receiver cache: a `WeakFixedArray` table mapping closures
    /// to their synthesized initial map identity-keyed per closure since `.prototype` is per-closure.
    /// Entries are `[closure(weak), fn_map, proto, proto_slot(Smi),
    /// initial_map]`; a closure's entry is valid while its map and the
    /// `.prototype` slot value compare equal. The hole = not yet allocated.
    construct_cache: Register,
    /// The guest `Thread` object mounted on this execution context.
    thread_object: Register,
    /// Stable heap view for this task. Its allocator is swapped with the
    /// running carrier around every mount, so captured `&mut Heap` values
    /// stay valid across migration.
    task_heap: UnsafeCell<Heap>,
    /// TSan logical-stack token for this task (null without `tsan`).
    fiber: *mut core::ffi::c_void,
    /// TSan fiber to return to when the task suspends or finishes.
    resume_fiber: StdCell<*mut core::ffi::c_void>,
    /// Debug guard: a task may be mounted by at most one carrier.
    mounting: AtomicBool,
    id: u64,
    sync: SyncState,
}

impl ThreadState {
    pub fn new(heap: &Heap, id: u64) -> Self {
        // Safety: root-slot reads stored straight into rooted fill cells.
        let the_hole = heap.known().the_hole.get(heap).erase();
        let undefined = heap.known().undefined.get(heap).erase();
        Self {
            id,
            sync: SyncState::default(),
            handles: HandleData::new(the_hole),
            stack: Stack::new(STACK_SLOTS, undefined),
            continuation: UnsafeCell::new(continuation::Continuation::zeroed()),
            continuation_started: StdCell::new(false),
            resume_target: StdCell::new(core::ptr::null()),
            mounted_by_task: StdCell::new(false),
            task_run: StdCell::new(core::ptr::null_mut()),
            frame_base: StdCell::new(0),
            frame_active: StdCell::new(false),
            acc: unsafe { Register::from_value(undefined) },
            pending_exception: unsafe { Register::from_value(the_hole) },
            has_pending_exception: StdCell::new(false),
            callback_threw: StdCell::new(false),
            termination: StdCell::new(None),
            rust_stack_base: StdCell::new(0),
            construct_cache: unsafe { Register::from_value(the_hole) },
            thread_object: unsafe { Register::from_value(the_hole) },
            task_heap: UnsafeCell::new(heap.task_view()),
            fiber: tsan::create_fiber(),
            resume_fiber: StdCell::new(core::ptr::null_mut()),
            mounting: AtomicBool::new(false),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn mark_finished(&self) {
        *self.sync.finished.lock().unwrap() = true;
        self.sync.finished_event.notify_all();
    }

    pub fn wait_finished(&self, heap: &mut Heap) {
        let mut finished = self.sync.finished.lock().unwrap();
        while !*finished {
            finished = heap.block_on(|| self.sync.finished_event.wait(finished).unwrap());
        }
    }

    #[inline(always)]
    pub fn stack(&self) -> &Stack {
        &self.stack
    }

    #[inline(always)]
    pub fn continuation_ptr(&self) -> *mut continuation::Continuation {
        self.continuation.get()
    }

    #[inline(always)]
    pub fn is_continuation_started(&self) -> bool {
        self.continuation_started.get()
    }

    pub fn start_continuation(&self, arg: *mut ()) {
        let continuation = unsafe { &mut *self.continuation.get() };
        if continuation.stack_size() == 0 {
            let Some((base, size)) = continuation::allocate_stack(TASK_STACK_SIZE) else {
                panic!("cannot allocate task stack");
            };
            continuation.prepare(base, size, arg);
            self.rust_stack_base.set(base as usize + size);
        } else {
            continuation.prepare(continuation.stack_base(), continuation.stack_size(), arg);
        }
        self.continuation_started.set(true);
    }

    pub fn release_continuation(&self) {
        let continuation = unsafe { &mut *self.continuation.get() };
        continuation::free_stack(continuation.stack_base(), continuation.stack_size());
        continuation.reset();
        self.continuation_started.set(false);
        self.rust_stack_base.set(0);
    }

    #[inline(always)]
    pub fn set_resume_target(&self, target: *const continuation::Continuation, by_task: bool) {
        self.resume_target.set(target);
        self.mounted_by_task.set(by_task);
    }

    #[inline(always)]
    pub fn resume_target(&self) -> *const continuation::Continuation {
        self.resume_target.get()
    }

    #[inline(always)]
    pub fn is_mounted_by_task(&self) -> bool {
        self.mounted_by_task.get()
    }

    /// The task's stable heap view. Only the carrier currently running the
    /// task may touch it.
    #[inline(always)]
    pub fn task_heap(&self) -> &mut Heap {
        // Safety: the task runs on one carrier at a time (`mounting`).
        unsafe { &mut *self.task_heap.get() }
    }

    /// Run the raw continuation switch with this task's allocator installed,
    /// then give the allocator back to the carrier. `switch` must perform the
    /// continuation switch itself.
    pub fn run_on_heap<R>(&self, carrier: &mut Heap, switch: impl FnOnce() -> R) -> R {
        {
            let task_heap = self.task_heap();
            carrier.swap_allocator(task_heap);
        }
        tsan::switch_to(self.fiber());
        let result = switch();
        {
            let task_heap = self.task_heap();
            carrier.swap_allocator(task_heap);
        }
        result
    }

    #[inline(always)]
    pub fn fiber(&self) -> *mut core::ffi::c_void {
        self.fiber
    }

    #[inline(always)]
    pub fn resume_fiber(&self) -> *mut core::ffi::c_void {
        self.resume_fiber.get()
    }

    #[inline(always)]
    pub fn set_resume_fiber(&self, fiber: *mut core::ffi::c_void) {
        self.resume_fiber.set(fiber);
    }

    /// Claim the single mount slot. Returns false on a concurrent mount.
    #[inline(always)]
    pub fn begin_mount(&self) -> bool {
        self.mounting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[inline(always)]
    pub fn end_mount(&self) {
        self.mounting.store(false, Ordering::Release);
    }

    /// Called by the carrier after the continuation yielded. Publishes the
    /// park and returns true when this carrier owns resubmission (yield, or a
    /// permit that arrived while still parking). A fully parked task is left
    /// to its waker.
    pub fn after_mount(&self) -> bool {
        let mut park = self.sync.park_state.lock().unwrap();
        if !park.blocked {
            // Yielded (or finished): the mounting carrier owns resubmission.
            return true;
        }
        park.parked = true;
        if park.permit {
            park.permit = false;
            park.blocked = false;
            park.parked = false;
            true
        } else {
            false
        }
    }

    /// Park the current task. `unpark` either leaves a permit or wakes it.
    pub fn park(&self, heap: &mut Heap) {
        let Some(thread) = self
            .thread_object(heap)
            .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
        else {
            return;
        };
        if !self.is_continuation_started() {
            return;
        }
        if !thread.as_ref().is_virtual(heap) {
            let thread = thread.raw();
            self.park_platform(heap, thread, None);
            return;
        }
        {
            let mut park = self.sync.park_state.lock().unwrap();
            park.timed_out = false;
            park.parked = false;
            if park.permit {
                park.permit = false;
                return;
            }
            park.blocked = true;
            thread.as_ref().set_state(ThreadStatus::Blocked);
        }
        self.suspend();
        let mut park = self.sync.park_state.lock().unwrap();
        park.blocked = false;
        park.parked = false;
        park.permit = false;
        park.timed_out = false;
    }

    pub fn park_timed(&self, heap: &mut Heap, duration: Duration) -> bool {
        let Some(thread) = self
            .thread_object(heap)
            .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
        else {
            return false;
        };
        if !self.is_continuation_started() {
            return false;
        }
        if !thread.as_ref().is_virtual(heap) {
            let thread = thread.raw();
            return self.park_platform(heap, thread, Some(duration));
        }
        let mut park = self.sync.park_state.lock().unwrap();
        park.timed_out = false;
        if park.permit {
            park.permit = false;
            return false;
        }
        if duration.is_zero() {
            park.timed_out = true;
            return true;
        }
        drop(park);
        std::thread::sleep(duration);
        true
    }

    fn park_platform(&self, heap: &mut Heap, thread: Value, timeout: Option<Duration>) -> bool {
        let thread = unsafe { Tagged::<ThreadObject>::from_value_unchecked(thread) };
        let mut park = self.sync.park_state.lock().unwrap();
        park.timed_out = false;
        if park.permit {
            park.permit = false;
            return false;
        }
        park.blocked = true;
        park.parked = true;
        thread.as_ref().set_state(ThreadStatus::Blocked);
        let timed_out = match timeout {
            Some(duration) => heap.block_on(|| {
                let (guard, result) = self.sync.park_event.wait_timeout(park, duration).unwrap();
                let outcome = result.timed_out() || guard.timed_out;
                (guard, outcome)
            }),
            None => heap.block_on(|| {
                let mut park = park;
                while park.blocked {
                    park = self.sync.park_event.wait(park).unwrap();
                }
                (park, false)
            }),
        };
        let (mut park, timed_out) = match timed_out {
            (park, timed_out) => (park, timed_out),
        };
        park.blocked = false;
        park.parked = false;
        park.timed_out = false;
        park.permit = false;
        timed_out
    }

    /// Returns true when a parked task must be made runnable. A request that
    /// arrives while the task is still parking only leaves a permit; the
    /// carrier resubmits it after the yield (see `after_mount`).
    pub fn unpark(&self) -> bool {
        let mut park = self.sync.park_state.lock().unwrap();
        let woke = if park.blocked && park.parked {
            park.blocked = false;
            park.parked = false;
            park.timed_out = false;
            true
        } else {
            park.permit = true;
            false
        };
        drop(park);
        self.sync.park_event.notify_one();
        woke
    }

    pub fn is_park_blocked(&self) -> bool {
        self.sync.park_state.lock().unwrap().blocked
    }

    pub fn add_waiter(&self, heap: &Heap, waiter: Tagged<'_, ThreadObject>) {
        // The queue lives off-heap and is scanned from `visit_edges` every
        // collection, so its slots need no generational barrier.
        let slot = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
        slot.store(waiter);
        self.sync.waiters.lock().unwrap().push(slot);
    }

    pub fn take_waiter_slots(&self) -> Vec<AtomicOptionGcSlot<ThreadObject>> {
        core::mem::take(&mut *self.sync.waiters.lock().unwrap())
    }

    pub fn remove_waiter(&self, heap: &Heap, waiter: Tagged<'_, ThreadObject>) {
        let raw = waiter.raw().to_bits();
        let mut waiters = self.sync.waiters.lock().unwrap();
        let mut i = 0;
        while i < waiters.len() {
            if waiters[i].load_word(heap) == raw {
                waiters.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    #[inline(always)]
    pub fn task_run(&self) -> *mut TaskRun {
        self.task_run.get()
    }

    #[inline(always)]
    pub fn set_task_run(&self, run: *mut TaskRun) {
        self.task_run.set(run);
    }

    #[inline(always)]
    pub fn take_task_run(&self) -> *mut TaskRun {
        self.task_run.replace(core::ptr::null_mut())
    }

    /// Suspend the current task until the carrier resumes it.
    #[inline]
    pub fn suspend(&self) {
        let target = self.resume_target();
        debug_assert!(!target.is_null(), "suspend without a resume target");
        // Safety: the target is the active switch target and the task has
        // a prepared continuation.
        tsan::switch_to(self.resume_fiber());
        unsafe { continuation::switch(self.continuation_ptr(), target) };
    }

    #[inline(always)]
    pub fn frame_base(&self) -> usize {
        self.frame_base.get()
    }

    #[inline(always)]
    pub fn set_frame_base(&self, base: usize) {
        self.frame_base.set(base);
    }

    #[inline]
    pub fn runtime<'a>(&self, vm: &'a VM, heap: &Heap) -> &'a dyn ErasedRuntimeState {
        debug_assert!(self.is_frame_active(), "no frame");
        let code = self.stack().frame_header(self.frame_base()).code(heap);
        vm.runtime_by_id(code.as_ref().runtime())
    }

    #[inline(always)]
    pub fn is_frame_active(&self) -> bool {
        self.frame_active.get()
    }

    #[inline(always)]
    pub fn set_frame_active(&self, active: bool) {
        self.frame_active.set(active);
    }

    /// The accumulator cell (see the field docs).
    pub fn acc_slot(&self) -> &Register {
        &self.acc
    }

    #[inline(always)]
    pub fn construct_initial_map<'a>(
        &self,
        heap: &'a Heap,
        closure: Tagged<'_, Object>,
    ) -> Option<Tagged<'a, Map>> {
        let word = self.construct_cache.get(heap);
        if word.raw() == heap.known().the_hole.get(heap).raw() {
            return None;
        }
        let table = word.get_as::<WeakFixedArray>(heap)?;
        let t = table;
        let mask = t.len() / 5 - 1;
        let mut i = ((closure.raw().to_bits() >> 5) as usize) & mask;
        for _ in 0..=mask {
            let base = i * 5;
            let entry = t.get(heap, base);
            let slot = t.element_slot(base);
            if entry.raw().is_weak_ptr() && !slot.is_cleared() {
                if entry.raw().to_bits() == closure.raw().to_bits() | WEAK_PTR {
                    // validate: same function map, same `.prototype` slot
                    let fn_map = t.get(heap, base + 1);
                    let proto = t.get(heap, base + 2);
                    let offset = Smi::decode(t.get(heap, base + 3).raw())?.value() as usize;
                    let map_now = closure.map(heap);
                    if fn_map.raw() != map_now.raw() {
                        return None;
                    }
                    if closure.slot(heap, offset).raw() != proto.raw() {
                        return None;
                    }
                    // The initial map is written only by
                    // `construct_cache_insert`, into a strong slot.
                    let initial = t.get(heap, base + 4).as_strong()?.get_as::<Map>(heap)?;
                    return Some(initial);
                }
                // dead entry: keep probing
            } else if entry.raw() == heap.known().the_hole.get(heap).raw() {
                return None; // reached an empty slot: absent
            } else {
                return None; // strong non-hole: not our layout — bail
            }
            i = (i + 1) & mask;
        }
        None
    }

    /// Ensure the weak `closure -> initial_map` table exists. Callers
    /// that capture raw heap words across later allocations must call this
    /// first: this is the only allocation a cache fill may trigger, and a
    /// collection here would leave captured words stale.
    pub fn construct_cache_ensure_table(&self, heap: &mut Heap) {
        let the_hole = heap.known().the_hole.get(heap).raw();
        if self.construct_cache.get(heap).raw() != the_hole {
            return;
        }
        self.handle_scope(|_scope| {
            let table = heap.allocate_hole_weak_array(320);
            self.construct_cache.store(table.erase());
        });
    }

    /// Record `closure -> initial_map` (miss path; `proto_slot` is the
    /// object-slot offset of the closure's `.prototype` data property).
    /// The table must already exist ([`Self::construct_cache_ensure_table`]).
    pub fn construct_cache_insert(
        &self,
        heap: &Heap,
        closure: Tagged<'_, Object>,
        proto: Tagged<'_, Value>,
        proto_slot: usize,
        initial_map: Tagged<'_, Map>,
    ) {
        let the_hole = heap.known().the_hole.get(heap).raw();
        let word = self.construct_cache.get(heap);
        debug_assert_ne!(word.raw(), the_hole, "cache table must be ensured");
        let table = word
            .get_as::<WeakFixedArray>(heap)
            .expect("construct cache slot holds the table");
        let mask = table.len() / 5 - 1;
        let fn_map = closure.map(heap);
        let mut i = ((closure.raw().to_bits() >> 5) as usize) & mask;
        for _ in 0..=mask {
            let base = i * 5;
            let entry = table.get(heap, base);
            let slot = table.element_slot(base);
            let free = entry.raw() == the_hole || (entry.raw().is_weak_ptr() && slot.is_cleared());
            if free {
                table.set(heap, base, closure.erase().as_weak());
                table.set(heap, base + 1, fn_map.erase());
                table.set(heap, base + 2, proto);
                table.set(heap, base + 3, Smi::new(proto_slot as i64).into_tagged());
                table.set(heap, base + 4, initial_map.erase());
                return;
            }
            i = (i + 1) & mask;
        }
        // full: drop inserts (rare — 64 constructor kinds)
    }

    pub fn set_pending_exception<'x, T: 'x>(&self, value: Tagged<'x, T>) {
        self.pending_exception.store(value);
        self.has_pending_exception.set(true);
    }

    pub fn take_pending_exception(&self) -> Option<Value> {
        if self.has_pending_exception.get() {
            self.has_pending_exception.set(false);
            Some(self.pending_exception.raw())
        } else {
            None
        }
    }

    /// Like [`Self::take_pending_exception`], but anchored to the heap
    /// borrow: the register is GC-visited, so the read is current.
    pub fn take_pending_exception_tagged<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        if self.has_pending_exception.get() {
            self.has_pending_exception.set(false);
            Some(self.pending_exception.get(heap))
        } else {
            None
        }
    }

    pub fn has_pending_exception(&self) -> bool {
        self.has_pending_exception.get()
    }

    /// A callback threw during this thread's FFI call.
    #[inline(always)]
    pub fn callback_threw(&self) -> bool {
        self.callback_threw.get()
    }

    #[inline(always)]
    pub fn set_callback_threw(&self) {
        self.callback_threw.set(true);
    }

    #[inline(always)]
    pub fn take_callback_threw(&self) -> bool {
        self.callback_threw.replace(false)
    }

    /// The guest `Thread` object for this execution context, if any.
    pub fn thread_object<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Object>> {
        let word = self.thread_object.get(heap);
        (word.raw() != heap.known().the_hole.raw()).then(|| unsafe { word.cast() })
    }

    pub fn set_thread_object<'x>(&self, object: Tagged<'x, ThreadObject>) {
        self.thread_object.store(object.erase());
    }

    /// Whether re-entering the interpreter would exhaust the Rust stack.
    /// Bytecode recursion is bounded by the register stack, but a native
    /// calling back into `execute` (a getter, callback, or coercion) recurses
    /// on the native stack, which aborts instead of throwing when exhausted.
    #[inline(always)]
    pub fn rust_stack_exhausted(&self) -> bool {
        let probe = 0u8;
        let here = core::ptr::addr_of!(probe) as usize;
        let base = self.rust_stack_base.get();
        if base == 0 {
            self.rust_stack_base.set(here);
            return false;
        }
        base.saturating_sub(here) > RUST_STACK_BUDGET
    }

    /// Why this thread terminated, if it did (uncatchable by guest code).
    pub fn termination(&self) -> Option<Termination> {
        self.termination.get()
    }

    pub fn set_termination(&self, termination: Termination) {
        self.termination.set(Some(termination));
    }

    /// Reset the termination marker for a fresh run: a terminated
    /// execution ends here, the thread itself keeps going.
    pub fn clear_termination(&self) {
        self.termination.set(None);
    }

    /// The current frame's context (the chain `LoadContextSlot` walks),
    /// for direct eval. `None` when no frame is executing.
    pub fn current_context<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        if !self.is_frame_active() {
            return None;
        }
        Some(self.stack.context(heap, self.frame_base()))
    }

    /// A handle scope over this thread's registered root slots.
    #[inline]
    pub fn scope(&self) -> HandleScope<'_> {
        // Safety: a live `ThreadState` is reachable from the VM root set, so
        // its handle slots are visited for the whole scope borrow.
        unsafe { HandleScope::from_raw(NonNull::from(&self.handles)) }
    }

    #[inline(always)]
    pub fn handle_scope<R>(&self, f: impl for<'s> FnOnce(HandleScope<'s>) -> R) -> R {
        f(self.scope())
    }
}

unsafe impl Send for ThreadState {}
// TODO: can we get rid of this somehow?
// in practice we seem to need Sync because Heap needs access,
// in theory full isolation (and passing?) should be possible
unsafe impl Sync for ThreadState {}

impl SharedVM {
    fn visit_roots(&self, visitor: &mut dyn Visitor) {
        self.interner.visit_edges(visitor);
        self.known.visit_edges(visitor);
        self.heap.iterate_roots(visitor);
        for slot in self.weak_slots.lock().unwrap().iter() {
            visitor.visit(slot);
        }
        self.ffi.visit_edges(visitor);
        for state in &self.runtimes {
            state.visit_edges(visitor);
        }
        self.threads.visit_edges(visitor);
    }

    fn gc_host(&self) -> GcHost {
        GcHost {
            ctx: self as *const SharedVM as *const (),
            visit_roots: host_visit_roots,
            layout_of: host_layout_of,
            visit_object: host_visit_object,
        }
    }
}

fn host_visit_roots(ctx: *const (), visitor: &mut dyn Visitor) {
    let shared = unsafe { &*(ctx as *const SharedVM) };
    shared.visit_roots(visitor);
}

fn host_layout_of(addr: ObjectAddr) -> core::alloc::Layout {
    unsafe { object_layout(addr) }
}

fn host_visit_object(addr: ObjectAddr, visitor: &mut dyn Visitor) {
    unsafe { visit_object(addr, visitor) }
}

impl EdgeVisitable for ThreadState {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.handles.visit_edges(visitor);
        self.stack.visit_edges(visitor);
        visitor.visit(self.acc.as_raw());
        visitor.visit(self.pending_exception.as_raw());
        visitor.visit(self.construct_cache.as_raw());
        visitor.visit(self.thread_object.as_raw());
        for waiter in self.sync.waiters.lock().unwrap().iter() {
            visitor.visit(waiter.as_raw());
        }
    }
}

pub struct Thread {
    vm: VM,
    heap: Heap,
    state: Arc<ThreadState>,
}

impl Thread {
    pub fn vm(&self) -> &VM {
        &self.vm
    }

    pub fn heap(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// Split the thread into its parts (multi-borrow calls).
    pub fn split(&mut self) -> (&VM, &mut Heap, &ThreadState) {
        (&self.vm, &mut self.heap, &self.state)
    }

    pub fn state(&self) -> &ThreadState {
        &self.state
    }

    pub fn intern<'s>(&mut self, scope: &'s HandleScope<'_>, s: &str) -> Handle<'s, DenseString> {
        self.vm.interner().intern_str(&mut self.heap, scope, s)
    }

    pub fn take_pending_exception(&self) -> Option<Value> {
        self.state.take_pending_exception()
    }

    pub fn has_pending_exception(&self) -> bool {
        self.state.has_pending_exception()
    }

    #[inline(always)]
    pub fn handle_scope<R>(
        &mut self,
        f: impl for<'s> FnOnce(&mut Self, HandleScope<'s>) -> R,
    ) -> R {
        let state = Arc::clone(&self.state);
        let scope = state.scope();
        f(self, scope)
    }

    pub fn execute(
        &mut self,
        callable: Handle<'_, Object>,
        args: &[Value],
        runtime: RuntimeId,
    ) -> Result<Value, Thrown> {
        debug_assert_eq!(self.state.stack.top(), 0);
        debug_assert!(!self.state.is_frame_active());

        // don't leak pending exception if it exists
        let _ = self.state.take_pending_exception();
        self.state.clear_termination();
        let _ = self.heap.take_cancel();
        Self::release_task(&self.state);

        let main_state = Arc::clone(&self.state);
        if main_state.thread_object(&self.heap).is_none() {
            create_thread_object(&self.vm, &mut self.heap, &main_state);
        }
        let mut continuation = Continuation::zeroed();
        let result = main_state.handle_scope(|scope| {
            let object = main_state
                .thread_object(&self.heap)
                .and_then(|object| object.erase().get_as::<ThreadObject>(&self.heap))
                .expect("thread object must be installed");
            let object = scope.handle(object);
            let is_virtual = object.as_tagged(&self.heap).is_virtual(&self.heap);
            let state = if is_virtual {
                let id = object.as_tagged(&self.heap).tid(&self.heap);
                let Some(state) = self.vm.thread_state(id as u64) else {
                    let heap = &self.heap;
                    return Ok(self.vm.known().undefined.get(heap).raw());
                };
                state
            } else {
                debug_assert_eq!(
                    object.as_tagged(&self.heap).tid(&self.heap) as u64,
                    main_state.id()
                );
                Arc::clone(&main_state)
            };
            object.as_tagged(&self.heap).set_runtime(runtime);
            object
                .as_tagged(&self.heap)
                .set_state(ThreadStatus::Running);
            let thread = object.as_tagged(&self.heap).raw();

            let start = TaskStart {
                callable: callable.raw(),
                args: args.to_vec(),
                runtime,
            };
            match self.resume_task(thread, state, &mut continuation, Some(start)) {
                ResumeOutcome::Finished(result) => {
                    let keep_exception = result.is_err();
                    self.finish_task(object.as_tagged(&self.heap), &main_state, !keep_exception);
                    self.state = Arc::clone(&main_state);
                    if self.state.termination().is_some() {
                        let _ = self.state.take_pending_exception();
                        let heap = &self.heap;
                        return Ok(self.vm.known().undefined.get(heap).raw());
                    }
                    result
                }
                ResumeOutcome::Suspended => {
                    self.state = Arc::clone(&main_state);
                    let heap = &self.heap;
                    Ok(self.vm.known().undefined.get(heap).raw())
                }
            }
        });
        result
    }

    fn resume_task(
        &mut self,
        thread: Value,
        state: Arc<ThreadState>,
        continuation: &mut Continuation,
        start: Option<TaskStart>,
    ) -> ResumeOutcome {
        // Safety: the caller holds the thread object's handle rooted; no
        // collection runs before the task state is read.
        let thread = unsafe { Tagged::<ThreadObject>::from_value_unchecked(thread) };
        state.set_thread_object(thread);
        self.state = Arc::clone(&state);
        state.set_resume_target(continuation, false);
        state.set_resume_fiber(tsan::current_fiber());

        if !state.is_continuation_started() {
            let start = start.expect("fresh task without start data");
            prepare_task(&state, &self.vm, start);
        }

        // Safety: the continuation yields its heap borrow until the task
        // suspends or finishes.
        state.run_on_heap(&mut self.heap, || unsafe {
            continuation::switch(continuation, state.continuation_ptr())
        });

        match task_outcome(&state) {
            Some(result) => {
                release_task_run(&state);
                ResumeOutcome::Finished(result)
            }
            None => ResumeOutcome::Suspended,
        }
    }

    fn release_task(state: &ThreadState) {
        release_task_run(state);
    }

    fn finish_task(
        &self,
        thread: Tagged<'_, ThreadObject>,
        state: &ThreadState,
        take_exception: bool,
    ) {
        if take_exception && let Some(exception) = state.take_pending_exception_tagged(&self.heap) {
            thread.set_exception(&self.heap, exception);
        }
        thread.set_state(ThreadStatus::Terminated);
        state.mark_finished();
        if thread.is_virtual(&self.heap) {
            self.vm.remove_thread_state(state.id());
        }
        for slot in state.take_waiter_slots() {
            if let Some(waiter) = slot.load(&self.heap) {
                waiter.set_state(ThreadStatus::Ready);
            }
        }
        Self::release_task(state);
    }

    pub fn run_program(
        &mut self,
        program: bytecode::Program,
        runtime: RuntimeId,
    ) -> Result<Value, ScriptError> {
        self.handle_scope(|thread, scope| {
            let closure = materialize::Materialize::script(thread, &scope, &program, runtime);
            thread
                .execute(closure, &[], runtime)
                .map_err(ScriptError::Vm)
        })
    }

    pub fn eval<C: Compiler>(&mut self, src: &str) -> Result<Value, ScriptError> {
        let program = C::compile(self.vm(), src, bytecode::SourceMode::Script)
            .map_err(ScriptError::from_frontend)?;
        self.run_program(program, C::RUNTIME)
    }

    pub fn eval_file<C: Compiler>(&mut self, src: &str) -> Result<Value, ScriptError> {
        let program = C::compile(self.vm(), src, bytecode::SourceMode::File)
            .map_err(ScriptError::from_frontend)?;
        self.run_program(program, C::RUNTIME)
    }

    pub fn eval_repl<C: Compiler>(&mut self, src: &str) -> Result<Value, ScriptError> {
        let program = C::compile(self.vm(), src, bytecode::SourceMode::Repl)
            .map_err(ScriptError::from_frontend)?;
        self.run_program(program, C::RUNTIME)
    }
}

/// Failure of any stage of [`Thread::eval`].
#[derive(Debug)]
pub enum ScriptError {
    Parse(bytecode::FrontendError),
    Compile(bytecode::FrontendError),
    Vm(Thrown),
}

impl ScriptError {
    fn from_frontend(err: bytecode::FrontendError) -> Self {
        match err.kind {
            bytecode::FrontendErrorKind::Syntax => ScriptError::Parse(err),
            bytecode::FrontendErrorKind::Compile => ScriptError::Compile(err),
        }
    }
}

impl core::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse error: {e}"),
            Self::Compile(e) => write!(f, "compile error: {e}"),
            Self::Vm(_) => write!(f, "uncaught exception"),
        }
    }
}

impl std::error::Error for ScriptError {}

impl Clone for VM {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl VM {
    pub fn new<B: HeapBackend, I: Interpreter>(config: B::Config) -> Result<Self, AllocError> {
        let heap = GlobalHeap::from_backend::<B>(config)?;
        let interner = StringInterner::new();
        let shared = Arc::new(SharedVM {
            heap,
            ffi: FfiState::default(),
            known: WellKnown::uninit(),
            threads: ThreadRegistry::default(),
            interner,
            prototype_registry: PrototypeRegistry::new(),
            natives: NativeRegistry::new(),
            runtimes: Vec::new(),
            eval_languages: Vec::new(),
            weak_slots: Mutex::new(Vec::new()),
            shutdown_requested: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            thread_event_lock: Mutex::new(()),
            thread_event: Condvar::new(),
            execute: I::EXECUTE,
        });
        shared.heap.set_host(shared.gc_host());
        let mut local = shared
            .heap
            .new_local(&shared.known, &shared.prototype_registry);
        bootstrap_basics(&mut local);
        intern_well_known_strings(&mut local, &shared.interner);
        bootstrap_well_known(&mut local);
        Ok(Self { shared })
    }

    pub fn heap(&self) -> &GlobalHeap {
        &self.shared.heap
    }

    pub fn is_shutdown(&self) -> bool {
        self.shared.shutdown_requested.load(Ordering::Acquire)
    }

    pub fn note_shutdown(&self) {
        self.shared
            .shutdown_requested
            .store(true, Ordering::Release);
        self.notify_thread_event();
    }

    /// Record the process exit status a program requested via `os.exit`.
    pub fn set_exit_code(&self, code: i32) {
        self.shared.exit_code.store(code, Ordering::Release);
    }

    /// The process exit status a program requested; `0` by default.
    pub fn exit_code(&self) -> i32 {
        self.shared.exit_code.load(Ordering::Acquire)
    }

    /// Wake every host-blocked mutator waiting on a thread lifecycle event.
    pub fn notify_thread_event(&self) {
        let _guard = self.shared.thread_event_lock.lock().unwrap();
        self.shared.thread_event.notify_all();
    }

    /// Wait up to `timeout` for a lifecycle event. The caller loops on its
    /// own predicate. Must run inside [`Heap::block_on`] so the safepoint
    /// does not wait on this thread.
    pub fn wait_thread_event(&self, timeout: Duration) {
        let guard = self.shared.thread_event_lock.lock().unwrap();
        let _ = self.shared.thread_event.wait_timeout(guard, timeout);
    }

    pub fn known(&self) -> &WellKnown {
        &self.shared.known
    }

    #[inline(always)]
    pub fn ffi(&self) -> &FfiState {
        &self.shared.ffi
    }

    /// The FFI state during setup, before the VM is shared.
    pub fn ffi_mut(&mut self) -> &mut FfiState {
        &mut Arc::get_mut(&mut self.shared)
            .expect("cannot mutate the FFI state on a shared VM")
            .ffi
    }

    /// A weak handle for host state that must not keep the VM alive (FFI
    /// callback entries).
    pub fn downgrade(&self) -> Weak<SharedVM> {
        Arc::downgrade(&self.shared)
    }

    /// Re-attach to a live VM from [`VM::downgrade`].
    pub fn from_shared_weak(shared: &Weak<SharedVM>) -> Option<VM> {
        shared.upgrade().map(|shared| VM { shared })
    }

    pub fn threads(&self) -> &ThreadRegistry {
        &self.shared.threads
    }

    pub fn interner(&self) -> &StringInterner {
        &self.shared.interner
    }

    pub fn natives(&self) -> &NativeRegistry {
        &self.shared.natives
    }

    /// `[[Call]]`/`[[Construct]]` of an object callable without the
    /// [`HostCtx::enter`] checks: `callable` must neither be bound nor a proxy.
    #[inline]
    pub fn execute<'h>(
        &self,
        runtime: &'h dyn ErasedRuntimeState,
        heap: &'h mut Heap,
        state: &'h ThreadState,
        callable: Handle<'_, Object>,
        args: HandleSlice<'_>,
        new_target: Option<Handle<'_, Value>>,
    ) -> Result<Tagged<'h, Value>> {
        (self.shared.execute)(self, runtime, heap, state, callable, args, new_target)
    }

    #[inline]
    pub fn native(&self, index: NativeIndex) -> NativeFn {
        self.shared
            .natives
            .get(index)
            .expect("unknown native function index")
    }

    pub fn register_native(&mut self, f: NativeFn) -> NativeIndex {
        Arc::get_mut(&mut self.shared)
            .expect("cannot register natives on a shared VM")
            .natives
            .insert(f)
    }

    pub fn visit_roots(&self, visitor: &mut dyn Visitor) {
        self.shared.visit_roots(visitor)
    }

    /// Register a weak slot for `value` (must be an anchored strong heap
    /// pointer); returns its index. The GC clears the slot once the target
    /// dies.
    pub fn track_weak<'x, T: 'x>(&self, value: Tagged<'x, T>) -> usize {
        let weak = value.erase().as_weak().raw();
        let mut slots = self.shared.weak_slots.lock().unwrap();
        slots.push(unsafe { RawCell::from_word(weak.to_bits()) });
        slots.len() - 1
    }

    /// Current contents of a tracked weak slot: the weak-tagged target, or
    /// the cleared sentinel once the target died.
    pub fn weak_value(&self, index: usize) -> Value {
        Value::from_bits(self.shared.weak_slots.lock().unwrap()[index].load())
    }

    pub fn attach(&self) -> Thread {
        let heap = self
            .shared
            .heap
            .new_local(&self.shared.known, &self.shared.prototype_registry);
        let state = match self.shared.threads.attached_state() {
            Some(state) => state,
            None => {
                let id = self.shared.threads.next_id();
                let state = Arc::new(ThreadState::new(&heap, id));
                self.shared.threads.register(&state);
                self.shared.threads.remember_attached(&state);
                state
            }
        };
        Thread {
            vm: self.clone(),
            heap,
            state,
        }
    }

    pub fn attach_with_state(&self, state: Arc<ThreadState>) -> Thread {
        let heap = self
            .shared
            .heap
            .new_local(&self.shared.known, &self.shared.prototype_registry);
        Thread {
            vm: self.clone(),
            heap,
            state,
        }
    }

    pub fn next_thread_id(&self) -> u64 {
        self.shared.threads.next_id()
    }

    pub fn register_thread_state(&self, state: &Arc<ThreadState>) {
        self.shared.threads.register(state);
    }

    pub fn thread_state(&self, id: u64) -> Option<Arc<ThreadState>> {
        self.shared.threads.get(id)
    }

    pub fn remove_thread_state(&self, id: u64) -> Option<Arc<ThreadState>> {
        self.shared.threads.remove(id)
    }

    /// Run `R`'s setup (native-fn registration + global installation) and
    /// store its rooted state. Must run before the VM is shared:
    /// `R::setup` registers native functions, which needs exclusive access.
    fn install<R: Runtime>(&mut self) -> Result<(), SetupError> {
        assert!(
            !self.has_runtime::<R>(),
            "a runtime is installed once per VM"
        );
        let mut state = Box::new(R::default());
        R::setup(self, &mut state)?;
        let shared =
            Arc::get_mut(&mut self.shared).expect("cannot install runtimes on a shared VM");
        shared.runtimes.push(state);
        Ok(())
    }

    pub fn add<R: Runtime>(mut self) -> Result<Self, SetupError> {
        self.install::<R>()?;
        Ok(self)
    }

    /// Register `C`'s front end under `name` for indirect `KetteTools.eval`.
    pub fn add_eval<C: Compiler>(mut self, name: &'static str) -> Self {
        Arc::get_mut(&mut self.shared)
            .expect("cannot register eval languages on a shared VM")
            .eval_languages
            .push(EvalLanguage {
                name,
                compile: C::compile,
                runtime: C::RUNTIME,
            });
        self
    }

    /// Look up a registered eval language.
    pub fn eval_language(&self, name: &str) -> Option<&EvalLanguage> {
        self.shared
            .eval_languages
            .iter()
            .find(|language| language.name == name)
    }

    /// Whether `R` was installed on this VM.
    #[inline]
    pub fn has_runtime<R: Runtime>(&self) -> bool {
        self.shared.runtimes.get(R::ID.0 as usize).is_some()
    }

    /// The slot of an installed runtime.
    #[inline]
    pub fn runtime_id<R: Runtime>(&self) -> RuntimeId {
        debug_assert!(self.has_runtime::<R>(), "runtime not added to this VM");
        R::ID
    }

    /// The state of the runtime in `id`'s slot.
    #[inline]
    pub fn runtime_by_id(&self, id: RuntimeId) -> &dyn ErasedRuntimeState {
        &*self.shared.runtimes[id.0 as usize]
    }

    /// The state a runtime stored during [`VM::add`].
    #[inline]
    pub fn runtime<R: Runtime>(&self) -> &R {
        self.shared.runtimes[R::ID.0 as usize]
            .as_any()
            .downcast_ref::<R>()
            .expect("runtime not added to this VM")
    }

    /// Arm the `stress-minor-gc` knob (no-op without the feature).
    pub fn arm_gc_stress(&self) {
        self.shared.heap.arm_gc_stress();
    }

    pub fn spawn<F, R>(&self, f: F) -> std::thread::JoinHandle<R>
    where
        F: FnOnce(&mut Thread) -> R + Send + 'static,
        R: Send + 'static,
    {
        let vm = self.clone();
        std::thread::spawn(move || {
            let mut thread = vm.attach();
            f(&mut thread)
        })
    }
}
