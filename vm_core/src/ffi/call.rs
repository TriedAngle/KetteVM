use crate::ffi::closure::{CallbackCtx, enter_call, leave_call};
use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, ParamKind, TypeId, TypeKind, param_kind};
use crate::{
    Args, CValue, CValueFlags, CValueInit, Convert, DenseString, ErasedRuntimeState,
    FixedByteArray, Float, ForeignLibrary, Handle, HandleScope, Heap, Smi, StringRef, Tagged,
    ThreadState, TypeInfo, Utf8String, VM, Value,
};
use libffi::middle::{Arg, CodePtr, Ret};

/// Scratch space for one scalar C argument or a result.
///
/// Backed by `u128` so the data is 16-byte aligned; `len` is the number of
/// live bytes. Small slots stay inline, so scalar calls allocate nothing.
pub struct CallSlot {
    inline: [u128; 1],
    heap: Vec<u128>,
    len: usize,
}

impl CallSlot {
    pub fn new(len: usize) -> Self {
        const INLINE: usize = 16;
        Self {
            inline: [0; 1],
            heap: if len <= INLINE {
                Vec::new()
            } else {
                vec![0; len.div_ceil(16)]
            },
            len,
        }
    }

    fn as_ptr(&self) -> *const u8 {
        if self.heap.is_empty() {
            self.inline.as_ptr().cast()
        } else {
            self.heap.as_ptr().cast()
        }
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        if self.heap.is_empty() {
            self.inline.as_mut_ptr().cast()
        } else {
            self.heap.as_mut_ptr().cast()
        }
    }

    pub fn bytes(&self) -> &[u8] {
        // Safety: the storage holds at least `len` initialized bytes.
        unsafe { core::slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // Safety: same as `bytes`.
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
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
        vm: &VM,
        runtime: &dyn ErasedRuntimeState,
        heap: &mut Heap,
        state: Option<&ThreadState>,
        scope: &'s HandleScope<'_>,
        callee: &Handle<'_, Value>,
        args: Args,
    ) -> FfiResult<RawValue<'s>> {
        let (plan, address) = {
            let function = callee
                .as_tagged(heap)
                .get_as::<CValue>(heap)
                .ok_or(FfiError::NotCallable)?;
            function.check_live(heap)?;
            let signature = function.c_type(heap);
            (signature.as_ref().plan(), function.address())
        };
        if args.len() != plan.params.len() {
            return Err(FfiError::ArgCount {
                expected: plan.params.len(),
                found: args.len(),
            });
        }

        // Scalars are packed into one aligned buffer; aggregates are passed by
        // address. Small signatures fit in the inline buffer.
        const INLINE_PARAMS: usize = 16;
        const INLINE_BYTES: usize = 128;
        let mut inline = [0u128; INLINE_BYTES / 16];
        let mut heap_buffer: Vec<u128> = Vec::new();
        let base: *mut u8 = if plan.buffer_len <= INLINE_BYTES {
            inline.as_mut_ptr().cast()
        } else {
            heap_buffer = vec![0u128; plan.buffer_len.div_ceil(16)];
            heap_buffer.as_mut_ptr().cast()
        };

        let mut scratch = CallScratch::default();
        let mut pointers = [base as *const u8; INLINE_PARAMS];
        for (index, param) in plan.params.iter().enumerate() {
            let value = args.get_handle(heap, index).as_tagged(heap);
            pointers[index] = match param.kind {
                ParamKind::Aggregate(type_id) => {
                    let value = value
                        .get_as::<CValue>(heap)
                        .ok_or(FfiError::InvalidArgument("expected a C buffer"))?;
                    let found = value.c_type(heap);
                    if found.id() != type_id {
                        return Err(FfiError::MismatchedType {
                            expected: type_id,
                            found: found.id(),
                        });
                    }
                    value.base(heap)? as *const u8
                }
                kind => {
                    let slot = unsafe { base.add(param.offset) };
                    // Safety: every scalar slot is 8 bytes inside the buffer.
                    let bytes = unsafe { core::slice::from_raw_parts_mut(slot, 8) };
                    encode_scalar(kind, value, heap, bytes, Some(&mut scratch))?;
                    slot as *const u8
                }
            };
        }

        // Safety: every pointer above is valid for the call. The callee was
        // live-checked, aggregates point at their owner's storage, and scalars
        // point into `inline`/`heap_buffer`, which outlive the call.
        let code = CodePtr(address.cast());
        let heap_pointer: *mut Heap = heap;
        let context = state.map(|state| CallbackCtx {
            vm,
            // Safety: the VM owns its runtimes and outlives every call.
            runtime: unsafe {
                core::mem::transmute::<&dyn ErasedRuntimeState, &'static dyn ErasedRuntimeState>(
                    runtime,
                )
            },
            heap: heap_pointer,
            state,
            ffi: self,
        });
        if let Some(context) = context {
            enter_call(context);
        }
        let mut inline_args: [Arg<'static>; INLINE_PARAMS] =
            core::array::from_fn(|_| Arg::new(&EMPTY_BYTE));
        let mut heap_args: Vec<Arg<'static>> = Vec::new();
        unsafe {
            if plan.params.len() <= INLINE_PARAMS {
                for (slot, pointer) in inline_args.iter_mut().zip(&pointers) {
                    *slot = Arg::new(&**pointer);
                }
                if plan.result_void {
                    plan.cif
                        .call_return_into(code, &inline_args[..plan.params.len()], Ret::void());
                } else {
                    let result = &mut *base.add(plan.result_offset);
                    plan.cif.call_return_into(
                        code,
                        &inline_args[..plan.params.len()],
                        Ret::new(result),
                    );
                }
            } else {
                for pointer in &pointers {
                    heap_args.push(Arg::new(&**pointer));
                }
                if plan.result_void {
                    plan.cif.call_return_into(code, &heap_args, Ret::void());
                } else {
                    let result = &mut *base.add(plan.result_offset);
                    plan.cif
                        .call_return_into(code, &heap_args, Ret::new(result));
                }
            }
        }

        if context.is_some() {
            leave_call();
        }
        drop(heap_buffer);

        let result_ty = {
            let function = callee
                .as_tagged(heap)
                .get_as::<CValue>(heap)
                .ok_or(FfiError::NotCallable)?;
            let signature = function.c_type(heap);
            signature
                .as_ref()
                .result(heap)
                .ok_or(FfiError::NotAFunction(signature.as_ref().id()))?
        };
        let result_size = if plan.result_void {
            0
        } else {
            result_ty.size().max(8)
        };
        let mut slot = CallSlot::new(result_size);
        if result_size > 0 {
            // Safety: `result_size` bytes were written at the result offset.
            let bytes =
                unsafe { core::slice::from_raw_parts(base.add(plan.result_offset), result_size) };
            slot.put(bytes);
        }
        Ok(RawValue {
            slot,
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
                    let map = pointee.value_map(heap).map(|map| scope.handle(map));
                    let pointee = scope.handle(pointee);
                    let value = heap.allocate_handle::<CValue>(
                        CValueInit {
                            ty: pointee,
                            map,
                            address,
                            flags: CValueFlags::POINTER,
                            storage: None,
                            library: None,
                            callable: None,
                        },
                        scope,
                    );
                    if let Some(library) = library {
                        value
                            .as_tagged(heap)
                            .set_library(heap, library.as_tagged(heap));
                    }
                    value.as_tagged(heap).erase()
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
            // A field or element view is storage, not a pointer.
            let tagged = view.as_tagged(heap);
            tagged.set_flags(heap, tagged.flags().without(CValueFlags::POINTER));
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
        encode_value(heap, ty, guest, out)
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
        encode_value(heap, ty, guest, slot.bytes_mut())?;
        memory::copy_in_at(base, offset, &slot.bytes()[..size]);
        Ok(())
    }
}

impl FfiState {
    /// Read `value[index]`, treating the value's type as the element type.
    pub fn element_read_raw<'s>(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
        index: usize,
    ) -> FfiResult<RawValue<'s>> {
        let element = value.as_tagged(heap).c_type(heap);
        value.as_tagged(heap).check_live(heap)?;
        let size = element.size();
        let address = (value.as_tagged(heap).address() as usize)
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
            ty: scope.handle(element),
        })
    }

    /// Write `value[index]`, treating the value's type as the element type.
    pub fn element_write(
        &self,
        heap: &Heap,
        value: &Handle<CValue>,
        index: usize,
        guest: Tagged<Value>,
    ) -> FfiResult<()> {
        let element = value.as_tagged(heap).c_type(heap);
        value.as_tagged(heap).check_live(heap)?;
        let size = element.size();
        let address = (value.as_tagged(heap).address() as usize)
            .checked_add(
                index
                    .checked_mul(size.max(1))
                    .ok_or(FfiError::AddressOverflow)?,
            )
            .ok_or(FfiError::AddressOverflow)? as *mut u8;
        let mut slot = CallSlot::new(size.max(8));
        encode_value(heap, element, guest, slot.bytes_mut())?;
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

const EMPTY_BYTE: u8 = 0;

/// Encode one scalar argument into its slot.
fn encode_scalar(
    kind: ParamKind,
    value: Tagged<Value>,
    heap: &Heap,
    out: &mut [u8],
    scratch: Option<&mut CallScratch>,
) -> FfiResult<()> {
    match kind {
        ParamKind::Scalar(kind) => match kind {
            TypeKind::Bool => out[0] = u8::from(bool_from(heap, value)?),
            TypeKind::I8 => out[..1].copy_from_slice(&(int_from(heap, value)? as i8).to_ne_bytes()),
            TypeKind::U8 | TypeKind::Char => out[0] = int_from(heap, value)? as u8,
            TypeKind::I16 => {
                out[..2].copy_from_slice(&(int_from(heap, value)? as i16).to_ne_bytes())
            }
            TypeKind::U16 | TypeKind::Char16 => {
                out[..2].copy_from_slice(&(int_from(heap, value)? as u16).to_ne_bytes())
            }
            TypeKind::I32 => {
                out[..4].copy_from_slice(&(int_from(heap, value)? as i32).to_ne_bytes())
            }
            TypeKind::U32 => {
                out[..4].copy_from_slice(&(int_from(heap, value)? as u32).to_ne_bytes())
            }
            TypeKind::I64 => out[..8].copy_from_slice(&int_from(heap, value)?.to_ne_bytes()),
            TypeKind::U64 => {
                out[..8].copy_from_slice(&(int_from(heap, value)? as u64).to_ne_bytes())
            }
            TypeKind::F32 => {
                out[..4].copy_from_slice(&(float_from(heap, value)? as f32).to_ne_bytes())
            }
            TypeKind::F64 => out[..8].copy_from_slice(&float_from(heap, value)?.to_ne_bytes()),
            _ => unreachable!("non-scalar kind in a scalar slot"),
        },
        ParamKind::CharPointer
        | ParamKind::Char16Pointer
        | ParamKind::PointerAny
        | ParamKind::PointerTo(_) => {
            let address = encode_pointer(kind, value, heap, scratch)?;
            out[..8].copy_from_slice(&(address as usize).to_ne_bytes());
        }
        ParamKind::Aggregate(_) => {
            return Err(FfiError::InvalidArgument(
                "aggregate passed through a scalar slot",
            ));
        }
    }
    Ok(())
}

/// Encode a pointer argument; char pointers also accept strings and bytes.
fn encode_pointer(
    kind: ParamKind,
    value: Tagged<Value>,
    heap: &Heap,
    mut scratch: Option<&mut CallScratch>,
) -> FfiResult<*mut u8> {
    let expected = match kind {
        ParamKind::CharPointer => TypeId::CHAR,
        ParamKind::Char16Pointer => TypeId::CHAR16,
        ParamKind::PointerAny => return encode_pointer_value(value, heap, None),
        ParamKind::PointerTo(id) => return encode_pointer_value(value, heap, Some(id)),
        _ => unreachable!("not a pointer kind"),
    };
    if let Some(scratch) = scratch.as_deref_mut() {
        if let Some(text) = StringRef::try_from(heap, value) {
            return if expected == TypeId::CHAR {
                utf8_argument(text, heap, scratch)
            } else {
                utf16_argument(text, heap, scratch)
            };
        }
        if expected == TypeId::CHAR
            && let Some(bytes) = value.get_as::<FixedByteArray>(heap)
        {
            return byte_array_argument(bytes.as_ref(), scratch);
        }
    }
    encode_pointer_value(value, heap, Some(expected))
}

/// Encode a C pointer value, accepting `null`/`undefined`, void pointers and
/// (for `char *`) matching byte pointers.
fn encode_pointer_value(
    value: Tagged<Value>,
    heap: &Heap,
    expected: Option<TypeId>,
) -> FfiResult<*mut u8> {
    if value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
    {
        return Ok(core::ptr::null_mut());
    }
    let pointer = value
        .get_as::<CValue>(heap)
        .ok_or(FfiError::InvalidArgument("expected a C pointer"))?;
    pointer.check_live(heap)?;
    let mut actual = pointer.c_type(heap);
    // A `T[n]` value decays to `T *`.
    if actual.as_ref().kind() == TypeKind::Array {
        actual = actual
            .as_ref()
            .element(heap)
            .ok_or(FfiError::InvalidArgument("array type without element"))?;
    }
    if let Some(expected) = expected {
        let actual_id = actual.as_ref().id();
        let compatible = actual_id == expected
            || actual.as_ref().kind() == TypeKind::Void
            || (expected == TypeId::CHAR && (actual_id == TypeId::U8 || actual_id == TypeId::I8))
            || (expected == TypeId::CHAR16 && actual_id == TypeId::U16);
        if !compatible {
            return Err(FfiError::MismatchedType {
                expected,
                found: actual.as_ref().id(),
            });
        }
    }
    // TODO: pinning. A value wrapped over VM-owned bytes points into the GC
    // heap; a callback that allocates during the call can move it.
    pointer.base(heap)
}

fn byte_array_argument(bytes: &FixedByteArray, scratch: &mut CallScratch) -> FfiResult<*mut u8> {
    // Always copy: a callback may allocate while C holds the pointer.
    let mut copy = bytes.as_slice().to_vec();
    if copy.last() != Some(&0) {
        copy.push(0);
    }
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
        let mut copy = bytes.to_vec();
        copy.push(0);
        return Ok(scratch.push_utf8(copy) as *mut u8);
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
    let mut units = string.to_units(heap);
    if units.contains(&0) {
        return Err(FfiError::EmbeddedNul);
    }
    units.push(0);
    Ok(scratch.push_utf16(units) as *mut u8)
}

/// Encode `guest` as `ty` into `out` (the write path: no call buffer).
pub fn encode_value(
    heap: &Heap,
    ty: Tagged<TypeInfo>,
    guest: Tagged<Value>,
    out: &mut [u8],
) -> FfiResult<()> {
    match param_kind(heap, ty)? {
        ParamKind::Aggregate { .. } => {
            let buffer = guest
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
            // Safety: `out` is at least `size` bytes and `base` is live.
            unsafe { core::ptr::copy_nonoverlapping(base, out.as_mut_ptr(), size) };
        }
        kind => encode_scalar(kind, guest, heap, out, None)?,
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
