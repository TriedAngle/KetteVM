use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId, TypeKind};
use crate::{
    Args, CFunction, CPointer, CPointerFlags, CPointerInit, CValue, Convert, DenseString,
    FixedByteArray, Float, ForeignLibrary, Handle, HandleScope, Heap, Smi, StringData, StringRef,
    Tagged, TypeInfo, Utf8String, Value,
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

/// Scratch storage for transcoded string arguments.
#[derive(Default)]
struct CallScratch {
    utf8: Vec<Box<[u8]>>,
    utf16: Vec<Box<[u16]>>,
}

impl CallScratch {
    fn push_utf8(&mut self, bytes: Vec<u8>) -> usize {
        self.utf8.push(bytes.into_boxed_slice());
        self.utf8.last().expect("just pushed").as_ptr() as usize
    }

    fn push_utf16(&mut self, units: Vec<u16>) -> usize {
        self.utf16.push(units.into_boxed_slice());
        self.utf16.last().expect("just pushed").as_ptr() as usize
    }
}

/// A raw C value (call result or field read) plus its boxed type.
pub struct RawValue<'s> {
    pub slot: CallSlot,
    pub ty: Handle<'s, TypeInfo>,
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
            (function.signature(heap), target.address())
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
        let mut scratch = CallScratch::default();
        for (index, slot) in slots.iter_mut().enumerate() {
            let param = param_at(heap, params, index)?;
            let value = args.get_handle(heap, index).as_tagged(heap);
            convert_arg(param, value, heap, slot, Some(&mut scratch))?;
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
        let code = CodePtr(address.cast());
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

    pub fn read_value<'a>(
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
            TypeKind::Bool => Convert::boolean(heap, bytes[0] != 0),
            TypeKind::I8 => Smi::new(bytes[0] as i8 as i64).into_tagged(),
            TypeKind::U8 | TypeKind::Char => Smi::new(bytes[0] as i64).into_tagged(),
            TypeKind::I16 => {
                Smi::new(i16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64).into_tagged()
            }
            TypeKind::U16 | TypeKind::Char16 => {
                Smi::new(u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as i64).into_tagged()
            }
            TypeKind::I32 => {
                Smi::new(i32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64).into_tagged()
            }
            TypeKind::U32 => {
                Smi::new(u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as i64).into_tagged()
            }
            TypeKind::I64 => {
                // TODO: the full i64 range needs BigInt; beyond 2^53 the Float
                // fallback loses precision.
                let value = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
                if Smi::in_range(value) {
                    Smi::new(value).into_tagged()
                } else {
                    heap.allocate_handle::<Float>(value as f64, scope)
                        .as_tagged(heap)
                        .erase()
                }
            }
            TypeKind::U64 => {
                // TODO: the full u64 range needs BigInt; beyond 2^53 the Float
                // fallback loses precision.
                let value = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
                if value <= Smi::MAX as u64 {
                    Smi::new(value as i64).into_tagged()
                } else {
                    heap.allocate_handle::<Float>(value as f64, scope)
                        .as_tagged(heap)
                        .erase()
                }
            }
            TypeKind::F32 => {
                let value = f32::from_ne_bytes(bytes[..4].try_into().unwrap()) as f64;
                heap.allocate_handle::<Float>(value, scope)
                    .as_tagged(heap)
                    .erase()
            }
            TypeKind::F64 => {
                let value = f64::from_ne_bytes(bytes[..8].try_into().unwrap());
                heap.allocate_handle::<Float>(value, scope)
                    .as_tagged(heap)
                    .erase()
            }
            TypeKind::Pointer => {
                let width = core::mem::size_of::<usize>();
                let address = usize::from_ne_bytes(bytes[..width].try_into().unwrap()) as *mut u8;
                let pointee = {
                    let ty = ty.as_tagged(heap);
                    ty.pointee(heap).expect("pointer type without pointee")
                };
                if pointee.kind() == TypeKind::Char {
                    decode_utf8(heap, scope, address.cast())
                } else if pointee.kind() == TypeKind::Char16 {
                    decode_utf16(heap, scope, address.cast())
                } else {
                    let pointee = scope.handle(pointee);
                    let pointer = heap.allocate_handle::<CPointer>(
                        CPointerInit {
                            pointee,
                            address,
                            flags: CPointerFlags::NONE,
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
            }
            TypeKind::Struct | TypeKind::Union | TypeKind::Array => {
                let value = self
                    .value(heap, scope, ty)
                    .expect("C FFI aggregate result allocation");
                let base = value.as_tagged(heap).base(heap).expect("C value backing");
                memory::copy_in(base, &bytes[..size]);
                value.as_tagged(heap).erase()
            }
            TypeKind::Function => heap.known().undefined.get(heap).erase(),
        }
    }

    pub fn read_raw<'a>(
        &self,
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        raw: &RawValue<'_>,
        library: Option<&Handle<'_, ForeignLibrary>>,
    ) -> Tagged<'a, Value> {
        self.read_value(heap, scope, &raw.ty, raw.slot.bytes(), library)
    }
}

impl FfiState {
    pub fn value_get_field<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<CValue>,
        name: &str,
    ) -> FfiResult<ValueRead<'s>> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (field_ty, offset) = field_of(heap, ty, name)?;
        let field_ty = scope.handle(field_ty);
        self.value_read(heap, scope, value, field_ty, offset)
    }

    pub fn value_get_index<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<CValue>,
        index: usize,
    ) -> FfiResult<ValueRead<'s>> {
        let ty = value.as_tagged(heap).c_type(heap);
        let (element, offset) = element_of(heap, ty, index)?;
        let element = scope.handle(element);
        self.value_read(heap, scope, value, element, offset)
    }

    pub fn value_set_field(
        &self,
        heap: &Heap,
        value: Tagged<CValue>,
        name: &str,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let ty = value.c_type(heap);
        let (field_ty, offset) = field_of(heap, ty, name)?;
        self.value_write(heap, value, field_ty, offset, guest)
    }

    pub fn value_set_index(
        &self,
        heap: &Heap,
        value: Tagged<CValue>,
        index: usize,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let ty = value.c_type(heap);
        let (element, offset) = element_of(heap, ty, index)?;
        self.value_write(heap, value, element, offset, guest)
    }

    pub fn value_read<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<CValue>,
        ty: Handle<'s, TypeInfo>,
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
        let base = value.as_tagged(heap).base(heap)?;
        let mut slot = CallSlot::new(size.max(8));
        memory::copy_out_at(base, offset, &mut slot.bytes_mut()[..size]);
        Ok(ValueRead::Raw(RawValue { slot, ty }))
    }

    /// Encode `guest` as `ty` into `out`.
    pub fn write_typed(
        &self,
        heap: &Heap,
        ty: Tagged<TypeInfo>,
        guest: Tagged<Value>,
        out: &mut [u8],
    ) -> FfiResult<()> {
        let size = ty.size();
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(ty, guest, heap, &mut slot, None)?;
        out[..size].copy_from_slice(&slot.bytes()[..size]);
        Ok(())
    }

    pub fn value_write(
        &self,
        heap: &Heap,
        value: Tagged<CValue>,
        ty: Tagged<TypeInfo>,
        offset: usize,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let base = value.base(heap)?;
        let size = ty.size();
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(ty, guest, heap, &mut slot, None)?;
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
        let address = (pointer.as_tagged(heap).address() as usize)
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)? as *const u8;
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
        let address = (pointer.as_tagged(heap).address() as usize)
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)? as *mut u8;
        let mut slot = CallSlot::new(size.max(8));
        convert_arg(pointee, value, heap, &mut slot, None)?;
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
    scratch: Option<&mut CallScratch>,
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
        TypeKind::U16 | TypeKind::Char16 => {
            slot.put(&u16::try_from(int_from(heap, value)?)?.to_ne_bytes())
        }
        TypeKind::I32 => slot.put(&i32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::U32 => slot.put(&u32::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::I64 => slot.put(&int_from(heap, value)?.to_ne_bytes()),
        TypeKind::U64 => slot.put(&u64::try_from(int_from(heap, value)?)?.to_ne_bytes()),
        TypeKind::F32 => slot.put(&(float_from(heap, value)? as f32).to_ne_bytes()),
        TypeKind::F64 => slot.put(&float_from(heap, value)?.to_ne_bytes()),
        TypeKind::Pointer => {
            let address = match scratch {
                Some(scratch) => match string_pointer_from(ty, value, heap, scratch)? {
                    Some(address) => address,
                    None => pointer_from(ty, value, heap)?,
                },
                None => pointer_from(ty, value, heap)?,
            };
            slot.put(&(address as usize).to_ne_bytes());
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
            let base = buffer.base(heap)?;
            let size = ty.size();
            memory::copy_out(base, &mut slot.bytes_mut()[..size]);
        }
        TypeKind::Void | TypeKind::Function => return Err(FfiError::UnsupportedType(ty.id())),
    }
    Ok(())
}

fn decode_utf8<'a>(
    heap: &'a mut Heap,
    scope: &HandleScope<'_>,
    address: *const u8,
) -> Tagged<'a, Value> {
    if address.is_null() {
        return heap.known().null.get(heap).erase();
    }
    let len = memory::c_str_len(address);
    let mut bytes = vec![0u8; len];
    memory::copy_out(address, &mut bytes);
    match Utf8String::from_bytes(heap, scope, &bytes) {
        Some(value) => value.as_tagged(heap).erase(),
        None => {
            let text = String::from_utf8_lossy(&bytes);
            Utf8String::from_str(heap, scope, &text)
                .as_tagged(heap)
                .erase()
        }
    }
}

fn decode_utf16<'a>(
    heap: &'a mut Heap,
    scope: &HandleScope<'_>,
    address: *const u8,
) -> Tagged<'a, Value> {
    if address.is_null() {
        return heap.known().null.get(heap).erase();
    }
    let len = memory::c_wstr_len(address);
    let mut units = vec![0u16; len];
    for (index, unit) in units.iter_mut().enumerate() {
        let mut bytes = [0u8; 2];
        memory::copy_out_at(address, index * 2, &mut bytes);
        *unit = u16::from_ne_bytes(bytes);
    }
    DenseString::from_units(heap, scope, &units)
        .as_tagged(heap)
        .erase()
}

fn string_pointer_from(
    ty: Tagged<TypeInfo>,
    value: Tagged<Value>,
    heap: &Heap,
    scratch: &mut CallScratch,
) -> FfiResult<Option<*mut u8>> {
    let expected = ty
        .pointee(heap)
        .ok_or(FfiError::InvalidArgument("pointer type without pointee"))?;
    match expected.kind() {
        TypeKind::Char => {
            if let Some(text) = StringRef::try_from(heap, value) {
                return utf8_argument(text, heap, scratch).map(Some);
            }
            if let Some(bytes) = value.get_as::<FixedByteArray>(heap) {
                return byte_array_argument(bytes.as_ref(), scratch).map(Some);
            }
            if let Some(pointer) = value.get_as::<CPointer>(heap) {
                return compatible_char_pointer(pointer.as_ref(), heap, expected).map(Some);
            }
            Ok(None)
        }
        TypeKind::Char16 => {
            if let Some(text) = StringRef::try_from(heap, value) {
                return utf16_argument(text, heap, scratch).map(Some);
            }
            if let Some(pointer) = value.get_as::<CPointer>(heap) {
                return compatible_char_pointer(pointer.as_ref(), heap, expected).map(Some);
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn compatible_char_pointer(
    pointer: &CPointer,
    heap: &Heap,
    expected: Tagged<TypeInfo>,
) -> FfiResult<*mut u8> {
    pointer.check_live(heap)?;
    let actual = pointer.pointee(heap);
    let compatible = actual.ptr_eq(expected)
        || actual.kind() == TypeKind::Void
        || (expected.kind() == TypeKind::Char
            && matches!(actual.kind(), TypeKind::U8 | TypeKind::I8))
        || (expected.kind() == TypeKind::Char16 && actual.kind() == TypeKind::U16);
    if !compatible {
        return Err(FfiError::MismatchedType {
            expected: expected.id(),
            found: actual.id(),
        });
    }
    Ok(pointer.address())
}

fn byte_array_argument(bytes: &FixedByteArray, scratch: &mut CallScratch) -> FfiResult<*mut u8> {
    let slice = bytes.as_slice();
    if slice.last() == Some(&0) {
        return Ok(bytes.as_ptr() as *mut u8);
    }
    let mut copy = slice.to_vec();
    copy.push(0);
    Ok(scratch.push_utf8(copy) as *mut u8)
}

fn utf8_argument(
    string: StringRef<'_>,
    heap: &Heap,
    scratch: &mut CallScratch,
) -> FfiResult<*mut u8> {
    if let Some(bytes) = string.utf8_bytes(heap) {
        if bytes.contains(&0) {
            return Err(FfiError::EmbeddedNul);
        }
        if bytes.is_empty() {
            return Ok(scratch.push_utf8(vec![0]) as *mut u8);
        }
        return Ok(bytes.as_ptr() as *mut u8);
    }
    let units = string.to_units(heap);
    if units.contains(&0) {
        return Err(FfiError::EmbeddedNul);
    }
    let mut bytes = String::from_utf16_lossy(&units).into_bytes();
    bytes.push(0);
    Ok(scratch.push_utf8(bytes) as *mut u8)
}

fn utf16_argument(
    string: StringRef<'_>,
    heap: &Heap,
    scratch: &mut CallScratch,
) -> FfiResult<*mut u8> {
    if let Some(StringData::Utf16(units)) = string.data(heap) {
        if units.contains(&0) {
            return Err(FfiError::EmbeddedNul);
        }
        if units.is_empty() {
            return Ok(scratch.push_utf16(vec![0]) as *mut u8);
        }
        return Ok(units.as_ptr() as *mut u8);
    }
    let mut units = string.to_units(heap);
    if units.contains(&0) {
        return Err(FfiError::EmbeddedNul);
    }
    units.push(0);
    Ok(scratch.push_utf16(units) as *mut u8)
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

fn pointer_from(ty: Tagged<TypeInfo>, value: Tagged<Value>, heap: &Heap) -> FfiResult<*mut u8> {
    if value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
    {
        return Ok(core::ptr::null_mut());
    }
    let expected = ty
        .pointee(heap)
        .ok_or(FfiError::InvalidArgument("pointer type without pointee"))?;
    if let Some(bytes) = value.get_as::<FixedByteArray>(heap) {
        if matches!(
            expected.kind(),
            TypeKind::Void | TypeKind::U8 | TypeKind::I8
        ) {
            return Ok(bytes.as_ref().as_ptr() as *mut u8);
        }
        return Err(FfiError::InvalidArgument("expected a C pointer"));
    }
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
        return c_value.base(heap);
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
    Ok(pointer.address())
}
