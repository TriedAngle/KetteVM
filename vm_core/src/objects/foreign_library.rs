//! Loaded foreign library objects.
//!
//! A library owns a dynamic-loader handle and the path it was opened from.
//! It shares the `Object` prefix (like `CPointer`) so property and prototype
//! machinery works, with the payload inline:
//!
//! ```text
//! handle: CPointer (Pointer(Void), LIBRARY | BORROWED [| PROCESS])
//! path:   DenseString (the path as given, or "" for the process)
//! ```
//!
//! Liveness is the handle's `DEAD` flag. Symbols hold a strong reference back
//! to the library, so the library object cannot be collected while any symbol
//! is alive.

use crate::{
    CPointer, DenseString, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap, HeapObject,
    ObjectKind, Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

pub struct ForeignLibraryInit<'a> {
    pub handle: Handle<'a, CPointer>,
    pub path: Handle<'a, DenseString>,
}

#[repr(C)]
pub struct ForeignLibrary {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub handle: GcSlot<CPointer>,
    pub path: GcSlot<DenseString>,
}

impl ForeignLibrary {
    pub fn handle<'a>(&self, heap: &'a Heap) -> Tagged<'a, CPointer> {
        self.handle.get(heap)
    }

    pub fn path<'a>(&self, heap: &'a Heap) -> Tagged<'a, DenseString> {
        self.path.get(heap)
    }

    pub fn is_live(&self, heap: &Heap) -> bool {
        self.handle(heap).is_live()
    }
}

impl HeapObject for ForeignLibrary {
    const KIND: ObjectKind = ObjectKind::ForeignLibrary;
    type Init<'a> = ForeignLibraryInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header
            .map
            .init(heap.known().foreign_library_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.handle.init_tagged(config.handle.as_tagged(heap));
        self.path.init_tagged(config.path.as_tagged(heap));
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for ForeignLibrary {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.handle.as_raw());
        visitor.visit(self.path.as_raw());
    }
}
