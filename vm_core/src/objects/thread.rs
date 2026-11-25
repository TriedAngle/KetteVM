use crate::{
    AtomicGcSlot, AtomicOptionGcSlot, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap,
    HeapObject, ObjectKind, RuntimeId, Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;
use core::sync::atomic::{AtomicU16, Ordering};

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

/// Thread lifecycle state.
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
    /// Stable guest id; also the VM thread-state key.
    pub tid: GcSlot<Smi>,
    /// Platform or virtual.
    pub kind: GcSlot<Smi>,
    pub target: GcSlot<Value>,
    pub name: GcSlot<Value>,
    pub state: AtomicGcSlot<Smi>,
    /// Interrupt flag.
    pub interrupted: AtomicGcSlot<Smi>,
    runtime: AtomicU16,
    pub exception: AtomicOptionGcSlot<Value>,
}

pub struct ThreadObjectInit<'a> {
    pub kind: ThreadKind,
    pub target: Handle<'a, Value>,
    pub name: Handle<'a, Value>,
    pub state: ThreadStatus,
    pub tid: i64,
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

    pub fn state(&self, heap: &Heap) -> ThreadStatus {
        ThreadStatus::from_i64(self.state.load_smi(heap).value())
    }

    pub fn set_state(&self, state: ThreadStatus) {
        self.state.store_smi(Smi::new(state as i64));
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

    pub fn runtime(&self) -> Option<RuntimeId> {
        match self.runtime.load(Ordering::Acquire) {
            u16::MAX => None,
            id => Some(RuntimeId(id)),
        }
    }

    pub fn set_runtime(&self, runtime: RuntimeId) {
        self.runtime.store(runtime.0, Ordering::Release);
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
            .set(heap, host, heap.known().thread_object_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.set(heap, host, empty);
        self.elements.set(heap, host, empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.tid.init_tagged(Smi::new(config.tid).into());
        self.kind.init_tagged(Smi::new(config.kind as i64).into());
        self.target.set(heap, host, config.target.as_tagged(heap));
        self.name.set(heap, host, config.name.as_tagged(heap));
        self.state.store_smi(Smi::new(config.state as i64));
        self.interrupted.store_smi(Smi::new(0));
        self.runtime = AtomicU16::new(u16::MAX);
        self.exception = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
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
        visitor.visit(self.state.as_raw());
        visitor.visit(self.interrupted.as_raw());
        visitor.visit(self.exception.as_raw());
    }
}
