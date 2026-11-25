use crate::{
    AtomicMaybeWeakGcSlot, EdgeVisitable, Header, Heap, HeapObject, MaybeWeak, ObjectKind, Tagged,
    Value, Visitor,
};
use core::{alloc::Layout, sync::atomic::Ordering};

#[repr(C)]
pub struct Cell {
    pub header: Header,
    pub maybe_value: AtomicMaybeWeakGcSlot,
}

pub struct CellInit<'a> {
    pub value: Tagged<'a, MaybeWeak<Value>>,
}

impl Cell {
    #[inline]
    pub fn is_valid(&self, _heap: &Heap) -> bool {
        !self.maybe_value.is_cleared()
    }

    #[inline]
    pub fn is_cleared(&self) -> bool {
        self.maybe_value.is_cleared()
    }

    pub fn clear(&self) {
        self.maybe_value
            .store_word(Value::CLEARED.to_bits(), Ordering::Relaxed);
    }

    pub fn payload<'a>(&self, heap: &'a Heap) -> Tagged<'a, MaybeWeak<Value>> {
        self.maybe_value.get(heap)
    }
}

impl HeapObject for Cell {
    const KIND: ObjectKind = ObjectKind::Cell;
    type Init<'a> = CellInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header.map.init(heap.known().cell_map.get(heap));
        self.maybe_value.set(heap, host, config.value);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for Cell {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.maybe_value.as_raw());
    }
}
