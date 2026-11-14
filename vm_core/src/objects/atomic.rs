use crate::{EdgeVisitable, Header, Heap, HeapObject, ObjectKind, Visitor};
use core::alloc::Layout;
use core::sync::atomic::AtomicU64;

/// A shared 64-bit integer cell.
#[repr(C)]
pub struct AtomicI64 {
    pub header: Header,
    pub value: AtomicU64,
}

pub struct AtomicI64Init {
    pub value: i64,
}

impl AtomicI64 {
    pub fn value(&self) -> i64 {
        self.value.load(core::sync::atomic::Ordering::SeqCst) as i64
    }
}

impl HeapObject for AtomicI64 {
    const KIND: ObjectKind = ObjectKind::AtomicI64;
    type Init<'a> = AtomicI64Init;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().atomic_i64_map.get(heap));
        self.value
            .store(config.value as u64, core::sync::atomic::Ordering::SeqCst);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for AtomicI64 {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
    }
}
