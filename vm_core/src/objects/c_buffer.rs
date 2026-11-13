//! Storage for C aggregate values (structs, unions, arrays).

use crate::ffi::TypeId;
use crate::{
    CPointer, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap, HeapObject, ObjectKind, Smi,
    Tagged, Value, Visitor,
};
use core::alloc::Layout;

pub struct CBufferInit<'a> {
    pub type_id: TypeId,
    pub storage: Handle<'a, CPointer>,
    pub count: usize,
}

#[repr(C)]
pub struct CBuffer {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub type_id: GcSlot<Smi>,
    /// Owned external storage (`CPointer` with `OWNED_EXTERNAL`).
    pub storage: GcSlot<CPointer>,
    /// Element count for array views; unused for structs.
    pub count: GcSlot<Smi>,
}

impl CBuffer {
    pub fn c_type(&self) -> TypeId {
        TypeId::from_i64(self.type_id.to_smi_unchecked().value())
    }

    pub fn storage<'a>(&self, heap: &'a Heap) -> Tagged<'a, CPointer> {
        self.storage.get(heap)
    }

    pub fn count(&self) -> usize {
        self.count.to_smi_unchecked().value() as usize
    }
}

impl HeapObject for CBuffer {
    const KIND: ObjectKind = ObjectKind::CBuffer;
    type Init<'a> = CBufferInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().c_buffer_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.type_id
            .init_tagged(Smi::new(config.type_id.as_i64()).into());
        self.storage.init_tagged(config.storage.as_tagged(heap));
        self.count.init_tagged(Smi::new(config.count as i64).into());
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for CBuffer {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.storage.as_raw());
    }
}
