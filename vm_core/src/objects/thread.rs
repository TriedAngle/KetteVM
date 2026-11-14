use crate::{
    AtomicGcSlot, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap, HeapObject, ObjectKind,
    Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

// Thread states.
pub const NEW: i64 = 0;
pub const RUNNABLE: i64 = 1;
pub const TERMINATED: i64 = 2;

/// The guest-facing `Thread` object; `slot` indexes the VM's host state.
#[repr(C)]
pub struct ThreadObject {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,
    pub target: GcSlot<Value>,
    pub name: GcSlot<Value>,
    pub state: AtomicGcSlot<Smi>,
    pub slot: AtomicGcSlot<Smi>,
}

pub struct ThreadObjectInit<'a> {
    pub target: Handle<'a, Value>,
    pub name: Handle<'a, Value>,
    pub state: i64,
    pub slot: i64,
}

impl ThreadObject {
    pub fn target<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.target.get(heap)
    }

    pub fn name<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.name.get(heap)
    }

    pub fn state(&self, heap: &Heap) -> i64 {
        self.state.load_smi(heap).value()
    }

    pub fn set_state(&self, state: i64) {
        self.state.store_smi(Smi::new(state));
    }

    pub fn slot(&self, heap: &Heap) -> i64 {
        self.slot.load_smi(heap).value()
    }

    pub fn set_slot(&self, slot: i64) {
        self.slot.store_smi(Smi::new(slot));
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
        self.target.set(heap, host, config.target.as_tagged(heap));
        self.name.set(heap, host, config.name.as_tagged(heap));
        self.state.store_smi(Smi::new(config.state));
        self.slot.store_smi(Smi::new(config.slot));
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
        visitor.visit(self.target.as_raw());
        visitor.visit(self.name.as_raw());
        visitor.visit(self.state.as_raw());
        visitor.visit(self.slot.as_raw());
    }
}
