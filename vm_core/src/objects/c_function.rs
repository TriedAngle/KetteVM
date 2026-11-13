//! A callable C function pointer.
//!
//! `CFunction` shares the `Object` prefix (so it is a receiver with a
//! prototype) and carries its call payload inline:
//!
//! ```text
//! info      CallableInfoObject with the FFI trampoline's NativeIndex
//! signature TypeId of a C function type
//! target    CPointer holding the raw function address
//! library   owning ForeignLibrary, or none
//! ```
//!
//! `Object::call_target` dispatches it to the trampoline native, which reads
//! these fields back from the callee.

use crate::ffi::TypeId;
use crate::{
    CPointer, CallableInfoObject, EdgeVisitable, FixedArray, ForeignLibrary, GcSlot, Handle,
    Header, Heap, HeapObject, ObjectKind, OptionGcSlot, Smi, Tagged, Value, Visitor,
};
use core::alloc::Layout;

pub struct CFunctionInit<'a> {
    pub info: Handle<'a, CallableInfoObject>,
    pub signature: TypeId,
    pub target: Handle<'a, CPointer>,
    pub library: Option<Handle<'a, ForeignLibrary>>,
}

#[repr(C)]
pub struct CFunction {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub info: GcSlot<CallableInfoObject>,
    /// TypeId of the function type this pointer was declared with.
    pub signature: GcSlot<Smi>,
    pub target: GcSlot<CPointer>,
    pub library: OptionGcSlot<ForeignLibrary>,
}

impl CFunction {
    pub fn info<'a>(&self, heap: &'a Heap) -> Tagged<'a, CallableInfoObject> {
        self.info.get(heap)
    }

    pub fn signature(&self) -> TypeId {
        TypeId::from_i64(self.signature.to_smi_unchecked().value())
    }

    pub fn target<'a>(&self, heap: &'a Heap) -> Tagged<'a, CPointer> {
        self.target.get(heap)
    }

    pub fn library<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ForeignLibrary>> {
        self.library.get(heap)
    }
}

impl HeapObject for CFunction {
    const KIND: ObjectKind = ObjectKind::CFunction;
    type Init<'a> = CFunctionInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().c_function_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.info.init_tagged(config.info.as_tagged(heap));
        self.signature
            .init_tagged(Smi::new(config.signature.as_i64()).into());
        self.target.init_tagged(config.target.as_tagged(heap));
        match &config.library {
            Some(library) => self
                .library
                .set(heap, self.tagged(heap), library.as_tagged(heap)),
            None => self.library.clear(heap),
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for CFunction {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.info.as_raw());
        visitor.visit(self.target.as_raw());
        visitor.visit(self.library.as_raw());
    }
}
