pub mod bindings;
mod call;
mod error;
mod library;
pub mod memory;
mod platform;
mod ty;
mod values;

pub use call::{CallSlot, ValueRead};
pub use error::{FfiError, FfiResult};
pub use ty::{Abi, TypeError, TypeId, TypeKind, TypeRegistry};

use crate::bootstrap::smi_slot;
use crate::{
    AccessorInfo, AccessorInfoInit, DenseString, EdgeVisitable, FixedArray, GcSlot, Handle,
    HandleScope, Heap, Map, MapInit, MapKind, NativeIndex, SlotFlags, Smi, Tagged, TypeInfo,
    TypeInfoInit, VM, Visitor,
};
use libffi::middle::{Cif, Type};
use std::sync::Mutex;

pub struct FfiTypes {
    slots: [GcSlot<TypeInfo>; TypeKind::BUILTIN_COUNT],
}

impl FfiTypes {
    pub fn uninit() -> Self {
        Self {
            slots: std::array::from_fn(|_| smi_slot::<TypeInfo>()),
        }
    }

    pub fn get<'a>(&self, heap: &'a Heap, id: TypeId) -> Option<Tagged<'a, TypeInfo>> {
        self.slots.get(id.index()).map(|slot| slot.get(heap))
    }

    pub fn set(&self, id: TypeId, ty: Tagged<TypeInfo>) {
        self.slots
            .get(id.index())
            .expect("builtin FFI type id")
            .init_tagged(ty);
    }

    pub fn visit_edges(&self, visitor: &mut dyn Visitor) {
        for slot in &self.slots {
            visitor.visit(slot.as_raw());
        }
    }
}

/// FFI state owned by `VmCore`.
pub struct FfiState {
    types: TypeRegistry,
    /// Native getter/setter used by per-type field accessors.
    accessors: Mutex<Option<(NativeIndex, NativeIndex)>>,
}

impl FfiState {
    pub fn types(&self) -> &TypeRegistry {
        &self.types
    }

    /// Create the built-in types; run once during VM setup.
    pub fn install(
        &self,
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        field_get: NativeIndex,
        field_set: NativeIndex,
    ) {
        *self.accessors.lock().expect("ffi accessors poisoned") = Some((field_get, field_set));
        for index in 0..TypeKind::BUILTIN_COUNT {
            let kind = TypeKind::from_u8(index as u8);
            let (size, align) = kind.builtin_layout();
            let ty = heap.allocate_handle::<TypeInfo>(
                TypeInfoInit {
                    kind,
                    id: TypeId::from_i64(index as i64),
                    size,
                    align,
                    flags: 0,
                    name: None,
                    pointee: None,
                    element: None,
                    len: 0,
                    fields: None,
                    params: None,
                    result: None,
                    cif: 0,
                },
                scope,
            );
            heap.known()
                .ffi_types
                .set(TypeId::from_i64(index as i64), ty.as_tagged(heap));
        }
    }

    pub fn define_struct<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Option<&str>,
        fields: &[(Option<&str>, Handle<TypeInfo>)],
    ) -> crate::ffi::FfiResult<Handle<'s, TypeInfo>> {
        let ty = self.types.define_struct(vm, heap, scope, name, fields)?;
        self.ensure_value_map(heap, scope, &ty);
        Ok(ty)
    }

    pub fn define_union<'s>(
        &self,
        vm: &VM,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        name: Option<&str>,
        fields: &[(Option<&str>, Handle<TypeInfo>)],
    ) -> crate::ffi::FfiResult<Handle<'s, TypeInfo>> {
        let ty = self.types.define_union(vm, heap, scope, name, fields)?;
        self.ensure_value_map(heap, scope, &ty);
        Ok(ty)
    }

    /// Build the per-type map with one native accessor per named field.
    fn ensure_value_map(&self, heap: &mut Heap, scope: &HandleScope<'_>, ty: &Handle<TypeInfo>) {
        let (kind, fields, already) = {
            let info = ty.as_tagged(heap);
            (
                info.kind(),
                info.fields(heap),
                info.value_map(heap).is_some(),
            )
        };
        if already || !matches!(kind, TypeKind::Struct | TypeKind::Union) {
            return;
        }
        let Some(fields) = fields else { return };
        let Some((getter, setter)) = *self.accessors.lock().expect("ffi accessors poisoned") else {
            return;
        };

        let mut entries: Vec<(Handle<DenseString>, Handle<TypeInfo>, usize)> = Vec::new();
        let mut index = 0;
        while index + 2 < fields.len() {
            let name = fields
                .at(heap, index)
                .get_as::<DenseString>(heap)
                .map(|name| scope.handle(name));
            let field_ty = fields
                .at(heap, index + 1)
                .get_as::<TypeInfo>(heap)
                .map(|ty| scope.handle(ty));
            let offset = fields.at(heap, index + 2).to_i64().unwrap_or(0) as usize;
            if let (Some(name), Some(field_ty)) = (name, field_ty) {
                entries.push((name, field_ty, offset));
            }
            index += 3;
        }

        let mut descriptors = Vec::with_capacity(entries.len());
        for (name, field_ty, offset) in entries {
            let slot_name = {
                let erased = name.as_tagged(heap).erase();
                scope.handle(erased.as_name())
            };
            let data = {
                let values = [
                    Smi::new(offset as i64).into_tagged(),
                    field_ty.as_tagged(heap).erase(),
                ];
                let staged = scope.stage(&values);
                heap.allocate_handle::<FixedArray>(staged, scope)
            };
            let info = heap.allocate_handle::<AccessorInfo>(
                AccessorInfoInit {
                    getter: Some(getter),
                    setter: Some(setter),
                    data: Some(data.erase()),
                },
                scope,
            );
            descriptors.push((
                slot_name,
                SlotFlags::ACCESSOR
                    .union(SlotFlags::WRITABLE)
                    .union(SlotFlags::ENUMERABLE)
                    .union(SlotFlags::CONFIGURABLE),
                info.erase(),
            ));
        }

        let prototype = {
            let map = heap.known().c_value_map.get(heap);
            scope.handle(map.prototype.get(heap))
        };
        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::C_VALUE.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &descriptors,
                prototype,
            },
            scope,
        );
        ty.as_tagged(heap).set_value_map(heap, map.as_tagged(heap));
    }
}

impl Default for FfiState {
    fn default() -> Self {
        Self {
            types: TypeRegistry::new(),
            accessors: Mutex::new(None),
        }
    }
}

impl EdgeVisitable for FfiState {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.types.visit_edges(visitor);
    }
}

impl std::fmt::Debug for FfiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiState")
            .field("types", &self.types)
            .finish()
    }
}

/// A prepared libffi signature.
pub struct CallPlan {
    pub cif: Cif,
}

// Safety: an `ffi_cif` is built once and is read-only during `ffi_call`;
// libffi does not mutate a prepared cif while calls are in flight.
unsafe impl Send for CallPlan {}
unsafe impl Sync for CallPlan {}

impl CallPlan {
    pub fn new(
        heap: &Heap,
        params: &[Handle<TypeInfo>],
        result: &Handle<TypeInfo>,
        variadic: bool,
    ) -> FfiResult<Self> {
        let mut lowered = Vec::with_capacity(params.len());
        for param in params {
            lowered.push(lower(heap, param.as_tagged(heap))?);
        }
        let result_ty = result.as_tagged(heap);
        let lowered_result = if result_ty.kind() == TypeKind::Void {
            Type::void()
        } else {
            lower(heap, result_ty)?
        };
        let fixed = params.len();
        let cif = if variadic {
            Cif::new_variadic(lowered, fixed, lowered_result)
        } else {
            Cif::new(lowered, lowered_result)
        };
        Ok(Self { cif })
    }
}

fn lower(heap: &Heap, ty: Tagged<TypeInfo>) -> FfiResult<Type> {
    Ok(match ty.kind() {
        TypeKind::Bool | TypeKind::U8 | TypeKind::Char => Type::u8(),
        TypeKind::I8 => Type::i8(),
        TypeKind::U16 | TypeKind::Char16 => Type::u16(),
        TypeKind::I16 => Type::i16(),
        TypeKind::U32 => Type::u32(),
        TypeKind::I32 => Type::i32(),
        TypeKind::U64 => Type::u64(),
        TypeKind::I64 => Type::i64(),
        TypeKind::F32 => Type::f32(),
        TypeKind::F64 => Type::f64(),
        TypeKind::Pointer => Type::pointer(),
        TypeKind::Struct => {
            let fields = ty.fields(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            let mut elements = Vec::with_capacity(fields.len() / 3);
            let mut index = 0;
            while index + 2 < fields.len() {
                let field = fields
                    .at(heap, index + 1)
                    .get_as::<TypeInfo>(heap)
                    .ok_or(FfiError::UnsupportedType(ty.id()))?;
                elements.push(lower(heap, field)?);
                index += 3;
            }
            Type::structure(elements)
        }
        // libffi has no array node: a fixed array is a struct of elements.
        TypeKind::Array if ty.len() > 0 => {
            let element = ty.element(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            let lowered = lower(heap, element)?;
            let elements = (0..ty.len()).map(|_| lowered.clone()).collect::<Vec<_>>();
            Type::structure(elements)
        }
        // libffi has no union node, and a zero-length array has no layout.
        TypeKind::Void | TypeKind::Union | TypeKind::Array | TypeKind::Function => {
            return Err(FfiError::UnsupportedType(ty.id()));
        }
    })
}
