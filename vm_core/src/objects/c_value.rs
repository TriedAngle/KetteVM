//! A C value proxy: a typed view over either VM-owned bytes or foreign memory.
//!
//! The backing is one of:
//! - `inline`: a VM-owned byte array (values created with `new`, call results),
//! - `external`: a `CPointer` (absolute or `VM_BACKED`), for views over C
//!   memory or over another value's storage.
//!
//! `offset` is added to the backing base, so views into a parent value share
//! the same backing and only move the offset. Accessors installed on the
//! per-type map read at `base + field offset`.

use crate::{
    CPointer, EdgeVisitable, FixedArray, FixedByteArray, GcSlot, Handle, Header, Heap, HeapObject,
    Map, ObjectKind, OptionGcSlot, Smi, Tagged, TypeInfo, Value, Visitor,
};
use core::alloc::Layout;

pub struct CValueInit<'a> {
    pub ty: Handle<'a, TypeInfo>,
    /// Per-type map for struct/union field accessors.
    pub map: Option<Handle<'a, Map>>,
    /// VM-owned storage.
    pub inline: Option<Handle<'a, FixedByteArray>>,
    /// Borrowed storage.
    pub external: Option<Handle<'a, CPointer>>,
    pub offset: usize,
}

#[repr(C)]
pub struct CValue {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub ty: GcSlot<TypeInfo>,
    pub inline: OptionGcSlot<FixedByteArray>,
    pub external: OptionGcSlot<CPointer>,
    pub offset: GcSlot<Smi>,
}

impl CValue {
    pub fn c_type<'a>(&self, heap: &'a Heap) -> Tagged<'a, TypeInfo> {
        self.ty.get(heap)
    }

    pub fn inline_data<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, FixedByteArray>> {
        self.inline.get(heap)
    }

    pub fn external_pointer<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, CPointer>> {
        self.external.get(heap)
    }

    pub fn offset(&self) -> usize {
        self.offset.to_smi_unchecked().value() as usize
    }

    /// The effective address of this value's first byte.
    pub fn base(&self, heap: &Heap) -> Option<usize> {
        if let Some(external) = self.external_pointer(heap) {
            return Some(external.address(heap) + self.offset());
        }
        self.inline_data(heap)
            .map(|inline| inline.as_ptr() as usize + self.offset())
    }
}

impl HeapObject for CValue {
    const KIND: ObjectKind = ObjectKind::CValue;
    type Init<'a> = CValueInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        match &config.map {
            Some(map) => self.header.map.init(map.as_tagged(heap)),
            None => self.header.map.init(heap.known().c_value_map.get(heap)),
        }
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());
        self.ty.init_tagged(config.ty.as_tagged(heap));
        self.offset
            .init_tagged(Smi::new(config.offset as i64).into());
        match &config.inline {
            Some(inline) => self.inline.set(heap, host, inline.as_tagged(heap)),
            None => self.inline.clear(heap),
        }
        match &config.external {
            Some(external) => self.external.set(heap, host, external.as_tagged(heap)),
            None => self.external.clear(heap),
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for CValue {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.ty.as_raw());
        visitor.visit(self.inline.as_raw());
        visitor.visit(self.external.as_raw());
    }
}
