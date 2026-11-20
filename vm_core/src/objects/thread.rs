use crate::{
    AtomicGcSlot, AtomicOptionGcSlot, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap,
    HeapObject, ObjectKind, OptionGcSlot, RuntimeId, Smi, Tagged, ThreadState, Value, Visitor,
    simple_scheduler::SchedulerObject,
};
use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;

/// Guest-visible thread kind.
#[repr(i64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadKind {
    Platform = 0,
    Virtual = 1,
}

impl ThreadKind {
    pub fn from_i64(value: i64) -> Self {
        match value {
            0 => Self::Platform,
            1 => Self::Virtual,
            _ => unreachable!("invalid thread kind {value}"),
        }
    }
}

/// Thread lifecycle: guest-visible state and scheduler state.
#[repr(i64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadStatus {
    New = 0,
    Ready = 1,
    Running = 2,
    Blocked = 3,
    Terminated = 4,
}

impl ThreadStatus {
    pub fn from_i64(value: i64) -> Self {
        match value {
            0 => Self::New,
            1 => Self::Ready,
            2 => Self::Running,
            3 => Self::Blocked,
            4 => Self::Terminated,
            _ => unreachable!("invalid thread state {value}"),
        }
    }

    pub fn is_alive(self) -> bool {
        matches!(self, Self::Ready | Self::Running | Self::Blocked)
    }
}

/// Guest-facing thread object.
#[repr(C)]
pub struct ThreadObject {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,
    /// Stable id.
    pub tid: GcSlot<Smi>,
    /// Platform or virtual.
    pub kind: GcSlot<Smi>,
    pub target: GcSlot<Value>,
    pub name: GcSlot<Value>,
    /// Task state while scheduled.
    pub task: OptionGcSlot<TaskStateObject>,
    pub state: AtomicGcSlot<Smi>,
    /// Interrupt flag.
    pub interrupted: AtomicGcSlot<Smi>,
    /// Host platform slot index; -1 when absent.
    pub platform_slot: AtomicGcSlot<Smi>,
}

pub struct ThreadObjectInit<'a> {
    pub kind: ThreadKind,
    pub target: Handle<'a, Value>,
    pub name: Handle<'a, Value>,
    pub task: Option<Handle<'a, TaskStateObject>>,
    pub state: ThreadStatus,
    pub tid: i64,
    pub platform_slot: i64,
}

impl ThreadObject {
    pub fn target<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.target.get(heap)
    }

    pub fn name<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.name.get(heap)
    }

    pub fn tid(&self, heap: &Heap) -> i64 {
        self.tid.get(heap).to_i64().unwrap_or_default()
    }

    pub fn kind(&self, heap: &Heap) -> ThreadKind {
        ThreadKind::from_i64(self.kind.get(heap).to_i64().unwrap_or_default())
    }

    pub fn is_virtual(&self, heap: &Heap) -> bool {
        self.kind(heap) == ThreadKind::Virtual
    }

    pub fn task<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, TaskStateObject>> {
        self.task.get(heap)
    }

    pub fn set_task<'x>(&self, heap: &Heap, task: Tagged<'x, TaskStateObject>) {
        self.task.set(heap, self.tagged(heap), task);
    }

    pub fn clear_task(&self, heap: &Heap) {
        self.task.clear(heap);
    }

    pub fn state(&self, heap: &Heap) -> ThreadStatus {
        ThreadStatus::from_i64(self.state.load_smi(heap).value())
    }

    pub fn set_state(&self, state: ThreadStatus) {
        self.state.store_smi(Smi::new(state as i64));
    }

    pub fn platform_slot(&self, heap: &Heap) -> i64 {
        self.platform_slot.load_smi(heap).value()
    }

    pub fn set_platform_slot(&self, slot: i64) {
        self.platform_slot.store_smi(Smi::new(slot));
    }

    pub fn is_alive(&self, heap: &Heap) -> bool {
        self.state(heap).is_alive()
    }

    pub fn is_interrupted(&self, heap: &Heap) -> bool {
        self.interrupted.load_smi(heap).value() != 0
    }

    pub fn set_interrupted(&self, interrupted: bool) {
        self.interrupted.store_smi(Smi::new(interrupted as i64));
    }
}

impl HeapObject for ThreadObject {
    const KIND: ObjectKind = ObjectKind::ThreadObject;
    type Init<'a> = ThreadObjectInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header
            .map
            .init(heap.known().thread_object_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init_tagged(empty);
        self.elements.init_tagged(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.tid.init_tagged(Smi::new(config.tid).into());
        self.kind.init_tagged(Smi::new(config.kind as i64).into());
        self.target.set(heap, host, config.target.as_tagged(heap));
        self.name.set(heap, host, config.name.as_tagged(heap));
        match config.task {
            Some(task) => self.task.set(heap, host, task.as_tagged(heap)),
            None => self.task.clear(heap),
        }
        self.state.store_smi(Smi::new(config.state as i64));
        self.interrupted.store_smi(Smi::new(0));
        self.platform_slot.store_smi(Smi::new(config.platform_slot));
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for ThreadObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.length.as_raw());
        visitor.visit(self.tid.as_raw());
        visitor.visit(self.kind.as_raw());
        visitor.visit(self.target.as_raw());
        visitor.visit(self.name.as_raw());
        visitor.visit(self.task.as_raw());
        visitor.visit(self.state.as_raw());
        visitor.visit(self.interrupted.as_raw());
        visitor.visit(self.platform_slot.as_raw());
    }
}

/// Task state attached to a scheduled thread.
#[repr(C)]
pub struct TaskStateObject {
    pub header: Header,
    scheduler: AtomicOptionGcSlot<SchedulerObject>,
    waiting_on: AtomicOptionGcSlot<ThreadObject>,
    /// Carrier that owns this task; 0 until bound.
    carrier: AtomicUsize,
    state: MaybeUninit<Arc<ThreadState>>,
    runtime: AtomicU16,
    result: AtomicOptionGcSlot<Value>,
    exception: AtomicOptionGcSlot<Value>,
}

pub struct TaskStateObjectInit {
    pub state: Arc<ThreadState>,
}

impl TaskStateObject {
    pub fn state(&self) -> &Arc<ThreadState> {
        // Safety: `init` writes the Arc before the object is published.
        unsafe { self.state.assume_init_ref() }
    }

    pub fn scheduler<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, SchedulerObject>> {
        self.scheduler.load(heap)
    }

    pub fn set_scheduler(&self, heap: &Heap, scheduler: Tagged<'_, SchedulerObject>) {
        self.scheduler.set(heap, self.tagged(heap), scheduler);
    }

    pub fn waiting_on<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ThreadObject>> {
        self.waiting_on.load(heap)
    }

    pub fn set_waiting_on(&self, heap: &Heap, target: Tagged<'_, ThreadObject>) {
        self.waiting_on.set(heap, self.tagged(heap), target);
    }

    pub fn clear_waiting_on(&self, heap: &Heap) {
        self.waiting_on.clear(heap);
    }

    pub fn carrier(&self) -> usize {
        self.carrier.load(Ordering::Acquire)
    }

    pub fn set_carrier(&self, carrier: usize) {
        self.carrier.store(carrier, Ordering::Release);
    }

    pub fn remove_waiter(&self, heap: &Heap, waiter: Tagged<'_, ThreadObject>) {
        self.state().remove_waiter(heap, waiter);
    }

    pub fn runtime(&self) -> Option<RuntimeId> {
        match self.runtime.load(Ordering::Acquire) {
            u16::MAX => None,
            id => Some(RuntimeId(id)),
        }
    }

    pub fn set_runtime(&self, runtime: RuntimeId) {
        self.runtime.store(runtime.0, Ordering::Release);
    }

    pub fn result<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        self.result.load(heap)
    }

    pub fn set_result(&self, heap: &Heap, result: Tagged<'_, Value>) {
        self.result.set(heap, self.tagged(heap), result);
    }

    pub fn exception<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
        self.exception.load(heap)
    }

    pub fn set_exception(&self, heap: &Heap, exception: Tagged<'_, Value>) {
        self.exception.set(heap, self.tagged(heap), exception);
    }

    pub fn clear_exception(&self, heap: &Heap) {
        self.exception.clear(heap);
    }

    pub fn add_waiter(&self, heap: &Heap, waiter: Tagged<'_, ThreadObject>) {
        self.state().add_waiter(heap, waiter);
    }

    /// Move the waiter roots out.
    pub fn take_waiter_slots(&self) -> Vec<AtomicOptionGcSlot<ThreadObject>> {
        self.state().take_waiter_slots()
    }
}

impl HeapObject for TaskStateObject {
    const KIND: ObjectKind = ObjectKind::TaskStateObject;
    type Init<'a> = TaskStateObjectInit;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let hole = heap.known().the_hole.raw();
        self.header.map.init(heap.known().task_state_map.get(heap));
        self.scheduler = unsafe { AtomicOptionGcSlot::from_value(hole) };
        self.waiting_on = unsafe { AtomicOptionGcSlot::from_value(hole) };
        self.carrier = AtomicUsize::new(0);
        // Safety: the field is uninitialized heap storage; write without
        // dropping a previous `Arc`.
        unsafe { self.state.as_mut_ptr().write(Arc::clone(&config.state)) };
        self.runtime = AtomicU16::new(u16::MAX);
        self.result = unsafe { AtomicOptionGcSlot::from_value(hole) };
        self.exception = unsafe { AtomicOptionGcSlot::from_value(hole) };
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for TaskStateObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.scheduler.as_raw());
        visitor.visit(self.waiting_on.as_raw());
        self.state().visit_edges(visitor);
        visitor.visit(self.result.as_raw());
        visitor.visit(self.exception.as_raw());
    }
}
