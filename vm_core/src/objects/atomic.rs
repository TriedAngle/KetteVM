use crate::{
    AtomicGcSlot, EdgeVisitable, Header, Heap, HeapObject, ObjectKind, Tagged, Value, Visitor,
};
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

#[repr(C)]
pub struct AtomicRefObject {
    pub header: Header,
    pub value: AtomicGcSlot<Value>,
}

pub struct AtomicRefObjectInit<'a> {
    pub value: Tagged<'a, Value>,
}

impl AtomicRefObject {
    pub fn load<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.value.get(heap)
    }

    pub fn store<'h, 'x>(&self, heap: &Heap, host: Tagged<'h, Value>, value: Tagged<'x, Value>) {
        self.value.set(heap, host, value);
    }

    pub fn swap<'a, 'h, 'x>(
        &self,
        heap: &'a Heap,
        host: Tagged<'h, Value>,
        value: Tagged<'x, Value>,
    ) -> Tagged<'a, Value> {
        let old = self.value.swap_value(heap, host, value);
        unsafe { Tagged::from_value_unchecked(Value::from_bits(old)) }
    }

    pub fn compare_exchange<'a, 'h>(
        &self,
        heap: &'a Heap,
        host: Tagged<'h, Value>,
        expected: crate::Word,
        value: Tagged<'_, Value>,
    ) -> Tagged<'a, Value> {
        let old = match self
            .value
            .compare_exchange_value(heap, host, expected, value)
        {
            Ok(old) | Err(old) => old,
        };
        unsafe { Tagged::from_value_unchecked(Value::from_bits(old)) }
    }
}

impl HeapObject for AtomicRefObject {
    const KIND: ObjectKind = ObjectKind::AtomicRefObject;
    type Init<'a> = AtomicRefObjectInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().atomic_ref_map.get(heap));
        self.value.init_tagged(config.value);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for AtomicRefObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.value.as_raw());
    }
}
