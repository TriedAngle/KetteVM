//! Calling a prepared C function through libffi.

use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId, TypeKind};
use crate::{
    Args, CFunction, CPointer, CPointerFlags, CPointerInit, CValue, Errors, Float, ForeignLibrary,
    Handle, HandleScope, Heap, Smi, Tagged, ThreadState, TypeInfo, Value, VmError,
};
use libffi::middle::{Arg, CodePtr, Ret};

/// Scratch space for one C argument or the return value.
///
/// Backed by `Vec<u128>` so the data is 16-byte aligned; `len` is the number
/// of live bytes. Scalars use 16 bytes; aggregates use their exact size.
pub struct CallSlot {
    data: Vec<u128>,
    len: usize,
}

impl CallSlot {
    pub fn new(len: usize) -> Self {
        Self {
            data: vec![0; len.div_ceil(16).max(1)],
            len,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        // Safety: `data` owns at least `len` initialized bytes.
        unsafe { core::slice::from_raw_parts(self.data.as_ptr().cast::<u8>(), self.len) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // Safety: same as `bytes`.
        unsafe { core::slice::from_raw_parts_mut(self.data.as_mut_ptr().cast::<u8>(), self.len) }
    }

    fn put(&mut self, bytes: &[u8]) {
        self.bytes_mut()[..bytes.len()].copy_from_slice(bytes);
    }
}

/// A raw C value (call result or field read) plus its boxed type.
pub struct RawValue<'s> {
    pub(super) slot: CallSlot,
    pub(super) ty: Handle<'s, TypeInfo>,
}

/// A C value read: a scalar waiting to be boxed, or an aggregate view.
pub enum ValueRead<'s> {
    Raw(RawValue<'s>),
    View(Handle<'s, CValue>),
}

impl FfiState {
    /// Run the C call. No guest objects are allocated here.
    pub fn call_raw<'s>(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        callee: &Handle<'_, Value>,
        args: Args,
    ) -> FfiResult<RawValue<'s>> {
        let (signature, address) = {
            let function = callee
                .as_tagged(heap)
                .get_as::<CFunction>(heap)
                .ok_or(FfiError::NotCallable)?;
            let target = function.target(heap);
            target.check_live(heap)?;
            (function.signature(heap), target.address(heap))
        };
        let plan = signature.plan();
        let params = signature
            .params(heap)
            .ok_or(FfiError::NotAFunction(signature.id()))?;
        if args.len() != params.len() {
            return Err(FfiError::ArgCount {
                expected: params.len(),
                found: args.len(),
            });
        }

        let mut slots = Vec::with_capacity(params.len());
        for index in 0..params.len() {
            let param = param_at(heap, params, index)?;
            slots.push(CallSlot::new(param.size().max(8)));
        }
        for (index, slot) in slots.iter_mut().enumerate() {
            let param = param_at(heap, params, index)?;
            let value = args.get_handle(heap, index).as_tagged(heap);
            convert_arg(param, value, heap, slot)?;
        }

        // `bytes()` is a slice; take a thin reference to its first byte so the
        // argument pointer is unambiguously the data address.
        let ffi_args: Vec<Arg<'_>> = slots
            .iter()
            .map(|slot| Arg::new(&slot.bytes()[0]))
            .collect();
        let result_ty = signature
            .result(heap)
            .ok_or(FfiError::NotAFunction(signature.id()))?;
        let mut result = CallSlot::new(result_ty.size().max(16));
        let code = CodePtr(address as *mut core::ffi::c_void);
        // libffi writes the result into the pointed-to bytes, so point it at
        // the slot's data, not at the `CallSlot` header.
        let ret_bytes = &mut result.bytes_mut()[0];
        // Safety: the pointer was declared with this exact C signature and
        // every argument slot matches the lowered parameter types.
        unsafe {
            if result_ty.kind() == TypeKind::Void {
                plan.cif.call_return_into(code, &ffi_args, Ret::void());
            } else {
                plan.cif
                    .call_return_into(code, &ffi_args, Ret::new(ret_bytes));
            }
        }

        check_result(result_ty, &result)?;
        Ok(RawValue {
            slot: result,
            ty: scope.handle(result_ty),
        })
    }

    fn read_value<'a>(
        &self,
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        ty: &Handle<TypeInfo>,
        bytes: &[u8],
        library: Option<&Handle<'_, ForeignLibrary>>,
    ) -> Tagged<'a, Value> {
        let (kind, size) = {
            let info = ty.as_tagged(heap);
            (info.kind(), info.size())
        };
        match kind {
            TypeKind::Void => heap.known().undefined.get(heap).erase(),
            TypeKind::Bool => bool_object(heap, bytes[0] != 0),
            TypeKind::I8 => smi_value(bytes[0] as i8 as i64),
            TypeKind::U8 | TypeKind::Char => smi_value(bytes[0] as i64),
            TypeKind::I16 => smi_value(i16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64),
            TypeKind::U16 => smi_value(u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64),
            TypeKind::I32 => smi_value(i32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64),
            TypeKind::U32 => smi_value(u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64),
            TypeKind::I64 => smi_value(i64::from_ne_bytes(bytes[..8].try_into().unwrap())),
            TypeKind::U64 => smi_value(u64::from_ne_bytes(bytes[..8].try_into().unwrap()) as i64),
            TypeKind::F32 => {
                let value = f32::from_ne_bytes(bytes[..4].try_into().unwrap());
                float_object(heap, scope, value as f64)
            }
            TypeKind::F64 => {
                let value = f64::from_ne_bytes(bytes[..8].try_into().unwrap());
                float_object(heap, scope, value)
            }
            TypeKind::Pointer => {
                let width = core::mem::size_of::<usize>();
                let address = usize::from_ne_bytes(bytes[..width].try_into().unwrap());
                let pointee = {
                    let ty = ty.as_tagged(heap);
                    ty.pointee(heap).expect("pointer type without pointee")
                };
                let pointee = scope.handle(pointee);
                let pointer = heap.allocate_handle::<CPointer>(
                    CPointerInit {
                        pointee,
                        address,
                        flags: CPointerFlags::BORROWED,
                        library: None,
                        owner: None,
                    },
                    scope,
                );
                if let Some(library) = library {
                    pointer
                        .as_tagged(heap)
                        .set_library(heap, library.as_tagged(heap));
                }
                pointer.as_tagged(heap).erase()
            }
            TypeKind::Struct | TypeKind::Union | TypeKind::Array => {
                let value = self
                    .value(heap, scope, ty)
                    .expect("C FFI aggregate result allocation");
                let base = value
                    .as_tagged(heap)
                    .base(heap)
                    .expect("C value without storage");
                memory::copy_in(base, &bytes[..size]);
                value.as_tagged(heap).erase()
            }
            TypeKind::Function => heap.known().undefined.get(heap).erase(),
        }
    }

    /// Box a raw value as a guest value.
    pub fn read_raw<'a>(
        &self,
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        raw: &RawValue<'_>,
        library: Option<&Handle<'_, ForeignLibrary>>,
    ) -> Tagged<'a, Value> {
        self.read_value(heap, scope, &raw.ty, raw.slot.bytes(), library)
    }

    /// `call_raw` + `read_raw`, with errors materialized as guest errors.
    pub fn call<'a>(
        &self,
        vm: &'a crate::VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        callee: &Handle<'_, Value>,
        args: Args,
    ) -> crate::Result<Tagged<'a, Value>> {
        Ok(state.handle_scope(|scope| {
            let library = {
                let function = callee.as_tagged(heap).get_as::<CFunction>(heap);
                function
                    .and_then(|function| function.library(heap))
                    .map(|library| scope.handle(library))
            };
            let raw = match self.call_raw(heap, &scope, callee, args) {
                Ok(raw) => raw,
                Err(error) => {
                    Errors::throw_with(vm, heap, state, VmError::FfiError, &error.to_string());
                    return Err(crate::Thrown);
                }
            };
            Ok(self.read_raw(heap, &scope, &raw, library.as_ref()))
        })?)
    }
}

impl FfiState {
    /// Read `value.field` as a guest value. Aggregate fields are views.
    pub fn value_get_field<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
        name: &str,
    ) -> FfiResult<ValueRead<'s>> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (field_ty, offset) = field_of(heap, ty, name)?;
        let field_ty = scope.handle(field_ty);
        self.value_read(heap, scope, value, &field_ty, offset)
    }

    /// Read `value[index]` as a guest value. Aggregate elements are views.
    pub fn value_get_index<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
        index: usize,
    ) -> FfiResult<ValueRead<'s>> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (element, offset) = element_of(heap, ty, index)?;
        let element = scope.handle(element);
        self.value_read(heap, scope, value, &element, offset)
    }

    /// Write `value.field` from a guest value.
    pub fn value_set_field(
        &self,
        heap: &Heap,
        value: &Handle<CValue>,
        name: &str,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (field_ty, offset) = field_of(heap, ty, name)?;
        self.value_write(heap, value, field_ty, offset, guest)
    }

    /// Write `value[index]` from a guest value.
    pub fn value_set_index(
        &self,
        heap: &Heap,
        value: &Handle<CValue>,
        index: usize,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (element, offset) = element_of(heap, ty, index)?;
        self.value_write(heap, value, element, offset, guest)
    }

    pub fn value_read<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
        ty: &Handle<'s, TypeInfo>,
        offset: usize,
    ) -> FfiResult<ValueRead<'s>> {
        let (kind, size) = {
            let info = ty.as_tagged(heap);
            (info.kind(), info.size())
        };
        if matches!(kind, TypeKind::Struct | TypeKind::Union | TypeKind::Array) {
            let view = self.view(heap, scope, value, ty, offset);
            return Ok(ValueRead::View(view));
        }
        let base = value
            .as_tagged(heap)
            .base(heap)
            .ok_or(FfiError::InvalidArgument("C value has no storage"))?;
        let mut slot = CallSlot::new(size.max(8));
        memory::copy_out_at(base, offset, &mut slot.bytes_mut()[..size]);
        Ok(ValueRead::Raw(RawValue {
            slot,
            ty: ty.clone(),
        }))
    }

    pub fn value_write(
        &self,
        heap: &Heap,
        value: &Handle<CValue>,
        ty: Tagged<TypeInfo>,
        offset: usize,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let base = value
            .as_tagged(heap)
            .base(heap)
            .ok_or(FfiError::InvalidArgument("C value has no storage"))?;
        let size = ty.size();
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(ty, guest, heap, &mut slot)?;
        memory::copy_in_at(base, offset, &slot.bytes()[..size]);
        Ok(())
    }
}

fn param_at<'a>(
    heap: &'a Heap,
    params: Tagged<'a, crate::FixedArray>,
    index: usize,
) -> FfiResult<Tagged<'a, TypeInfo>> {
    params
        .at(heap, index)
        .get_as::<TypeInfo>(heap)
        .ok_or(FfiError::NotAFunction(TypeId::VOID))
}

impl FfiState {
    /// Read `pointer[index]` as the pointee type.
    pub fn pointer_read_raw<'s>(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        index: usize,
    ) -> FfiResult<RawValue<'s>> {
        let pointee = pointer.as_tagged(heap).pointee(heap);
        pointer.as_tagged(heap).check_live(heap)?;
        let size = pointee.size();
        let address = pointer
            .as_tagged(heap)
            .address(heap)
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)?;
        let mut slot = CallSlot::new(size.max(8));
        memory::copy_out_at(address, 0, &mut slot.bytes_mut()[..size]);
        Ok(RawValue {
            slot,
            ty: scope.handle(pointee),
        })
    }

    /// Write `pointer[index]` as the pointee type.
    pub fn pointer_write(
        &self,
        heap: &Heap,
        pointer: &Handle<CPointer>,
        index: usize,
        value: Tagged<Value>,
    ) -> FfiResult<()> {
        let pointee = pointer.as_tagged(heap).pointee(heap);
        pointer.as_tagged(heap).check_live(heap)?;
        let size = pointee.size();
        let address = pointer
            .as_tagged(heap)
            .address(heap)
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)?;
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(pointee, value, heap, &mut slot)?;
        memory::copy_in_at(address, 0, &slot.bytes()[..size]);
        Ok(())
    }
}

fn element_of<'a>(
    heap: &'a Heap,
    aggregate: Tagged<'a, TypeInfo>,
    index: usize,
) -> FfiResult<(Tagged<'a, TypeInfo>, usize)> {
    if aggregate.kind() != TypeKind::Array {
        return Err(FfiError::NotAnArray(aggregate.id()));
    }
    let element = aggregate
        .element(heap)
        .ok_or(FfiError::NotAnArray(aggregate.id()))?;
    if index >= aggregate.len() {
        return Err(FfiError::IndexOutOfBounds);
    }
    Ok((element, index * element.size()))
}

fn field_of<'a>(
    heap: &'a Heap,
    aggregate: Tagged<'a, TypeInfo>,
    name: &str,
) -> FfiResult<(Tagged<'a, TypeInfo>, usize)> {
    match aggregate.kind() {
        TypeKind::Struct | TypeKind::Union => aggregate
            .field(heap, name)
            .ok_or_else(|| FfiError::UnknownField(name.to_string())),
        _ => Err(FfiError::NotAnObject(aggregate.id())),
    }
}

fn check_result(result: Tagged<TypeInfo>, slot: &CallSlot) -> FfiResult<()> {
    let bytes = slot.bytes();
    match result.kind() {
        TypeKind::I64 => {
            let value = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
            if Smi::in_range(value) {
                Ok(())
            } else {
                Err(FfiError::IntegerOutOfRange)
            }
        }
        TypeKind::U64 => {
            let value = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
            if value <= Smi::MAX as u64 {
                Ok(())
            } else {
                Err(FfiError::IntegerOutOfRange)
            }
        }
        _ => Ok(()),
    }
}

fn convert_arg(
    ty: Tagged<TypeInfo>,
    value: Tagged<Value>,
    heap: &Heap,
    slot: &mut CallSlot,
) -> FfiResult<()> {
    match ty.kind() {
        TypeKind::Bool => {
            let bit = u8::from(bool_from(heap, value)?);
            slot.put(&bit.to_ne_bytes());
        }
        TypeKind::I8 => slot.put(&i8::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::U8 | TypeKind::Char => {
            slot.put(&u8::try_from(int_from(heap, value)?)?.to_ne_bytes())
        }
        TypeKind::I16 => slot.put(&i16::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::U16 => slot.put(&u16::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::I32 => slot.put(&i32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::U32 => slot.put(&u32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::I64 => slot.put(&int_from(heap, value)?.to_ne_bytes()),
        TypeKind::U64 => slot.put(&u64::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::F32 => slot.put(&(float_from(heap, value)? as f32).to_ne_bytes()),
        TypeKind::F64 => slot.put(&float_from(heap, value)?.to_ne_bytes()),
        TypeKind::Pointer => {
            let address = pointer_from(ty, value, heap)?;
            slot.put(&address.to_ne_bytes());
        }
        TypeKind::Struct | TypeKind::Union | TypeKind::Array => {
            let buffer = value
                .get_as::<CValue>(heap)
                .ok_or(FfiError::InvalidArgument("expected a C buffer"))?;
            let found = buffer.c_type(heap);
            if !found.ptr_eq(ty) {
                return Err(FfiError::MismatchedType {
                    expected: ty.id(),
                    found: found.id(),
                });
            }
            let base = buffer
                .base(heap)
                .ok_or(FfiError::InvalidArgument("C value has no storage"))?;
            let size = ty.size();
            memory::copy_out(base, &mut slot.bytes_mut()[..size]);
        }
        TypeKind::Void | TypeKind::Function => return Err(FfiError::UnsupportedType(ty.id())),
    }
    Ok(())
}

fn int_from(heap: &Heap, value: Tagged<Value>) -> FfiResult<i64> {
    if let Some(smi) = value.to_i64() {
        return Ok(smi);
    }
    if let Some(float) = value.get_as::<Float>(heap) {
        let number = float.value.get();
        if number.is_finite()
            && number.fract() == 0.0
            && number >= i64::MIN as f64
            && number <= i64::MAX as f64
        {
            return Ok(number as i64);
        }
    }
    Err(FfiError::InvalidArgument("expected an integer"))
}

fn float_from(heap: &Heap, value: Tagged<Value>) -> FfiResult<f64> {
    if let Some(smi) = value.to_i64() {
        return Ok(smi as f64);
    }
    if let Some(float) = value.get_as::<Float>(heap) {
        return Ok(float.value.get());
    }
    Err(FfiError::InvalidArgument("expected a number"))
}

fn bool_from(heap: &Heap, value: Tagged<Value>) -> FfiResult<bool> {
    if value.ptr_eq(heap.known().true_object.get(heap).erase()) {
        return Ok(true);
    }
    if value.ptr_eq(heap.known().false_object.get(heap).erase()) {
        return Ok(false);
    }
    match value.to_i64() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(FfiError::InvalidArgument("expected a boolean")),
    }
}

fn pointer_from(ty: Tagged<TypeInfo>, value: Tagged<Value>, heap: &Heap) -> FfiResult<usize> {
    if value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
    {
        return Ok(0);
    }
    let expected = ty
        .pointee(heap)
        .ok_or(FfiError::InvalidArgument("pointer type without pointee"))?;
    if let Some(c_value) = value.get_as::<CValue>(heap) {
        let actual = c_value.c_type(heap);
        let actual = if actual.kind() == TypeKind::Array {
            actual
                .element(heap)
                .ok_or(FfiError::InvalidArgument("array type without element"))?
        } else {
            actual
        };
        if !actual.ptr_eq(expected) && expected.kind() != TypeKind::Void {
            return Err(FfiError::MismatchedType {
                expected: expected.id(),
                found: actual.id(),
            });
        }
        return c_value
            .base(heap)
            .ok_or(FfiError::InvalidArgument("C value has no storage"));
    }
    let pointer = value
        .get_as::<CPointer>(heap)
        .ok_or(FfiError::InvalidArgument("expected a C pointer"))?;
    pointer.check_live(heap)?;
    let actual = pointer.pointee(heap);
    if !actual.ptr_eq(expected) && actual.kind() != TypeKind::Void {
        return Err(FfiError::MismatchedType {
            expected: expected.id(),
            found: actual.id(),
        });
    }
    Ok(pointer.address(heap))
}

fn smi_value(value: i64) -> Tagged<'static, Value> {
    Smi::new(value).into_tagged()
}

fn bool_object<'a>(heap: &'a Heap, value: bool) -> Tagged<'a, Value> {
    let object = if value {
        heap.known().true_object.get(heap)
    } else {
        heap.known().false_object.get(heap)
    };
    object.erase()
}

fn float_object<'a>(heap: &'a mut Heap, scope: &HandleScope<'_>, value: f64) -> Tagged<'a, Value> {
    let float = heap.allocate_handle::<Float>(value, scope);
    float.as_tagged(heap).erase()
}
