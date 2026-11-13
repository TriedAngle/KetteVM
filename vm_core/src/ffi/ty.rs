//! C type identity, layout, and the runtime type table.
//!
//! `CType` is a nominal C type, not a language object map. It describes C ABI
//! identity and byte layout only. Struct and union types are distinct values
//! even when their layout matches; primitives, pointers, arrays, and function
//! signatures are interned structurally.

use core::alloc::Layout;
use std::collections::HashMap;
use std::fmt;

/// Stable handle into the type table.
///
/// Ids are append-only for the lifetime of the table: `CPointer` and
/// `CFunction` objects store them, so they can never be renumbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(u32);

impl TypeId {
    pub const VOID: Self = Self(0);
    pub const BOOL: Self = Self(1);
    pub const I8: Self = Self(2);
    pub const U8: Self = Self(3);
    pub const I16: Self = Self(4);
    pub const U16: Self = Self(5);
    pub const I32: Self = Self(6);
    pub const U32: Self = Self(7);
    pub const I64: Self = Self(8);
    pub const U64: Self = Self(9);
    pub const F32: Self = Self(10);
    pub const F64: Self = Self(11);
    /// C `char`: one byte, platform-dependent signedness.
    pub const CHAR: Self = Self(12);

    /// C `long`: 8 bytes on LP64, 4 bytes on Windows and 32-bit targets.
    pub const C_LONG: Self = if cfg!(windows) || cfg!(target_pointer_width = "32") {
        Self::I32
    } else {
        Self::I64
    };

    /// C `unsigned long`.
    pub const C_ULONG: Self = if cfg!(windows) || cfg!(target_pointer_width = "32") {
        Self::U32
    } else {
        Self::U64
    };

    /// C `size_t` / `uintptr_t`.
    pub const SIZE: Self = if cfg!(target_pointer_width = "32") {
        Self::U32
    } else {
        Self::U64
    };

    /// C `ptrdiff_t` / `intptr_t`.
    pub const PTRDIFF: Self = if cfg!(target_pointer_width = "32") {
        Self::I32
    } else {
        Self::I64
    };

    /// C `wchar_t`: 4 bytes signed on the targets KetteVM builds for.
    pub const WCHAR: Self = Self::I32;

    /// Number of reserved primitive ids, in registration order.
    pub const BUILTIN_COUNT: u32 = 13;

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn as_i64(self) -> i64 {
        self.0 as i64
    }

    pub const fn from_i64(bits: i64) -> Self {
        Self(bits as u32)
    }
}

const POINTER_SIZE: usize = core::mem::size_of::<*const ()>();

/// Calling convention of a function type.
///
/// Platforms use their default C convention; the field exists so a plan can
/// carry a non-default convention on Windows without a new type system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Abi {
    C,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructType {
    pub name: Option<String>,
    pub fields: Vec<Field>,
    pub layout: Layout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrayType {
    pub element: TypeId,
    pub len: usize,
    pub layout: Layout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionType {
    pub params: Vec<TypeId>,
    pub result: TypeId,
    pub variadic: bool,
    pub abi: Abi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: Option<String>,
    pub ty: TypeId,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CType {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    /// C `char`: one byte, signedness follows the target.
    Char,
    Pointer(TypeId),
    Struct(StructType),
    Union(StructType),
    /// A fixed-length array. `len == 0` is a C flexible array member tail.
    Array(ArrayType),
    Function(FunctionType),
}

impl CType {
    /// Layout of a complete object type; `None` for function types, which are
    /// never objects.
    pub fn layout(&self) -> Option<Layout> {
        let (size, align) = match self {
            CType::Void => (0, 1),
            CType::Bool | CType::I8 | CType::U8 | CType::Char => (1, 1),
            CType::I16 | CType::U16 => (2, 2),
            CType::I32 | CType::U32 | CType::F32 => (4, 4),
            // 8 on the 64-bit targets KetteVM builds for. i386 SysV aligns
            // `long long` and `double` to 4, so 32-bit support needs a
            // per-target alignment table before it can reuse this.
            CType::I64 | CType::U64 | CType::F64 => (8, 8),
            CType::Pointer(_) => (POINTER_SIZE, POINTER_SIZE),
            CType::Struct(s) | CType::Union(s) => return Some(s.layout),
            CType::Array(a) => return Some(a.layout),
            CType::Function(_) => return None,
        };
        Layout::from_size_align(size, align).ok()
    }

    pub const fn is_function(&self) -> bool {
        matches!(self, CType::Function(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeError {
    VoidField,
    FunctionField,
    SizeOverflow,
    DuplicateField(String),
    NotAStruct(TypeId),
    NotAnArray(TypeId),
    UnknownField(String),
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeError::VoidField => write!(f, "void is not an object type"),
            TypeError::FunctionField => write!(f, "function type is not an object type"),
            TypeError::SizeOverflow => write!(f, "type size overflows usize"),
            TypeError::DuplicateField(name) => write!(f, "duplicate field '{name}'"),
            TypeError::NotAStruct(id) => write!(f, "type {:?} is not a struct", id),
            TypeError::NotAnArray(id) => write!(f, "type {:?} is not an array", id),
            TypeError::UnknownField(name) => write!(f, "unknown field '{name}'"),
        }
    }
}

impl std::error::Error for TypeError {}

/// Structural identity of interned types.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum StructuralKey {
    Pointer(TypeId),
    Array(TypeId, usize),
    Function {
        params: Vec<TypeId>,
        result: TypeId,
        variadic: bool,
        abi: Abi,
    },
}

/// The runtime type table.
///
/// Append-only: ids stay valid forever. Struct and union types are nominal and
/// are never deduplicated; primitives, pointers, arrays, and function
/// signatures are interned structurally.
pub struct TypeTable {
    types: Vec<CType>,
    structural: HashMap<StructuralKey, TypeId>,
}

impl TypeTable {
    pub fn new() -> Self {
        let mut types = Vec::with_capacity(TypeId::BUILTIN_COUNT as usize);
        types.push(CType::Void);
        types.push(CType::Bool);
        types.push(CType::I8);
        types.push(CType::U8);
        types.push(CType::I16);
        types.push(CType::U16);
        types.push(CType::I32);
        types.push(CType::U32);
        types.push(CType::I64);
        types.push(CType::U64);
        types.push(CType::F32);
        types.push(CType::F64);
        types.push(CType::Char);
        debug_assert_eq!(types.len() as u32, TypeId::BUILTIN_COUNT);
        Self {
            types,
            structural: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    pub fn get(&self, id: TypeId) -> &CType {
        self.types
            .get(id.index())
            .unwrap_or_else(|| panic!("unknown C type id {id:?}"))
    }

    pub fn try_get(&self, id: TypeId) -> Option<&CType> {
        self.types.get(id.index())
    }

    /// Layout of a complete object type; `None` for function types.
    pub fn layout(&self, id: TypeId) -> Option<Layout> {
        self.get(id).layout()
    }

    fn require_object(&self, id: TypeId) -> Result<Layout, TypeError> {
        if id == TypeId::VOID {
            return Err(TypeError::VoidField);
        }
        self.layout(id).ok_or(TypeError::FunctionField)
    }

    fn push(&mut self, key: StructuralKey, ty: CType) -> TypeId {
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty);
        self.structural.insert(key, id);
        id
    }

    pub fn pointer_to(&mut self, pointee: TypeId) -> TypeId {
        let key = StructuralKey::Pointer(pointee);
        if let Some(id) = self.structural.get(&key) {
            return *id;
        }
        self.push(key, CType::Pointer(pointee))
    }

    pub fn array_of(&mut self, element: TypeId, len: usize) -> Result<TypeId, TypeError> {
        let element_layout = self.require_object(element)?;
        let size = element_layout
            .size()
            .checked_mul(len)
            .ok_or(TypeError::SizeOverflow)?;
        let layout = Layout::from_size_align(size, element_layout.align())
            .map_err(|_| TypeError::SizeOverflow)?;
        let key = StructuralKey::Array(element, len);
        if let Some(id) = self.structural.get(&key) {
            return Ok(*id);
        }
        Ok(self.push(
            key,
            CType::Array(ArrayType {
                element,
                len,
                layout,
            }),
        ))
    }

    pub fn function(
        &mut self,
        params: Vec<TypeId>,
        result: TypeId,
        variadic: bool,
        abi: Abi,
    ) -> Result<TypeId, TypeError> {
        for &param in &params {
            self.require_object(param)?;
        }
        if result != TypeId::VOID {
            self.require_object(result)?;
        }
        let key = StructuralKey::Function {
            params: params.clone(),
            result,
            variadic,
            abi,
        };
        if let Some(id) = self.structural.get(&key) {
            return Ok(*id);
        }
        Ok(self.push(
            key,
            CType::Function(FunctionType {
                params,
                result,
                variadic,
                abi,
            }),
        ))
    }

    /// Define a nominal struct. Field types must already exist and be complete.
    pub fn define_struct(
        &mut self,
        name: Option<String>,
        fields: Vec<(Option<String>, TypeId)>,
    ) -> Result<TypeId, TypeError> {
        let (fields, layout) = self.layout_fields(fields, false)?;
        let id = TypeId(self.types.len() as u32);
        self.types.push(CType::Struct(StructType {
            name,
            fields,
            layout,
        }));
        Ok(id)
    }

    /// Define a nominal union. Field types must already exist and be complete.
    pub fn define_union(
        &mut self,
        name: Option<String>,
        fields: Vec<(Option<String>, TypeId)>,
    ) -> Result<TypeId, TypeError> {
        let (fields, layout) = self.layout_fields(fields, true)?;
        let id = TypeId(self.types.len() as u32);
        self.types.push(CType::Union(StructType {
            name,
            fields,
            layout,
        }));
        Ok(id)
    }

    fn layout_fields(
        &self,
        fields: Vec<(Option<String>, TypeId)>,
        union: bool,
    ) -> Result<(Vec<Field>, Layout), TypeError> {
        let mut out = Vec::with_capacity(fields.len());
        let mut offset = 0usize;
        let mut max_size = 0usize;
        let mut align = 1usize;
        for (name, ty) in fields {
            if let Some(name) = &name
                && out
                    .iter()
                    .any(|field: &Field| field.name.as_deref() == Some(name))
            {
                return Err(TypeError::DuplicateField(name.clone()));
            }
            let field_layout = self.require_object(ty)?;
            align = align.max(field_layout.align());
            if union {
                out.push(Field {
                    name,
                    ty,
                    offset: 0,
                });
                max_size = max_size.max(field_layout.size());
                continue;
            }
            offset = align_up(offset, field_layout.align()).ok_or(TypeError::SizeOverflow)?;
            out.push(Field { name, ty, offset });
            offset = offset
                .checked_add(field_layout.size())
                .ok_or(TypeError::SizeOverflow)?;
        }
        let size = align_up(max_size.max(offset), align).ok_or(TypeError::SizeOverflow)?;
        let layout = Layout::from_size_align(size, align).map_err(|_| TypeError::SizeOverflow)?;
        Ok((out, layout))
    }

    /// Field of a struct or union by name.
    pub fn field(&self, id: TypeId, name: &str) -> Result<&Field, TypeError> {
        let fields = match self.get(id) {
            CType::Struct(s) | CType::Union(s) => &s.fields,
            _ => return Err(TypeError::NotAStruct(id)),
        };
        fields
            .iter()
            .find(|field| field.name.as_deref() == Some(name))
            .ok_or_else(|| TypeError::UnknownField(name.to_string()))
    }

    pub fn field_offset(&self, id: TypeId, name: &str) -> Result<usize, TypeError> {
        Ok(self.field(id, name)?.offset)
    }

    pub fn field_type(&self, id: TypeId, name: &str) -> Result<TypeId, TypeError> {
        Ok(self.field(id, name)?.ty)
    }

    /// Element type and length of an array type.
    pub fn array_element(&self, id: TypeId) -> Result<(TypeId, usize), TypeError> {
        match self.get(id) {
            CType::Array(a) => Ok((a.element, a.len)),
            _ => Err(TypeError::NotAnArray(id)),
        }
    }
}

impl Default for TypeTable {
    fn default() -> Self {
        Self::new()
    }
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    // Raylib mirrors: the first real consumer of the layout code.

    #[repr(C)]
    struct Color {
        r: u8,
        g: u8,
        b: u8,
        a: u8,
    }

    #[repr(C)]
    struct Vector2 {
        x: f32,
        y: f32,
    }

    #[repr(C)]
    struct Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    }

    #[repr(C)]
    struct Camera2D {
        offset: Vector2,
        target: Vector2,
        rotation: f32,
        zoom: f32,
    }

    #[repr(C)]
    struct Texture2D {
        id: u32,
        width: i32,
        height: i32,
        mipmaps: i32,
        format: i32,
    }

    #[repr(C)]
    struct Matrix {
        m: [f32; 16],
    }

    #[repr(C)]
    struct Padded {
        a: u8,
        b: u32,
        c: u16,
    }

    #[repr(C)]
    union Word {
        i: u64,
        f: f64,
        bytes: [u8; 8],
    }

    fn assert_layout(table: &TypeTable, id: TypeId, size: usize, align: usize) {
        assert_eq!(
            table.layout(id),
            Some(Layout::from_size_align(size, align).unwrap())
        );
    }

    fn assert_offset(table: &TypeTable, id: TypeId, name: &str, offset: usize) {
        assert_eq!(table.field_offset(id, name).unwrap(), offset);
    }

    #[test]
    fn builtins_are_registered_in_id_order() {
        let table = TypeTable::new();
        assert_eq!(table.len() as u32, TypeId::BUILTIN_COUNT);
        assert_eq!(table.get(TypeId::VOID), &CType::Void);
        assert_eq!(table.get(TypeId::BOOL), &CType::Bool);
        assert_eq!(table.get(TypeId::CHAR), &CType::Char);
        assert_eq!(table.get(TypeId::F64), &CType::F64);
        assert_layout(&table, TypeId::BOOL, 1, 1);
        assert_layout(&table, TypeId::I8, 1, 1);
        assert_layout(&table, TypeId::I16, 2, 2);
        assert_layout(&table, TypeId::I32, 4, 4);
        assert_layout(&table, TypeId::I64, 8, 8);
        assert_layout(&table, TypeId::F32, 4, 4);
        assert_layout(&table, TypeId::F64, 8, 8);
        assert_eq!(
            table.layout(TypeId::VOID),
            Some(Layout::from_size_align(0, 1).unwrap())
        );
    }

    #[test]
    fn color_has_no_padding() {
        let mut table = TypeTable::new();
        let color = table
            .define_struct(
                Some("Color".into()),
                vec![
                    (Some("r".into()), TypeId::U8),
                    (Some("g".into()), TypeId::U8),
                    (Some("b".into()), TypeId::U8),
                    (Some("a".into()), TypeId::U8),
                ],
            )
            .unwrap();
        assert_layout(&table, color, size_of::<Color>(), align_of::<Color>());
        assert_offset(&table, color, "r", offset_of!(Color, r));
        assert_offset(&table, color, "g", offset_of!(Color, g));
        assert_offset(&table, color, "b", offset_of!(Color, b));
        assert_offset(&table, color, "a", offset_of!(Color, a));
    }

    #[test]
    fn vector2_matches_c() {
        let mut table = TypeTable::new();
        let vector2 = table
            .define_struct(
                Some("Vector2".into()),
                vec![
                    (Some("x".into()), TypeId::F32),
                    (Some("y".into()), TypeId::F32),
                ],
            )
            .unwrap();
        assert_layout(&table, vector2, size_of::<Vector2>(), align_of::<Vector2>());
        assert_offset(&table, vector2, "x", offset_of!(Vector2, x));
        assert_offset(&table, vector2, "y", offset_of!(Vector2, y));
    }

    #[test]
    fn rectangle_and_nested_camera2d() {
        let mut table = TypeTable::new();
        let vector2 = table
            .define_struct(
                Some("Vector2".into()),
                vec![
                    (Some("x".into()), TypeId::F32),
                    (Some("y".into()), TypeId::F32),
                ],
            )
            .unwrap();
        let rectangle = table
            .define_struct(
                Some("Rectangle".into()),
                vec![
                    (Some("x".into()), TypeId::F32),
                    (Some("y".into()), TypeId::F32),
                    (Some("width".into()), TypeId::F32),
                    (Some("height".into()), TypeId::F32),
                ],
            )
            .unwrap();
        assert_layout(
            &table,
            rectangle,
            size_of::<Rectangle>(),
            align_of::<Rectangle>(),
        );
        assert_offset(&table, rectangle, "width", offset_of!(Rectangle, width));

        let camera = table
            .define_struct(
                Some("Camera2D".into()),
                vec![
                    (Some("offset".into()), vector2),
                    (Some("target".into()), vector2),
                    (Some("rotation".into()), TypeId::F32),
                    (Some("zoom".into()), TypeId::F32),
                ],
            )
            .unwrap();
        assert_layout(
            &table,
            camera,
            size_of::<Camera2D>(),
            align_of::<Camera2D>(),
        );
        assert_offset(&table, camera, "offset", offset_of!(Camera2D, offset));
        assert_offset(&table, camera, "target", offset_of!(Camera2D, target));
        assert_offset(&table, camera, "rotation", offset_of!(Camera2D, rotation));
        assert_offset(&table, camera, "zoom", offset_of!(Camera2D, zoom));
    }

    #[test]
    fn texture2d_and_matrix_match_c() {
        let mut table = TypeTable::new();
        let texture = table
            .define_struct(
                Some("Texture2D".into()),
                vec![
                    (Some("id".into()), TypeId::U32),
                    (Some("width".into()), TypeId::I32),
                    (Some("height".into()), TypeId::I32),
                    (Some("mipmaps".into()), TypeId::I32),
                    (Some("format".into()), TypeId::I32),
                ],
            )
            .unwrap();
        assert_layout(
            &table,
            texture,
            size_of::<Texture2D>(),
            align_of::<Texture2D>(),
        );
        assert_offset(&table, texture, "mipmaps", offset_of!(Texture2D, mipmaps));

        let floats = table.array_of(TypeId::F32, 16).unwrap();
        let matrix = table
            .define_struct(Some("Matrix".into()), vec![(Some("m".into()), floats)])
            .unwrap();
        assert_layout(&table, matrix, size_of::<Matrix>(), align_of::<Matrix>());
        assert_layout(
            &table,
            floats,
            size_of::<[f32; 16]>(),
            align_of::<[f32; 16]>(),
        );
    }

    #[test]
    fn padding_matches_c() {
        let mut table = TypeTable::new();
        let padded = table
            .define_struct(
                Some("Padded".into()),
                vec![
                    (Some("a".into()), TypeId::U8),
                    (Some("b".into()), TypeId::U32),
                    (Some("c".into()), TypeId::U16),
                ],
            )
            .unwrap();
        assert_layout(&table, padded, size_of::<Padded>(), align_of::<Padded>());
        assert_offset(&table, padded, "a", offset_of!(Padded, a));
        assert_offset(&table, padded, "b", offset_of!(Padded, b));
        assert_offset(&table, padded, "c", offset_of!(Padded, c));
    }

    #[test]
    fn union_matches_c() {
        let mut table = TypeTable::new();
        let bytes = table.array_of(TypeId::U8, 8).unwrap();
        let word = table
            .define_union(
                Some("Word".into()),
                vec![
                    (Some("i".into()), TypeId::U64),
                    (Some("f".into()), TypeId::F64),
                    (Some("bytes".into()), bytes),
                ],
            )
            .unwrap();
        assert_layout(&table, word, size_of::<Word>(), align_of::<Word>());
        assert_offset(&table, word, "i", 0);
        assert_offset(&table, word, "f", 0);
        assert_offset(&table, word, "bytes", 0);
    }

    #[test]
    fn pointer_and_function_types_are_interned() {
        let mut table = TypeTable::new();
        let pointer = table.pointer_to(TypeId::U8);
        assert_eq!(pointer, table.pointer_to(TypeId::U8));
        assert_layout(&table, pointer, POINTER_SIZE, POINTER_SIZE);

        let void_fn = table.function(vec![], TypeId::VOID, false, Abi::C).unwrap();
        assert_eq!(
            void_fn,
            table.function(vec![], TypeId::VOID, false, Abi::C).unwrap()
        );
        assert_eq!(table.layout(void_fn), None);

        let strlen = table
            .function(vec![pointer], TypeId::U64, false, Abi::C)
            .unwrap();
        assert_eq!(
            strlen,
            table
                .function(vec![pointer], TypeId::U64, false, Abi::C)
                .unwrap()
        );
        assert_ne!(strlen, void_fn);
    }

    #[test]
    fn nominal_structs_with_equal_layout_are_distinct() {
        let mut table = TypeTable::new();
        let a = table
            .define_struct(
                Some("A".into()),
                vec![
                    (Some("x".into()), TypeId::I32),
                    (Some("y".into()), TypeId::I32),
                ],
            )
            .unwrap();
        let b = table
            .define_struct(
                Some("B".into()),
                vec![
                    (Some("x".into()), TypeId::I32),
                    (Some("y".into()), TypeId::I32),
                ],
            )
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(table.layout(a), table.layout(b));
    }

    #[test]
    fn array_of_struct_uses_struct_size() {
        let mut table = TypeTable::new();
        let vector2 = table
            .define_struct(
                Some("Vector2".into()),
                vec![
                    (Some("x".into()), TypeId::F32),
                    (Some("y".into()), TypeId::F32),
                ],
            )
            .unwrap();
        let array = table.array_of(vector2, 4).unwrap();
        assert_layout(&table, array, size_of::<[Vector2; 4]>(), 4);
    }

    #[test]
    fn flexible_array_member_has_zero_size() {
        let mut table = TypeTable::new();
        let tail = table.array_of(TypeId::U8, 0).unwrap();
        let header = table
            .define_struct(
                Some("Header".into()),
                vec![
                    (Some("len".into()), TypeId::U32),
                    (Some("data".into()), tail),
                ],
            )
            .unwrap();
        assert_eq!(table.field_offset(header, "data"), Ok(4));
        assert_layout(&table, header, 4, 4);
    }

    #[test]
    fn rejects_invalid_object_types() {
        let mut table = TypeTable::new();
        assert_eq!(
            table.define_struct(None, vec![(None, TypeId::VOID)]),
            Err(TypeError::VoidField)
        );
        let f = table.function(vec![], TypeId::VOID, false, Abi::C).unwrap();
        assert_eq!(
            table.define_struct(None, vec![(None, f)]),
            Err(TypeError::FunctionField)
        );
        assert_eq!(table.array_of(TypeId::VOID, 1), Err(TypeError::VoidField));
    }

    #[test]
    fn rejects_duplicate_field_names() {
        let mut table = TypeTable::new();
        assert_eq!(
            table.define_struct(
                None,
                vec![
                    (Some("x".into()), TypeId::I32),
                    (Some("x".into()), TypeId::I32)
                ],
            ),
            Err(TypeError::DuplicateField("x".into()))
        );
    }

    #[test]
    fn platform_aliases_have_expected_widths() {
        let table = TypeTable::new();
        let long = table.layout(TypeId::C_LONG).unwrap();
        if cfg!(windows) || cfg!(target_pointer_width = "32") {
            assert_eq!(long.size(), 4);
        } else {
            assert_eq!(long.size(), 8);
        }
        assert_eq!(
            table.layout(TypeId::SIZE).unwrap().size(),
            core::mem::size_of::<usize>()
        );
    }
}
