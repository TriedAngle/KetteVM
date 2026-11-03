use core::alloc::Layout;

use crate::{
    DenseString, EdgeVisitable, GcSlot, Handle, HandleScope, Header, Heap, HeapObject, ObjectKind,
    Tagged, Value, Visitor,
};

#[repr(C)]
pub struct Symbol {
    pub header: Header,
    /// `[[Description]]`: a String or `undefined`.
    pub description: GcSlot<Value>,
}

impl Symbol {
    pub fn new<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        description: Option<Handle<'_, DenseString>>,
    ) -> Handle<'s, Symbol> {
        let description = match description {
            Some(text) => text.as_tagged(heap).erase(),
            None => heap.known().undefined.get(heap).erase(),
        };
        let description = scope.handle(description);
        heap.allocate_handle::<Symbol>(description, scope)
    }

    pub fn description<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.description.get(heap)
    }
}

impl HeapObject for Symbol {
    const KIND: ObjectKind = ObjectKind::Symbol;
    type Init<'a> = Handle<'a, Value>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().symbol_map.get(heap));
        self.description.init(config.as_tagged(heap));
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for Symbol {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.description.as_raw());
    }
}
