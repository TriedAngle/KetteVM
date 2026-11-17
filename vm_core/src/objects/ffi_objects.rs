use crate::ffi::memory::{copy_in, copy_out};
use crate::ffi::{CallPlan, FfiError, FfiResult, TypeId, TypeKind};
use crate::{
    CallableInfoObject, DenseString, EdgeVisitable, FixedArray, FixedByteArray, GcSlot, Handle,
    Header, Heap, HeapObject, Map, ObjectKind, OptionGcSlot, Smi, StringData, Tagged, Value,
    Visitor,
};
use core::alloc::Layout;

pub struct CFunctionInit<'a> {
    pub info: Handle<'a, CallableInfoObject>,
    pub signature: Handle<'a, TypeInfo>,
    pub target: Handle<'a, CPointer>,
    pub library: Option<Handle<'a, ForeignLibrary>>,
}

#[repr(C)]
pub struct CFunction {
    pub header: Header,
    pub info: GcSlot<CallableInfoObject>,
    pub signature: GcSlot<TypeInfo>,
    pub target: GcSlot<CPointer>,
    pub library: OptionGcSlot<ForeignLibrary>,
}

impl CFunction {
    pub fn info<'a>(&self, heap: &'a Heap) -> Tagged<'a, CallableInfoObject> {
        self.info.get(heap)
    }

    pub fn signature<'a>(&self, heap: &'a Heap) -> Tagged<'a, TypeInfo> {
        self.signature.get(heap)
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
        self.info.init_tagged(config.info.as_tagged(heap));
        self.signature.init_tagged(config.signature.as_tagged(heap));
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
        visitor.visit(self.info.as_raw());
        visitor.visit(self.signature.as_raw());
        visitor.visit(self.target.as_raw());
        visitor.visit(self.library.as_raw());
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CPointerFlags(u8);

impl CPointerFlags {
    pub const NONE: Self = Self(0);
    pub const DEAD: Self = Self(1);

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

pub struct CPointerInit<'a> {
    /// The type this pointer points at.
    pub pointee: Handle<'a, TypeInfo>,
    pub address: *mut u8,
    pub flags: CPointerFlags,
    pub library: Option<Tagged<'a, ForeignLibrary>>,
}

#[repr(C)]
pub struct CPointer {
    pub header: Header,
    pub pointee: GcSlot<TypeInfo>,
    pub address: GcSlot<Smi>,
    // TODO: this can be put into the header probably
    pub flags: GcSlot<Smi>,
    pub library: OptionGcSlot<ForeignLibrary>,
}

impl CPointer {
    /// The owning library, if this pointer came from a loaded library.
    pub fn library<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ForeignLibrary>> {
        self.library.get(heap)
    }

    pub fn set_library(&self, heap: &Heap, library: Tagged<'_, ForeignLibrary>) {
        self.library.set(heap, self.tagged(heap), library);
    }

    pub fn pointee<'a>(&self, heap: &'a Heap) -> Tagged<'a, TypeInfo> {
        self.pointee.get(heap)
    }

    pub fn address(&self) -> *mut u8 {
        self.address.to_smi_unchecked().value() as usize as *mut u8
    }

    pub fn flags(&self) -> CPointerFlags {
        CPointerFlags::from_bits(self.flags.to_smi_unchecked().value() as u8)
    }

    pub fn set_flags(&self, heap: &Heap, flags: CPointerFlags) {
        let host = self.tagged(heap);
        self.flags.set(heap, host, Smi::new(flags.bits() as i64));
    }

    pub fn is_null(&self) -> bool {
        self.address().is_null()
    }

    pub fn is_dead(&self) -> bool {
        self.flags().contains(CPointerFlags::DEAD)
    }

    pub fn is_live(&self) -> bool {
        !self.is_dead()
    }

    /// Mark the pointer dead; its address is no longer usable.
    pub fn kill(&self, heap: &Heap) {
        let flags = self.flags().union(CPointerFlags::DEAD);
        self.set_flags(heap, flags);
    }
}

impl HeapObject for CPointer {
    const KIND: ObjectKind = ObjectKind::CPointer;
    type Init<'a> = CPointerInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header.map.init(heap.known().c_pointer_map.get(heap));
        self.pointee.init_tagged(config.pointee.as_tagged(heap));
        self.flags
            .init_tagged(Smi::new(config.flags.bits() as i64).into());
        self.address
            .init_tagged(Smi::new(config.address as usize as i64).into());
        match config.library {
            Some(library) => self.library.set(heap, self.tagged(heap), library),
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

impl EdgeVisitable for CPointer {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.pointee.as_raw());
        visitor.visit(self.address.as_raw());
        visitor.visit(self.library.as_raw());
    }
}

pub struct CValueInit<'a> {
    pub ty: Handle<'a, TypeInfo>,
    /// Field accessor map; `None` uses the base `CValue` map
    pub map: Option<Handle<'a, Map>>,
    /// VM-owned bytes (`FixedByteArray`) or an external pointer (`CPointer`).
    pub backing: Handle<'a, Value>,
    pub offset: usize,
}

#[repr(C)]
pub struct CValue {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,

    pub ty: GcSlot<TypeInfo>,
    /// VM-owned bytes (`FixedByteArray`) or an external pointer (`CPointer`).
    pub backing: GcSlot<Value>,
    pub offset: GcSlot<Smi>,
}

impl CValue {
    pub fn c_type<'a>(&self, heap: &'a Heap) -> Tagged<'a, TypeInfo> {
        self.ty.get(heap)
    }

    pub fn backing<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.backing.get(heap)
    }

    pub fn offset(&self) -> usize {
        self.offset.to_smi_unchecked().value() as usize
    }

    /// Bytes available from [`Self::base`].
    pub fn remaining(&self, heap: &Heap) -> usize {
        match self.backing(heap).get_as::<FixedByteArray>(heap) {
            Some(inline) => inline.len().saturating_sub(self.offset()),
            None => usize::MAX,
        }
    }

    pub fn base(&self, heap: &Heap) -> FfiResult<*mut u8> {
        let backing = self.backing(heap);
        if let Some(pointer) = backing.get_as::<CPointer>(heap) {
            pointer.check_live(heap)?;
            return Ok(pointer.address().wrapping_add(self.offset()));
        }
        let inline = backing
            .get_as::<FixedByteArray>(heap)
            .expect("C value backing");
        Ok(inline.as_ptr().wrapping_add(self.offset()) as *mut u8)
    }
}

impl HeapObject for CValue {
    const KIND: ObjectKind = ObjectKind::CValue;
    type Init<'a> = CValueInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
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
        self.backing.init_tagged(config.backing.as_tagged(heap));
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
        visitor.visit(self.backing.as_raw());
    }
}

pub struct ForeignLibraryInit<'a> {
    pub handle: Handle<'a, CPointer>,
    pub path: Handle<'a, DenseString>,
}

#[repr(C)]
pub struct ForeignLibrary {
    pub header: Header,
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
        visitor.visit(self.handle.as_raw());
        visitor.visit(self.path.as_raw());
    }
}

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
    pub value_map: OptionGcSlot<Map>,
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
        visitor.visit(self.name.as_raw());
        visitor.visit(self.pointee.as_raw());
        visitor.visit(self.element.as_raw());
        visitor.visit(self.fields.as_raw());
        visitor.visit(self.params.as_raw());
        visitor.visit(self.result.as_raw());
        visitor.visit(self.value_map.as_raw());
    }
}

impl CPointer {
    /// A dead pointer is never usable; a live null pointer is not
    /// dereferenceable; a pointer whose library was closed is dead.
    pub fn check_live(&self, heap: &Heap) -> FfiResult<()> {
        if self.is_dead() {
            return Err(FfiError::DeadPointer);
        }
        if self.is_null() {
            return Err(FfiError::NullPointer);
        }
        if let Some(library) = self.library(heap)
            && !library.is_live(heap)
        {
            return Err(FfiError::LibraryClosed);
        }
        Ok(())
    }

    pub fn read_bytes(&self, heap: &Heap, out: &mut [u8]) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_out(self.address(), out);
        Ok(())
    }
}

macro_rules! scalar_accessors {
    ($($read:ident, $write:ident: $ty:ty, $size:literal;)*) => {
        impl CPointer {
            $(
                pub fn $read(&self, heap: &Heap) -> FfiResult<$ty> {
                    self.check_live(heap)?;
                    let mut bytes = [0u8; $size];
                    copy_out(self.address(), &mut bytes);
                    Ok(<$ty>::from_ne_bytes(bytes))
                }

                pub fn $write(&self, heap: &Heap, value: $ty) -> FfiResult<()> {
                    self.check_live(heap)?;
                    copy_in(self.address(), &value.to_ne_bytes());
                    Ok(())
                }
            )*
        }
    };
}

scalar_accessors! {
    read_i8, write_i8: i8, 1;
    read_u8, write_u8: u8, 1;
    read_i16, write_i16: i16, 2;
    read_u16, write_u16: u16, 2;
    read_i32, write_i32: i32, 4;
    read_u32, write_u32: u32, 4;
    read_i64, write_i64: i64, 8;
    read_u64, write_u64: u64, 8;
    read_f32, write_f32: f32, 4;
    read_f64, write_f64: f64, 8;
}
