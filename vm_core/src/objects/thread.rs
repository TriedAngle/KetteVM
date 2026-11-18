use crate::{
    AtomicGcSlot, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap, HeapObject, ObjectKind,
    Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

// Guest-visible thread status (a subset of java.lang.Thread.State).
pub const NEW: i64 = 0;
pub const RUNNABLE: i64 = 1;
pub const TERMINATED: i64 = 2;

/// java.lang.Thread's default priority.
pub const NORM_PRIORITY: i64 = 5;

/// The guest-facing `Thread` object.
///
/// Mirrors the JVM split: VM-private fields (`native`, `tid`, `name`,
/// `interrupted`) plus the `FieldHolder` fields (`target`, `state`,
/// priority, daemon, stack size). `native` is the `eetop` analogue: the
/// host slot index, or -1 when the thread has no joinable host state.
#[repr(C)]
pub struct ThreadObject {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,
    /// Stable id assigned at construction (`tid`).
    pub tid: GcSlot<Smi>,
    pub target: GcSlot<Value>,
    pub name: GcSlot<Value>,
    /// Interrupted flag (space reserved for interruption support).
    pub interrupted: AtomicGcSlot<Smi>,
    pub state: AtomicGcSlot<Smi>,
    pub priority: GcSlot<Smi>,
    pub daemon: GcSlot<Smi>,
    pub stack_size: GcSlot<Smi>,
    /// Host `ThreadSlot` index; -1 when there is none.
    pub native: AtomicGcSlot<Smi>,
}

pub struct ThreadObjectInit<'a> {
    pub target: Handle<'a, Value>,
    pub name: Handle<'a, Value>,
    pub state: i64,
    pub tid: i64,
    pub native: i64,
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

    pub fn state(&self, heap: &Heap) -> i64 {
        self.state.load_smi(heap).value()
    }

    pub fn set_state(&self, state: i64) {
        self.state.store_smi(Smi::new(state));
    }

    pub fn native(&self, heap: &Heap) -> i64 {
        self.native.load_smi(heap).value()
    }

    pub fn set_native(&self, slot: i64) {
        self.native.store_smi(Smi::new(slot));
    }

    pub fn is_alive(&self, heap: &Heap) -> bool {
        self.state(heap) == RUNNABLE
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
        self.target.set(heap, host, config.target.as_tagged(heap));
        self.name.set(heap, host, config.name.as_tagged(heap));
        self.interrupted.store_smi(Smi::new(0));
        self.state.store_smi(Smi::new(config.state));
        self.priority.init_tagged(Smi::new(NORM_PRIORITY).into());
        self.daemon.init_tagged(Smi::new(0).into());
        self.stack_size.init_tagged(Smi::new(0).into());
        self.native.store_smi(Smi::new(config.native));
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
        visitor.visit(self.target.as_raw());
        visitor.visit(self.name.as_raw());
        visitor.visit(self.interrupted.as_raw());
        visitor.visit(self.state.as_raw());
        visitor.visit(self.priority.as_raw());
        visitor.visit(self.daemon.as_raw());
        visitor.visit(self.stack_size.as_raw());
        visitor.visit(self.native.as_raw());
    }
}
