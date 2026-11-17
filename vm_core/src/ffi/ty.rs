use crate::ffi::CallPlan;
use crate::ffi::{FfiError, FfiResult};
use crate::{
    EdgeVisitable, FixedArray, Handle, HandleScope, Heap, MaybeWeakGcSlot, Smi, Tagged, TypeInfo,
    TypeInfoInit, VM, Value, Visitor,
};
use core::alloc::Layout;
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// Stable handle into the type registry.
///
/// Ids are append-only: a type is never removed or renumbered, so an id always
/// names the same type. Built-in ids are `0..BUILTIN_COUNT`.
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

    /// C `char16_t`: a 16-bit UTF-16 code unit.
    pub const CHAR16: Self = Self(13);

    /// C `wchar_t`: 16-bit on Windows, 32-bit signed elsewhere.
    pub const WCHAR: Self = if cfg!(windows) {
        Self::CHAR16
    } else {
        Self::I32
    };

    pub const BUILTIN_COUNT: u32 = TypeKind::BUILTIN_COUNT as u32;

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

/// The shape of a C type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeKind {
    Void = 0,
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
    Char,
    Char16,
    Pointer,
    Struct,
    Union,
    Array,
    Function,
}

impl TypeKind {
    pub const BUILTIN_COUNT: usize = 14;

    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Void,
            1 => Self::Bool,
            2 => Self::I8,
            3 => Self::U8,
            4 => Self::I16,
            5 => Self::U16,
            6 => Self::I32,
            7 => Self::U32,
            8 => Self::I64,
            9 => Self::U64,
            10 => Self::F32,
            11 => Self::F64,
            12 => Self::Char,
            13 => Self::Char16,
            14 => Self::Pointer,
            15 => Self::Struct,
            16 => Self::Union,
            17 => Self::Array,
            18 => Self::Function,
            _ => panic!("invalid C type kind"),
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Bool => "bool",
            Self::I8 => "i8",
            Self::U8 => "u8",
            Self::I16 => "i16",
            Self::U16 => "u16",
            Self::I32 => "i32",
            Self::U32 => "u32",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Char => "char",
            Self::Char16 => "char16",
            Self::Pointer => "pointer",
            Self::Struct => "struct",
            Self::Union => "union",
            Self::Array => "array",
            Self::Function => "function",
        }
    }

    /// Size and alignment of a built-in type.
    pub const fn builtin_layout(self) -> (usize, usize) {
        match self {
            Self::Void => (0, 1),
            Self::Bool | Self::I8 | Self::U8 | Self::Char => (1, 1),
            Self::I16 | Self::U16 | Self::Char16 => (2, 2),
            Self::I32 | Self::U32 | Self::F32 => (4, 4),
            // 8 on the 64-bit targets KetteVM builds for. i386 SysV aligns
            // `long long` and `double` to 4.
            Self::I64 | Self::U64 | Self::F64 => (8, 8),
            Self::Pointer | Self::Struct | Self::Union | Self::Array | Self::Function => (
                core::mem::size_of::<*const ()>(),
                core::mem::size_of::<*const ()>(),
            ),
        }
    }
}

const POINTER_SIZE: usize = core::mem::size_of::<*const ()>();

/// Calling convention of a function type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Abi {
    C,
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

/// Structural identity of an interned type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TypeKey {
    Pointer(TypeId),
    Array(TypeId, usize),
    Function {
        params: Vec<TypeId>,
        result: TypeId,
        variadic: bool,
        abi: Abi,
    },
    Struct {
        name: Option<String>,
        fields: Vec<(Option<String>, TypeId, usize)>,
    },
    Union {
        name: Option<String>,
        fields: Vec<(Option<String>, TypeId, usize)>,
    },
}

struct RegistryInner {
    types: HashMap<TypeId, MaybeWeakGcSlot<TypeInfo>>,
    interned: HashMap<TypeKey, TypeId>,
}

/// Owns every C type and interns the structural ones.
///
/// Type creation locks; call and access paths read the type directly.
pub struct TypeRegistry {
    next_id: AtomicU32,
    inner: Mutex<RegistryInner>,
}

impl TypeRegistry {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(TypeId::BUILTIN_COUNT),
            inner: Mutex::new(RegistryInner {
                types: HashMap::new(),
                interned: HashMap::new(),
            }),
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("ffi type registry poisoned")
            .types
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get<'a>(&self, heap: &'a Heap, id: TypeId) -> Option<Tagged<'a, TypeInfo>> {
        if id.index() < TypeKind::BUILTIN_COUNT {
            return heap.known().ffi_types.get(heap, id);
        }
        let inner = self.inner.lock().expect("ffi type registry poisoned");
        inner
            .types
            .get(&id)
            .and_then(|slot| slot.get(heap).as_strong())
    }

    fn reserve_id(&self) -> TypeId {
        TypeId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn probe<'s>(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        key: &TypeKey,
    ) -> Option<Handle<'s, TypeInfo>> {
        let mut inner = self.inner.lock().expect("ffi type registry poisoned");
        let id = *inner.interned.get(key)?;
        if let Some(slot) = inner.types.get(&id)
            && let Some(ty) = slot.get(heap).as_strong()
        {
            return Some(ty.as_handle(scope));
        }
        inner.interned.remove(key);
        inner.types.remove(&id);
        None
    }

    /// Insert `ty` under `key`; returns the existing type if another thread
    /// won the race.
    fn publish<'s>(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        key: TypeKey,
        id: TypeId,
        ty: &Handle<'s, TypeInfo>,
    ) -> Option<Handle<'s, TypeInfo>> {
        let mut inner = self.inner.lock().expect("ffi type registry poisoned");
        if let Some(existing) = inner.interned.get(&key).copied() {
            if let Some(slot) = inner.types.get(&existing)
                && let Some(found) = slot.get(heap).as_strong()
            {
                return Some(found.as_handle(scope));
            }
            inner.interned.remove(&key);
            inner.types.remove(&existing);
        }
        inner
            .types
            .insert(id, MaybeWeakGcSlot::new_weak(ty.as_tagged(heap)));
        inner.interned.insert(key, id);
        None
    }

    pub fn pointer_to<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointee: &Handle<TypeInfo>,
    ) -> Handle<'s, TypeInfo> {
        let pointee_id = pointee.as_tagged(heap).id();
        let key = TypeKey::Pointer(pointee_id);
        if let Some(existing) = self.probe(heap, scope, &key) {
            return existing;
        }
        let id = self.reserve_id();
        let ty = heap.allocate_handle::<TypeInfo>(
            TypeInfoInit {
                kind: TypeKind::Pointer,
                id,
                size: POINTER_SIZE,
                align: POINTER_SIZE,
                flags: 0,
                name: None,
                pointee: Some(pointee.clone()),
                element: None,
                len: 0,
                fields: None,
                params: None,
                result: None,
                cif: 0,
            },
            scope,
        );
        match self.publish(heap, scope, key, id, &ty) {
            Some(existing) => existing,
            None => ty,
        }
    }

    pub fn array_of<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        element: &Handle<TypeInfo>,
        len: usize,
    ) -> Result<Handle<'s, TypeInfo>, TypeError> {
        let element_layout = require_object(element.as_tagged(heap))?;
        let size = element_layout
            .size()
            .checked_mul(len)
            .ok_or(TypeError::SizeOverflow)?;
        let layout = Layout::from_size_align(size, element_layout.align())
            .map_err(|_| TypeError::SizeOverflow)?;
        let key = TypeKey::Array(element.as_tagged(heap).id(), len);
        if let Some(existing) = self.probe(heap, scope, &key) {
            return Ok(existing);
        }
        let id = self.reserve_id();
        let ty = heap.allocate_handle::<TypeInfo>(
            TypeInfoInit {
                kind: TypeKind::Array,
                id,
                size: layout.size(),
                align: layout.align(),
                flags: 0,
                name: None,
                pointee: None,
                element: Some(element.clone()),
                len,
                fields: None,
                params: None,
                result: None,
                cif: 0,
            },
            scope,
        );
        Ok(match self.publish(heap, scope, key, id, &ty) {
            Some(existing) => existing,
            None => ty,
        })
    }

    pub fn function<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        params: &[Handle<TypeInfo>],
        result: &Handle<TypeInfo>,
        variadic: bool,
    ) -> FfiResult<Handle<'s, TypeInfo>> {
        for param in params {
            require_object(param.as_tagged(heap)).map_err(FfiError::from)?;
        }
        if result.as_tagged(heap).kind() != TypeKind::Void {
            require_object(result.as_tagged(heap)).map_err(FfiError::from)?;
        }
        let key = TypeKey::Function {
            params: params
                .iter()
                .map(|param| param.as_tagged(heap).id())
                .collect(),
            result: result.as_tagged(heap).id(),
            variadic,
            abi: Abi::C,
        };
        if let Some(existing) = self.probe(heap, scope, &key) {
            return Ok(existing);
        }
        let plan = CallPlan::new(heap, params, result, variadic)?;
        let id = self.reserve_id();
        let values: Vec<Tagged<Value>> = params
            .iter()
            .map(|param| param.as_tagged(heap).erase())
            .collect();
        let staged = scope.stage(&values);
        drop(values);
        let params_array = heap.allocate_handle::<FixedArray>(staged, scope);
        let raw = Box::into_raw(Box::new(plan));
        let ty = heap.allocate_handle::<TypeInfo>(
            TypeInfoInit {
                kind: TypeKind::Function,
                id,
                size: 0,
                align: 1,
                flags: if variadic {
                    crate::objects::ffi_objects::FLAG_VARIADIC
                } else {
                    0
                },
                name: None,
                pointee: None,
                element: None,
                len: 0,
                fields: None,
                params: Some(params_array),
                result: Some(result.clone()),
                cif: raw as usize,
            },
            scope,
        );
        match self.publish(heap, scope, key, id, &ty) {
            Some(existing) => {
                // Safety: the plan was never shared; the winning type has its own.
                unsafe { drop(Box::from_raw(raw)) };
                Ok(existing)
            }
            None => Ok(ty),
        }
    }

    pub fn define_struct<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Option<&str>,
        fields: &[(Option<&str>, Handle<TypeInfo>)],
    ) -> FfiResult<Handle<'s, TypeInfo>> {
        self.define_composite(vm, heap, scope, name, fields, false)
    }

    pub fn define_union<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Option<&str>,
        fields: &[(Option<&str>, Handle<TypeInfo>)],
    ) -> FfiResult<Handle<'s, TypeInfo>> {
        self.define_composite(vm, heap, scope, name, fields, true)
    }

    fn define_composite<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Option<&str>,
        fields: &[(Option<&str>, Handle<TypeInfo>)],
        union: bool,
    ) -> FfiResult<Handle<'s, TypeInfo>> {
        let mut layouts = Vec::with_capacity(fields.len());
        for (field_name, ty) in fields {
            let layout = require_object(ty.as_tagged(heap)).map_err(FfiError::from)?;
            layouts.push((field_name.map(str::to_string), layout));
        }
        let (field_layouts, layout) = layout_fields(layouts, union).map_err(FfiError::from)?;
        let key = match union {
            true => TypeKey::Union {
                name: name.map(str::to_string),
                fields: fields
                    .iter()
                    .zip(&field_layouts)
                    .map(|((field_name, ty), (_, offset))| {
                        (
                            field_name.map(str::to_string),
                            ty.as_tagged(heap).id(),
                            *offset,
                        )
                    })
                    .collect(),
            },
            false => TypeKey::Struct {
                name: name.map(str::to_string),
                fields: fields
                    .iter()
                    .zip(&field_layouts)
                    .map(|((field_name, ty), (_, offset))| {
                        (
                            field_name.map(str::to_string),
                            ty.as_tagged(heap).id(),
                            *offset,
                        )
                    })
                    .collect(),
            },
        };
        if let Some(existing) = self.probe(heap, scope, &key) {
            return Ok(existing);
        }
        let id = self.reserve_id();
        let mut name_handles = Vec::with_capacity(fields.len());
        for (field_name, _) in fields {
            name_handles.push(
                field_name.map(|field_name| vm.interner().intern_str(heap, scope, field_name)),
            );
        }
        let type_name = name.map(|name| vm.interner().intern_str(heap, scope, name));
        let mut values: Vec<Tagged<Value>> = Vec::with_capacity(fields.len() * 3);
        for (index, ((_, ty), (_, offset))) in fields.iter().zip(&field_layouts).enumerate() {
            let name_value = match &name_handles[index] {
                Some(name) => name.as_tagged(heap).erase(),
                None => heap.known().the_hole.get(heap).erase(),
            };
            values.push(name_value);
            values.push(ty.as_tagged(heap).erase());
            values.push(Smi::new(*offset as i64).into_tagged());
        }
        let staged = scope.stage(&values);
        drop(values);
        let field_array = heap.allocate_handle::<FixedArray>(staged, scope);
        let ty = heap.allocate_handle::<TypeInfo>(
            TypeInfoInit {
                kind: if union {
                    TypeKind::Union
                } else {
                    TypeKind::Struct
                },
                id,
                size: layout.size(),
                align: layout.align(),
                flags: 0,
                name: type_name,
                pointee: None,
                element: None,
                len: 0,
                fields: Some(field_array),
                params: None,
                result: None,
                cif: 0,
            },
            scope,
        );
        Ok(match self.publish(heap, scope, key, id, &ty) {
            Some(existing) => existing,
            None => ty,
        })
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgeVisitable for TypeRegistry {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        let inner = self.inner.lock().expect("ffi type registry poisoned");
        for slot in inner.types.values() {
            visitor.visit(slot.as_raw());
        }
    }
}

impl fmt::Debug for TypeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeRegistry")
            .field("types", &self.len())
            .finish()
    }
}

fn require_object(ty: Tagged<TypeInfo>) -> Result<Layout, TypeError> {
    if ty.kind() == TypeKind::Void {
        return Err(TypeError::VoidField);
    }
    ty.object_layout().ok_or(TypeError::FunctionField)
}

/// C field layout: declaration-order offsets for structs, offset 0 for unions.
fn layout_fields(
    fields: Vec<(Option<String>, Layout)>,
    union: bool,
) -> Result<(Vec<(Option<String>, usize)>, Layout), TypeError> {
    let mut out = Vec::with_capacity(fields.len());
    let mut offset = 0usize;
    let mut max_size = 0usize;
    let mut align = 1usize;
    for (name, field_layout) in fields {
        if let Some(name) = &name
            && out
                .iter()
                .any(|(existing, _): &(Option<String>, usize)| existing.as_deref() == Some(name))
        {
            return Err(TypeError::DuplicateField(name.clone()));
        }
        align = align.max(field_layout.align());
        if union {
            out.push((name, 0));
            max_size = max_size.max(field_layout.size());
            continue;
        }
        offset = offset
            .checked_next_multiple_of(field_layout.align())
            .ok_or(TypeError::SizeOverflow)?;
        out.push((name, offset));
        offset = offset
            .checked_add(field_layout.size())
            .ok_or(TypeError::SizeOverflow)?;
    }
    let size = max_size
        .max(offset)
        .checked_next_multiple_of(align)
        .ok_or(TypeError::SizeOverflow)?;
    let layout = Layout::from_size_align(size, align).map_err(|_| TypeError::SizeOverflow)?;
    Ok((out, layout))
}
