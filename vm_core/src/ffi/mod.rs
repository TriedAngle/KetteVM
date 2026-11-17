pub mod bindings;
mod call;
mod closure;
mod error;
mod library;
pub mod memory;
mod platform;
mod ty;
mod values;

pub use call::{CallSlot, ValueRead};
pub use closure::{callback, enter_call, leave_call};
pub use error::{FfiError, FfiResult};
pub use ty::{Abi, TypeError, TypeId, TypeKind, TypeRegistry};

use crate::bootstrap::smi_slot;
use crate::objects::FLAG_VARIADIC;
use crate::{
    AccessorInfo, AccessorInfoInit, DenseString, EdgeVisitable, FixedArray, GcSlot, Handle,
    HandleScope, Heap, Map, MapInit, MapKind, NativeIndex, SlotFlags, Smi, Tagged, TypeInfo,
    TypeInfoInit, VM, Value, Visitor,
};
use libffi::middle::{Cif, Type};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// A guest callback registered with C: the guarded entry libffi points at.
pub struct ClosureEntry {
    pub signature: GcSlot<TypeInfo>,
    pub callback: GcSlot<Value>,
    pub ffi: *const FfiState,
}

/// A live libffi closure plus its GC-visible entry.
struct ClosureSlot {
    entry: *mut ClosureEntry,
    /// Kept alive for the process; the code pointer above points into it.
    _closure: Box<libffi::middle::Closure<'static>>,
}

// Safety: the raw pointers are only used while the VM is alive and the FFI
// state is shared between threads; libffi closures are immutable once created.
unsafe impl Send for ClosureSlot {}
unsafe impl Sync for ClosureSlot {}

/// FFI state owned by `VmCore`.
pub struct FfiState {
    types: TypeRegistry,
    /// Native getter/setter used by per-type field accessors.
    accessors: Mutex<Option<(NativeIndex, NativeIndex)>>,
    /// Guest callbacks and the libffi closures that call them.
    closures: Mutex<Vec<ClosureSlot>>,
    /// A callback threw; the C call must return before it is re-raised.
    callback_threw: AtomicBool,
}

impl FfiState {
    pub fn types(&self) -> &TypeRegistry {
        &self.types
    }

    pub fn callback_threw(&self) -> bool {
        self.callback_threw.load(Ordering::Acquire)
    }

    pub fn set_callback_threw(&self) {
        self.callback_threw.store(true, Ordering::Release);
    }

    pub fn take_callback_threw(&self) -> bool {
        self.callback_threw.swap(false, Ordering::AcqRel)
    }

    pub fn register_closure(
        &self,
        entry: *mut ClosureEntry,
        closure: Box<libffi::middle::Closure<'static>>,
    ) {
        self.closures
            .lock()
            .expect("ffi closures poisoned")
            .push(ClosureSlot {
                entry,
                _closure: closure,
            });
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
    ) -> FfiResult<Handle<'s, TypeInfo>> {
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
    ) -> FfiResult<Handle<'s, TypeInfo>> {
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
            closures: Mutex::new(Vec::new()),
            callback_threw: AtomicBool::new(false),
        }
    }
}

impl EdgeVisitable for FfiState {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.types.visit_edges(visitor);
        let closures = self.closures.lock().expect("ffi closures poisoned");
        for slot in closures.iter() {
            // Safety: entries are boxed and live as long as the slot.
            let entry = unsafe { &*slot.entry };
            visitor.visit(entry.signature.as_raw());
            visitor.visit(entry.callback.as_raw());
        }
    }
}

impl std::fmt::Debug for FfiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiState")
            .field("types", &self.types)
            .finish()
    }
}

/// How one parameter is packed for a call.
///
/// Scalars keep their [`TypeKind`]; the rest are the decisions the type cannot
/// express: how a pointer is marshalled, and whether the argument is passed by
/// address.
#[derive(Clone, Copy, Debug)]
pub enum ParamKind {
    /// Passed by value: never `Pointer`, `Struct`, `Union`, `Array`, `Void` or
    /// `Function`.
    Scalar(TypeKind),
    /// `char *`: accepts strings, byte arrays and matching pointers.
    CharPointer,
    /// `char16_t *`: accepts strings and matching pointers.
    Char16Pointer,
    /// Any pointer (`void *`, or a pointee the caller cannot get wrong).
    PointerAny,
    /// A pointer whose pointee type must match this id.
    PointerTo(TypeId),
    /// A struct/union/array passed by value: the argument is its address.
    Aggregate(TypeId),
}

#[derive(Clone, Copy, Debug)]
pub struct ParamPlan {
    pub kind: ParamKind,
    /// Offset of the scalar slot in the call buffer; unused for aggregates.
    pub offset: usize,
}

/// A prepared libffi signature.
pub struct CallPlan {
    pub cif: Cif,
    /// Parameter lowering, computed once when the function type is created.
    pub params: Vec<ParamPlan>,
    pub result_void: bool,
    /// Bytes needed for the scalar argument slots and the result slot.
    pub buffer_len: usize,
    pub result_offset: usize,
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
        let mut plans = Vec::with_capacity(params.len());
        let mut offset = 0;
        for param in params {
            let ty = param.as_tagged(heap);
            lowered.push(lower(heap, ty)?);
            let kind = param_kind(heap, ty)?;
            let slot = match kind {
                ParamKind::Aggregate(_) => 0,
                _ => {
                    let slot = offset;
                    offset += 8;
                    slot
                }
            };
            plans.push(ParamPlan { kind, offset: slot });
        }
        let result_ty = result.as_tagged(heap);
        let result_void = result_ty.kind() == TypeKind::Void;
        let (lowered_result, result_offset, buffer_len) = if result_void {
            (Type::void(), offset, offset)
        } else {
            let result_offset = offset.next_multiple_of(16);
            (
                lower(heap, result_ty)?,
                result_offset,
                result_offset + result_ty.size().max(16),
            )
        };
        let cif = build_cif(lowered, params.len(), lowered_result, variadic);
        Ok(Self {
            cif,
            params: plans,
            result_void,
            buffer_len,
            result_offset,
        })
    }
}

/// Build the cif of a function type, lowering its parameters and result.
pub fn cif_for_signature(heap: &Heap, signature: Tagged<TypeInfo>) -> FfiResult<Cif> {
    let params = signature
        .params(heap)
        .ok_or(FfiError::NotAFunction(signature.id()))?;
    let mut lowered = Vec::with_capacity(params.len());
    for index in 0..params.len() {
        let param = params
            .at(heap, index)
            .get_as::<TypeInfo>(heap)
            .ok_or(FfiError::NotAFunction(signature.id()))?;
        lowered.push(lower(heap, param)?);
    }
    let result = signature
        .result(heap)
        .ok_or(FfiError::NotAFunction(signature.id()))?;
    let lowered_result = if result.kind() == TypeKind::Void {
        Type::void()
    } else {
        lower(heap, result)?
    };
    let variadic = signature.flags() & FLAG_VARIADIC != 0;
    Ok(build_cif(lowered, params.len(), lowered_result, variadic))
}

/// Build the cif for a lowered signature.
fn build_cif(lowered: Vec<Type>, fixed: usize, result: Type, variadic: bool) -> Cif {
    if variadic {
        Cif::new_variadic(lowered, fixed, result)
    } else {
        Cif::new(lowered, result)
    }
}

pub fn param_kind(heap: &Heap, ty: Tagged<TypeInfo>) -> FfiResult<ParamKind> {
    Ok(match ty.kind() {
        TypeKind::Pointer => {
            let pointee = ty.pointee(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            match pointee.kind() {
                TypeKind::Char => ParamKind::CharPointer,
                TypeKind::Char16 => ParamKind::Char16Pointer,
                TypeKind::Void => ParamKind::PointerAny,
                _ => ParamKind::PointerTo(pointee.id()),
            }
        }
        TypeKind::Struct | TypeKind::Union | TypeKind::Array => ParamKind::Aggregate(ty.id()),
        TypeKind::Void | TypeKind::Function => return Err(FfiError::UnsupportedType(ty.id())),
        kind => ParamKind::Scalar(kind),
    })
}

fn lower(heap: &Heap, ty: Tagged<TypeInfo>) -> FfiResult<Type> {
    // libffi cannot represent a zero-size aggregate, and C gives zero-size
    // objects no by-value ABI: a zero-length array is only usable as a struct
    // field, where it contributes no bytes and is not part of the ABI shape.
    if matches!(
        ty.kind(),
        TypeKind::Struct | TypeKind::Union | TypeKind::Array
    ) && ty.size() == 0
    {
        return Err(FfiError::UnsupportedType(ty.id()));
    }
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
                if field.size() > 0 {
                    elements.push(lower(heap, field)?);
                }
                index += 3;
            }
            Type::structure(elements)
        }
        // libffi has no array node: a fixed array is a struct of elements.
        TypeKind::Array => {
            let element = ty.element(heap).ok_or(FfiError::UnsupportedType(ty.id()))?;
            let lowered = lower(heap, element)?;
            Type::structure((0..ty.len()).map(|_| lowered.clone()))
        }
        // libffi has no union node.
        TypeKind::Void | TypeKind::Union | TypeKind::Function => {
            return Err(FfiError::UnsupportedType(ty.id()));
        }
    })
}
