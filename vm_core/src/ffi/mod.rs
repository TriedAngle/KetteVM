//! C FFI: type identity, layout, foreign pointers, libraries, and calls.
//!
//! FFI is a core VM subsystem owned by [`crate::VmCore`]; it is always
//! compiled in and has no runtime id of its own.
//!
//! VM-owned C data lives in VM byte arrays. Types are heap objects and are
//! compared by pointer; the registry is only used to intern a type at
//! creation and to resolve an id for diagnostics.

pub mod bindings;
mod call;
mod error;
mod library;
mod memory;
mod plan;
mod platform;
mod pointer;
mod ty;
mod values;

pub use call::{CallSlot, ValueRead};
pub use error::{FfiError, FfiResult};
pub use plan::CallPlan;
pub use ty::{Abi, TypeError, TypeId, TypeKind, TypeRegistry};

use crate::bootstrap::smi_slot;
use crate::{
    AccessorInfo, AccessorInfoInit, DenseString, EdgeVisitable, FixedArray, GcSlot, Handle,
    HandleScope, Heap, Map, MapInit, MapKind, NativeIndex, SlotFlags, Smi, Tagged, TypeInfo,
    TypeInfoInit, VM, Visitor,
};
use std::sync::Mutex;

/// Types that exist before any guest code runs.
pub struct Builtins {
    slots: [GcSlot<TypeInfo>; TypeKind::BUILTIN_COUNT],
    char_pointer: GcSlot<TypeInfo>,
}

/// FFI state owned by `VmCore`.
pub struct FfiState {
    types: TypeRegistry,
    builtins: Mutex<Option<Builtins>>,
    /// Native getter/setter used by per-type field accessors.
    accessors: Mutex<Option<(NativeIndex, NativeIndex)>>,
}

impl FfiState {
    pub fn types(&self) -> &TypeRegistry {
        &self.types
    }

    /// Create the built-in types and the canonical `char*`. Idempotent.
    pub fn install(
        &self,
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        field_get: NativeIndex,
        field_set: NativeIndex,
    ) {
        *self.accessors.lock().expect("ffi accessors poisoned") = Some((field_get, field_set));
        if self
            .builtins
            .lock()
            .expect("ffi builtins poisoned")
            .is_some()
        {
            return;
        }
        let slots: [GcSlot<TypeInfo>; TypeKind::BUILTIN_COUNT] =
            std::array::from_fn(|_| smi_slot::<TypeInfo>());
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
            self.types
                .register(TypeId::from_i64(index as i64), ty.as_tagged(heap));
            slots[index].init_tagged(ty.as_tagged(heap));
        }
        let char_handle = slots[TypeKind::Char as usize].get(heap).as_handle(scope);
        let char_pointer = self.types.pointer_to(heap, scope, &char_handle);
        let char_pointer_slot = smi_slot::<TypeInfo>();
        char_pointer_slot.init_tagged(char_pointer.as_tagged(heap));
        *self.builtins.lock().expect("ffi builtins poisoned") = Some(Builtins {
            slots,
            char_pointer: char_pointer_slot,
        });
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

    pub fn builtin<'a>(&self, heap: &'a Heap, kind: TypeKind) -> Tagged<'a, TypeInfo> {
        let builtins = self.builtins.lock().expect("ffi builtins poisoned");
        let builtins = builtins
            .as_ref()
            .expect("C FFI builtins are installed during VM setup");
        builtins.slots[kind as usize].get(heap)
    }

    pub fn char_pointer<'a>(&self, heap: &'a Heap) -> Tagged<'a, TypeInfo> {
        let builtins = self.builtins.lock().expect("ffi builtins poisoned");
        let builtins = builtins
            .as_ref()
            .expect("C FFI builtins are installed during VM setup");
        builtins.char_pointer.get(heap)
    }
}

impl Default for FfiState {
    fn default() -> Self {
        Self {
            types: TypeRegistry::new(),
            builtins: Mutex::new(None),
            accessors: Mutex::new(None),
        }
    }
}

impl EdgeVisitable for FfiState {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        if let Some(builtins) = self
            .builtins
            .lock()
            .expect("ffi builtins poisoned")
            .as_ref()
        {
            for slot in &builtins.slots {
                visitor.visit(slot.as_raw());
            }
            visitor.visit(builtins.char_pointer.as_raw());
        }
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
