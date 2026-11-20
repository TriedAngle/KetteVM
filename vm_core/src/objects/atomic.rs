use crate::{EdgeVisitable, Header, Heap, HeapObject, ObjectKind, Visitor};
use core::alloc::Layout;
use core::sync::atomic::{AtomicI64, Ordering};

/// A shared 64-bit integer cell.
#[repr(C)]
pub struct AtomicI64Object {
    pub header: Header,
    pub value: AtomicI64,
}

pub struct AtomicI64ObjectInit {
    pub value: i64,
}

impl AtomicI64Object {
    pub fn value(&self) -> i64 {
        self.value.load(Ordering::SeqCst)
    }
}

impl HeapObject for AtomicI64Object {
    const KIND: ObjectKind = ObjectKind::AtomicI64Object;
    type Init<'a> = AtomicI64ObjectInit;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().atomic_i64_map.get(heap));
        self.value.store(config.value, Ordering::SeqCst);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for AtomicI64Object {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
    }
}
