//! A C type descriptor: identity, layout, and references to related types.
//!
//! The object is immutable after construction, so pointer identity is a valid
//! type identity and the registry can intern it. Related types are strong
//! references: a live `*T` keeps `T` alive.

use crate::ffi::CallPlan;
use crate::ffi::{TypeId, TypeKind};
use crate::{
    DenseString, EdgeVisitable, FixedArray, GcSlot, Handle, Header, Heap, HeapObject, ObjectKind,
    OptionGcSlot, Smi, StringData, Tagged, Value, Visitor,
};
use core::alloc::Layout;

/// `flags` bit: the function type accepts extra arguments.
pub const FLAG_VARIADIC: u32 = 1 << 0;

pub struct TypeInfoInit<'a> {
    pub kind: TypeKind,
    pub id: TypeId,
    pub size: usize,
    pub align: usize,
    pub flags: u32,
    pub name: Option<Handle<'a, DenseString>>,
    pub pointee: Option<Handle<'a, TypeInfo>>,
    pub element: Option<Handle<'a, TypeInfo>>,
    pub len: usize,
    pub fields: Option<Handle<'a, FixedArray>>,
    pub params: Option<Handle<'a, FixedArray>>,
    pub result: Option<Handle<'a, TypeInfo>>,
    /// Leaked `*const CallPlan` for function types.
    pub cif: usize,
}

#[repr(C)]
pub struct TypeInfo {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub kind: GcSlot<Smi>,
    pub id: GcSlot<Smi>,
    pub size: GcSlot<Smi>,
    pub align: GcSlot<Smi>,
    pub flags: GcSlot<Smi>,
    pub cif: GcSlot<Smi>,

    pub name: OptionGcSlot<DenseString>,
    pub pointee: OptionGcSlot<TypeInfo>,
    pub element: OptionGcSlot<TypeInfo>,
    pub len: GcSlot<Smi>,
    pub fields: OptionGcSlot<FixedArray>,
    pub params: OptionGcSlot<FixedArray>,
    pub result: OptionGcSlot<TypeInfo>,
    /// Per-type map for struct/union CValue field accessors.
    pub value_map: OptionGcSlot<crate::Map>,
}

impl TypeInfo {
    pub fn kind(&self) -> TypeKind {
        TypeKind::from_u8(self.kind.to_smi_unchecked().value() as u8)
    }

    pub fn id(&self) -> TypeId {
        TypeId::from_i64(self.id.to_smi_unchecked().value())
    }

    pub fn size(&self) -> usize {
        self.size.to_smi_unchecked().value() as usize
    }

    pub fn align(&self) -> usize {
        self.align.to_smi_unchecked().value() as usize
    }

    pub fn flags(&self) -> u32 {
        self.flags.to_smi_unchecked().value() as u32
    }

    pub fn is_variadic(&self) -> bool {
        self.flags() & FLAG_VARIADIC != 0
    }

    /// Complete object layout; `None` for function types.
    pub fn object_layout(&self) -> Option<Layout> {
        if self.kind() == TypeKind::Function {
            return None;
        }
        Layout::from_size_align(self.size(), self.align()).ok()
    }

    pub fn is_object(&self) -> bool {
        self.kind() != TypeKind::Function
    }

    pub fn name<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, DenseString>> {
        self.name.get(heap)
    }

    pub fn pointee<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, TypeInfo>> {
        self.pointee.get(heap)
    }

    pub fn element<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, TypeInfo>> {
        self.element.get(heap)
    }

    pub fn len(&self) -> usize {
        self.len.to_smi_unchecked().value() as usize
    }

    pub fn fields<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, FixedArray>> {
        self.fields.get(heap)
    }

    pub fn params<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, FixedArray>> {
        self.params.get(heap)
    }

    pub fn result<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, TypeInfo>> {
        self.result.get(heap)
    }

    pub fn value_map<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, crate::Map>> {
        self.value_map.get(heap)
    }

    pub fn set_value_map(&self, heap: &Heap, map: Tagged<'_, crate::Map>) {
        self.value_map.set(heap, self.tagged(heap), map);
    }

    /// The leaked call plan of a function type.
    pub fn plan(&self) -> &'static CallPlan {
        let ptr = self.cif.to_smi_unchecked().value() as usize;
        debug_assert!(ptr != 0, "function type without a call plan");
        // Safety: call plans are leaked and never moved or freed.
        unsafe { &*(ptr as *const CallPlan) }
    }

    /// Look up a struct/union field: its type and byte offset.
    pub fn field<'a>(&self, heap: &'a Heap, name: &str) -> Option<(Tagged<'a, TypeInfo>, usize)> {
        let fields = self.fields(heap)?;
        let mut index = 0;
        while index + 2 < fields.len() {
            let matches = fields
                .at(heap, index)
                .get_as::<DenseString>(heap)
                .is_some_and(|field| name_matches(&field, heap, name));
            if matches {
                let ty = fields
                    .at(heap, index + 1)
                    .get_as::<TypeInfo>(heap)
                    .expect("field type slot");
                let offset = fields.at(heap, index + 2).to_i64().unwrap_or(0) as usize;
                return Some((ty, offset));
            }
            index += 3;
        }
        None
    }

    /// Field names in declaration order; `None` for an anonymous field.
    pub fn field_names(&self, heap: &Heap) -> Vec<Option<String>> {
        let Some(fields) = self.fields(heap) else {
            return Vec::new();
        };
        let mut names = Vec::with_capacity(fields.len() / 3);
        let mut index = 0;
        while index + 2 < fields.len() {
            names.push(
                fields
                    .at(heap, index)
                    .get_as::<DenseString>(heap)
                    .map(|name| name.to_rust_string(heap)),
            );
            index += 3;
        }
        names
    }
}

fn name_matches(name: &DenseString, heap: &Heap, target: &str) -> bool {
    let data = name.data(heap);
    if target.is_ascii() {
        data == StringData::Latin1(target.as_bytes())
    } else {
        data.to_rust_string() == target
    }
}

impl HeapObject for TypeInfo {
    const KIND: ObjectKind = ObjectKind::TypeInfo;
    type Init<'a> = TypeInfoInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().c_type_map.get(heap));
        let empty = heap.known().empty_fixed_array.get(heap);
        self.slots.init(empty);
        self.elements.init(empty.erase());
        self.length.init_tagged(Smi::new(0).into());

        let host = self.tagged(heap);
        self.kind.init_tagged(Smi::new(config.kind as i64).into());
        self.id.init_tagged(Smi::new(config.id.as_i64()).into());
        self.size.init_tagged(Smi::new(config.size as i64).into());
        self.align.init_tagged(Smi::new(config.align as i64).into());
        self.flags.init_tagged(Smi::new(config.flags as i64).into());
        self.cif.init_tagged(Smi::new(config.cif as i64).into());
        self.len.init_tagged(Smi::new(config.len as i64).into());

        match &config.name {
            Some(name) => self.name.set(heap, host, name.as_tagged(heap)),
            None => self.name.clear(heap),
        }
        match &config.pointee {
            Some(pointee) => self.pointee.set(heap, host, pointee.as_tagged(heap)),
            None => self.pointee.clear(heap),
        }
        match &config.element {
            Some(element) => self.element.set(heap, host, element.as_tagged(heap)),
            None => self.element.clear(heap),
        }
        match &config.fields {
            Some(fields) => self.fields.set(heap, host, fields.as_tagged(heap)),
            None => self.fields.clear(heap),
        }
        match &config.params {
            Some(params) => self.params.set(heap, host, params.as_tagged(heap)),
            None => self.params.clear(heap),
        }
        match &config.result {
            Some(result) => self.result.set(heap, host, result.as_tagged(heap)),
            None => self.result.clear(heap),
        }
        self.value_map.clear(heap);
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for TypeInfo {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
        visitor.visit(self.name.as_raw());
        visitor.visit(self.pointee.as_raw());
        visitor.visit(self.element.as_raw());
        visitor.visit(self.fields.as_raw());
        visitor.visit(self.params.as_raw());
        visitor.visit(self.result.as_raw());
        visitor.visit(self.value_map.as_raw());
    }
}
