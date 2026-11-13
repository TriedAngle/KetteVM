//! Calling a prepared C function through libffi.

use crate::ffi::memory;
use crate::ffi::plan::CallPlan;
use crate::ffi::{CType, FfiError, FfiResult, FfiState, TypeId, TypeTable};
use crate::{
    Args, CBuffer, CFunction, CPointer, CPointerFlags, CPointerInit, Errors, Float, ForeignLibrary,
    Handle, HandleScope, Heap, Smi, Tagged, ThreadState, Value, VmError,
};
use libffi::middle::{Arg, CodePtr, Ret};
use std::sync::Arc;

/// Scratch space for one C argument or the return value.
///
/// Backed by `Vec<u128>` so the data is 16-byte aligned; `len` is the number
/// of live bytes. Scalars use 16 bytes, aggregates use their exact size.
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

/// A raw C value (call result or field read) before boxing.
pub struct RawValue {
    pub(super) slot: CallSlot,
    pub(super) type_id: TypeId,
}

fn layout_size(types: &TypeTable, id: TypeId) -> usize {
    types.layout(id).map(|layout| layout.size()).unwrap_or(0)
}

impl FfiState {
    fn plan_for(&self, signature: TypeId) -> FfiResult<Arc<CallPlan>> {
        if let Some(plan) = self.plans().get(&signature) {
            return Ok(plan.clone());
        }
        let plan = Arc::new({
            let types = self.types();
            CallPlan::new(&types, signature)?
        });
        self.plans_mut().insert(signature, plan.clone());
        Ok(plan)
    }

    /// Run the C call. No guest objects are allocated here.
    pub fn call_raw(
        &self,
        heap: &Heap,
        callee: &Handle<'_, Value>,
        args: Args,
    ) -> FfiResult<RawValue> {
        let (signature, address) = {
            let function = callee
                .as_tagged(heap)
                .get_as::<CFunction>(heap)
                .ok_or(FfiError::NotCallable)?;
            let target = function.target(heap);
            target.check_live(heap)?;
            (function.signature(), target.address())
        };

        let plan = self.plan_for(signature)?;
        if args.len() != plan.params.len() {
            return Err(FfiError::ArgCount {
                expected: plan.params.len(),
                found: args.len(),
            });
        }

        let types = self.types();
        let mut slots = Vec::with_capacity(plan.params.len());
        for &param in &plan.params {
            slots.push(CallSlot::new(layout_size(&types, param).max(8)));
        }
        for (index, &param) in plan.params.iter().enumerate() {
            let value = args.get_handle(heap, index).as_tagged(heap);
            convert_arg(&types, param, value, heap, &mut slots[index])?;
        }

        // `bytes()` is a slice; take a thin reference to its first byte so the
        // argument pointer is unambiguously the data address.
        let ffi_args: Vec<Arg<'_>> = slots
            .iter()
            .map(|slot| Arg::new(&slot.bytes()[0]))
            .collect();
        let mut result = CallSlot::new(layout_size(&types, plan.result).max(16));
        let code = CodePtr(address as *mut core::ffi::c_void);
        // libffi writes the result into the pointed-to bytes, so point it at
        // the slot's data, not at the `CallSlot` header.
        let ret_bytes = &mut result.bytes_mut()[0];
        // Safety: the pointer was declared with this exact C signature and
        // every argument slot matches the lowered parameter types.
        unsafe {
            if plan.result == TypeId::VOID {
                plan.cif.call_return_into(code, &ffi_args, Ret::void());
            } else {
                plan.cif
                    .call_return_into(code, &ffi_args, Ret::new(ret_bytes));
            }
        }

        check_result(&types, plan.result, &result)?;
        Ok(RawValue {
            slot: result,
            type_id: plan.result,
        })
    }

    fn read_value<'a>(
        &self,
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        c_type: &CType,
        type_id: TypeId,
        bytes: &[u8],
        library: Option<&Handle<'_, ForeignLibrary>>,
    ) -> Tagged<'a, Value> {
        match c_type {
            CType::Void => heap.known().undefined.get(heap).erase(),
            CType::Bool => bool_object(heap, bytes[0] != 0),
            CType::I8 => smi_value(bytes[0] as i8 as i64),
            CType::U8 | CType::Char => smi_value(bytes[0] as i64),
            CType::I16 => smi_value(i16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64),
            CType::U16 => smi_value(u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64),
            CType::I32 => smi_value(i32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64),
            CType::U32 => smi_value(u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64),
            CType::I64 => smi_value(i64::from_ne_bytes(bytes[..8].try_into().unwrap())),
            CType::U64 => smi_value(u64::from_ne_bytes(bytes[..8].try_into().unwrap()) as i64),
            CType::F32 => {
                let value = f32::from_ne_bytes(bytes[..4].try_into().unwrap());
                float_object(heap, scope, value as f64)
            }
            CType::F64 => {
                let value = f64::from_ne_bytes(bytes[..8].try_into().unwrap());
                float_object(heap, scope, value)
            }
            CType::Pointer(_) => {
                let width = core::mem::size_of::<usize>();
                let address = usize::from_ne_bytes(bytes[..width].try_into().unwrap());
                let pointer = heap.allocate_handle::<CPointer>(
                    CPointerInit {
                        type_id,
                        address,
                        flags: CPointerFlags::BORROWED,
                        library: None,
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
            CType::Struct(_) | CType::Union(_) | CType::Array(_) => {
                let size = c_type.layout().map(|layout| layout.size()).unwrap_or(0);
                let buffer = self
                    .buffer(heap, scope, type_id)
                    .expect("C FFI aggregate result allocation");
                let storage = buffer.as_tagged(heap).storage(heap);
                memory::copy_in(storage.address(), &bytes[..size]);
                buffer.as_tagged(heap).erase()
            }
            CType::Function(_) => heap.known().undefined.get(heap).erase(),
        }
    }

    /// Box a raw value as a guest value. Infallible: validation happened
    /// when the value was produced, and aggregate allocation is treated as
    /// fatal on OOM.
    pub fn read_raw<'a>(
        &self,
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        raw: &RawValue,
        library: Option<&Handle<'_, ForeignLibrary>>,
    ) -> Tagged<'a, Value> {
        let c_type = self.types().get(raw.type_id).clone();
        self.read_value(heap, scope, &c_type, raw.type_id, raw.slot.bytes(), library)
    }

    /// `call_raw` + `box_result`, with errors materialized as guest errors.
    pub fn call<'a>(
        &self,
        vm: &'a crate::VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        callee: &Handle<'_, Value>,
        args: Args,
    ) -> crate::Result<Tagged<'a, Value>> {
        let raw = match self.call_raw(heap, callee, args) {
            Ok(raw) => raw,
            Err(error) => {
                Errors::throw_with(vm, heap, state, VmError::FfiError, &error.to_string());
                return Err(crate::Thrown);
            }
        };
        Ok(state.handle_scope(|scope| {
            let library = {
                let function = callee.as_tagged(heap).get_as::<CFunction>(heap);
                function
                    .and_then(|function| function.library(heap))
                    .map(|library| scope.handle(library))
            };
            self.read_raw(heap, &scope, &raw, library.as_ref())
        }))
    }
}

impl FfiState {
    /// Read one struct/union field into raw bytes.
    pub fn buffer_get_raw(
        &self,
        heap: &Heap,
        buffer: &Handle<CBuffer>,
        name: &str,
    ) -> FfiResult<RawValue> {
        let types = self.types();
        let (field_ty, offset) = field_of(&types, buffer.as_tagged(heap).c_type(), name)?;
        let size = layout_size(&types, field_ty);
        let storage = buffer.as_tagged(heap).storage(heap);
        storage.check_live(heap)?;
        let mut slot = CallSlot::new(size.max(8));
        memory::copy_out_at(storage.address(), offset, &mut slot.bytes_mut()[..size]);
        Ok(RawValue {
            slot,
            type_id: field_ty,
        })
    }

    /// Read `pointer[index]` as the pointee type.
    pub fn pointer_read_raw(
        &self,
        heap: &Heap,
        pointer: &Handle<CPointer>,
        index: usize,
    ) -> FfiResult<RawValue> {
        let types = self.types();
        let ty = pointer.as_tagged(heap).c_type();
        let CType::Pointer(pointee) = types.get(ty) else {
            return Err(FfiError::NotAPointer(ty));
        };
        let pointee = *pointee;
        pointer.as_tagged(heap).check_live(heap)?;
        let size = layout_size(&types, pointee);
        let address = pointer
            .as_tagged(heap)
            .address()
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
            type_id: pointee,
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
        let types = self.types();
        let ty = pointer.as_tagged(heap).c_type();
        let CType::Pointer(pointee) = types.get(ty) else {
            return Err(FfiError::NotAPointer(ty));
        };
        let pointee = *pointee;
        pointer.as_tagged(heap).check_live(heap)?;
        let size = layout_size(&types, pointee);
        let address = pointer
            .as_tagged(heap)
            .address()
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)?;
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(&types, pointee, value, heap, &mut slot)?;
        memory::copy_in_at(address, 0, &slot.bytes()[..size]);
        Ok(())
    }

    /// Read one array element into raw bytes.
    pub fn buffer_get_index_raw(
        &self,
        heap: &Heap,
        buffer: &Handle<CBuffer>,
        index: usize,
    ) -> FfiResult<RawValue> {
        let types = self.types();
        let (element, offset) = element_of(&types, buffer.as_tagged(heap).c_type(), index)?;
        let size = layout_size(&types, element);
        let storage = buffer.as_tagged(heap).storage(heap);
        storage.check_live(heap)?;
        let mut slot = CallSlot::new(size.max(8));
        memory::copy_out_at(storage.address(), offset, &mut slot.bytes_mut()[..size]);
        Ok(RawValue {
            slot,
            type_id: element,
        })
    }

    /// Write one array element from a guest value.
    pub fn buffer_set_index(
        &self,
        heap: &Heap,
        buffer: &Handle<CBuffer>,
        index: usize,
        value: Tagged<Value>,
    ) -> FfiResult<()> {
        let types = self.types();
        let (element, offset) = element_of(&types, buffer.as_tagged(heap).c_type(), index)?;
        let size = layout_size(&types, element);
        let storage = buffer.as_tagged(heap).storage(heap);
        storage.check_live(heap)?;
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(&types, element, value, heap, &mut slot)?;
        memory::copy_in_at(storage.address(), offset, &slot.bytes()[..size]);
        Ok(())
    }

    /// Write one struct/union field from a guest value.
    pub fn buffer_set(
        &self,
        heap: &Heap,
        buffer: &Handle<CBuffer>,
        name: &str,
        value: Tagged<Value>,
    ) -> FfiResult<()> {
        let types = self.types();
        let (field_ty, offset) = field_of(&types, buffer.as_tagged(heap).c_type(), name)?;
        let size = layout_size(&types, field_ty);
        let storage = buffer.as_tagged(heap).storage(heap);
        storage.check_live(heap)?;
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(&types, field_ty, value, heap, &mut slot)?;
        memory::copy_in_at(storage.address(), offset, &slot.bytes()[..size]);
        Ok(())
    }
}

fn element_of(types: &TypeTable, aggregate: TypeId, index: usize) -> FfiResult<(TypeId, usize)> {
    let CType::Array(array) = types.get(aggregate) else {
        return Err(FfiError::NotAnArray(aggregate));
    };
    if index >= array.len {
        return Err(FfiError::IndexOutOfBounds);
    }
    let stride = layout_size(types, array.element);
    Ok((array.element, index * stride))
}

fn field_of(types: &TypeTable, aggregate: TypeId, name: &str) -> FfiResult<(TypeId, usize)> {
    match types.get(aggregate) {
        CType::Struct(structure) | CType::Union(structure) => structure
            .fields
            .iter()
            .find(|field| field.name.as_deref() == Some(name))
            .map(|field| (field.ty, field.offset))
            .ok_or_else(|| FfiError::UnknownField(name.to_string())),
        _ => Err(FfiError::NotAnObject(aggregate)),
    }
}

fn check_result(types: &TypeTable, result: TypeId, slot: &CallSlot) -> FfiResult<()> {
    let bytes = slot.bytes();
    match types.get(result) {
        CType::I64 => {
            let value = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
            if Smi::in_range(value) {
                Ok(())
            } else {
                Err(FfiError::IntegerOutOfRange)
            }
        }
        CType::U64 => {
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
    types: &TypeTable,
    ty: TypeId,
    value: Tagged<Value>,
    heap: &Heap,
    slot: &mut CallSlot,
) -> FfiResult<()> {
    match types.get(ty) {
        CType::Bool => {
            let bit = u8::from(bool_from(heap, value)?);
            slot.put(&bit.to_ne_bytes());
        }
        CType::I8 => slot.put(&i8::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::U8 | CType::Char => slot.put(&u8::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::I16 => slot.put(&i16::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::U16 => slot.put(&u16::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::I32 => slot.put(&i32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::U32 => slot.put(&u32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::I64 => slot.put(&int_from(heap, value)?.to_ne_bytes()),
        CType::U64 => slot.put(&u64::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        CType::F32 => slot.put(&(float_from(heap, value)? as f32).to_ne_bytes()),
        CType::F64 => slot.put(&float_from(heap, value)?.to_ne_bytes()),
        CType::Pointer(_) => {
            let address = pointer_from(types, ty, value, heap)?;
            slot.put(&address.to_ne_bytes());
        }
        CType::Struct(_) | CType::Union(_) | CType::Array(_) => {
            let buffer = value
                .get_as::<CBuffer>(heap)
                .ok_or(FfiError::InvalidArgument("expected a C buffer"))?;
            let found = buffer.c_type();
            if found != ty {
                return Err(FfiError::MismatchedType {
                    expected: ty,
                    found,
                });
            }
            let storage = buffer.storage(heap);
            storage.check_live(heap)?;
            let size = layout_size(types, ty);
            memory::copy_out(storage.address(), &mut slot.bytes_mut()[..size]);
        }
        CType::Void | CType::Function(_) => return Err(FfiError::UnsupportedType(ty)),
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

fn pointer_from(
    types: &TypeTable,
    ty: TypeId,
    value: Tagged<Value>,
    heap: &Heap,
) -> FfiResult<usize> {
    if value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
    {
        return Ok(0);
    }
    let pointer = value
        .get_as::<CPointer>(heap)
        .ok_or(FfiError::InvalidArgument("expected a C pointer"))?;
    pointer.check_live(heap)?;
    let actual = pointer.c_type();
    let actual_is_void_pointer =
        matches!(types.get(actual), CType::Pointer(pointee) if *pointee == TypeId::VOID);
    if actual != ty && !actual_is_void_pointer {
        return Err(FfiError::MismatchedType {
            expected: ty,
            found: actual,
        });
    }
    Ok(pointer.address())
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
