use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId, TypeKind};
use crate::{
    CFunction, CFunctionInit, CPointer, CPointerInit, CValue, CValueInit, CallableInfoInit,
    CallableInfoObject, DenseString, FixedByteArray, ForeignLibrary, FunctionKind, Handle,
    HandleScope, Heap, NativeIndex, TypeInfo, Utf8String, mode_from_strict,
};

impl FfiState {
    pub fn cstring<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        text: &str,
    ) -> FfiResult<Handle<'s, CValue>> {
        if text.contains('\0') {
            return Err(FfiError::EmbeddedNul);
        }
        let char_ty = heap
            .known()
            .ffi_types
            .get(heap, TypeId::CHAR)
            .expect("C FFI builtins are installed during VM setup");
        let char_ty = scope.handle(char_ty);
        let ty = self.types.array_of(heap, scope, &char_ty, text.len() + 1)?;
        let value = self.value(heap, scope, &ty)?;
        let base = value.as_tagged(heap).base(heap)?;
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        memory::copy_in(base, &bytes);
        Ok(value)
    }

    pub fn wstring<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        units: &[u16],
    ) -> FfiResult<Handle<'s, CValue>> {
        if units.contains(&0) {
            return Err(FfiError::EmbeddedNul);
        }
        let char_ty = heap
            .known()
            .ffi_types
            .get(heap, TypeId::CHAR16)
            .expect("C FFI builtins are installed during VM setup");
        let char_ty = scope.handle(char_ty);
        let ty = self
            .types
            .array_of(heap, scope, &char_ty, units.len() + 1)?;
        let value = self.value(heap, scope, &ty)?;
        let base = value.as_tagged(heap).base(heap)?;
        let mut bytes = Vec::with_capacity((units.len() + 1) * 2);
        for unit in units {
            bytes.extend_from_slice(&unit.to_ne_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        memory::copy_in(base, &bytes);
        Ok(value)
    }

    pub fn read_c_string<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        address: *const u8,
    ) -> FfiResult<Handle<'s, Utf8String>> {
        if address.is_null() {
            return Err(FfiError::NullPointer);
        }
        let len = memory::c_str_len(address);
        let mut bytes = vec![0u8; len];
        memory::copy_out(address, &mut bytes);
        Utf8String::from_bytes(heap, scope, &bytes).ok_or(FfiError::InvalidUtf8)
    }

    pub fn read_c_wstring<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        address: *const u8,
    ) -> FfiResult<Handle<'s, DenseString>> {
        if address.is_null() {
            return Err(FfiError::NullPointer);
        }
        let len = memory::c_wstr_len(address);
        let mut units = vec![0u16; len];
        for (index, unit) in units.iter_mut().enumerate() {
            let mut bytes = [0u8; 2];
            memory::copy_out_at(address, index * 2, &mut bytes);
            *unit = u16::from_ne_bytes(bytes);
        }
        Ok(DenseString::from_units(heap, scope, &units))
    }

    /// Allocate zeroed VM-owned storage for an aggregate type.
    pub fn value<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        ty: &Handle<TypeInfo>,
    ) -> FfiResult<Handle<'s, CValue>> {
        let (id, layout, map) = {
            let info = ty.as_tagged(heap);
            (info.id(), info.object_layout(), info.value_map(heap))
        };
        let layout = layout.ok_or(FfiError::NotAnObject(id))?;
        if layout.align() > 16 {
            return Err(FfiError::UnsupportedAlignment(layout.align()));
        }
        let size = layout.size().max(1);
        let map = map.map(|map| scope.handle(map));
        let inline = heap.allocate_handle::<FixedByteArray>(&vec![0u8; size], scope);
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                backing: inline.erase(),
                offset: 0,
            },
            scope,
        ))
    }

    /// A view `delta` bytes into an existing value, sharing its backing.
    pub fn view<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<CValue>,
        ty: Handle<TypeInfo>,
        delta: usize,
    ) -> Handle<'s, CValue> {
        let (backing, offset, map) = {
            let source = value.as_tagged(heap);
            (
                scope.handle(source.backing(heap)),
                source.offset(),
                ty.as_tagged(heap)
                    .value_map(heap)
                    .map(|map| scope.handle(map)),
            )
        };
        heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                backing,
                offset: offset + delta,
            },
            scope,
        )
    }

    /// A view `delta` bytes into memory addressed by a pointer.
    pub fn view_pointer<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        ty: &Handle<TypeInfo>,
        delta: usize,
    ) -> FfiResult<Handle<'s, CValue>> {
        pointer.as_tagged(heap).check_live(heap)?;
        let map = ty
            .as_tagged(heap)
            .value_map(heap)
            .map(|map| scope.handle(map));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                backing: pointer.clone().erase(),
                offset: delta,
            },
            scope,
        ))
    }

    /// A pointer with the same storage and ownership, retyped.
    pub fn cast_pointer<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        ty: &Handle<TypeInfo>,
    ) -> FfiResult<Handle<'s, CPointer>> {
        let (address, flags, library) = {
            let source = pointer.as_tagged(heap);
            (
                source.address(),
                source.flags(),
                source.library(heap).map(|library| scope.handle(library)),
            )
        };
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty.clone(),
                address,
                flags,
                library: None,
            },
            scope,
        );
        if let Some(library) = library {
            out.as_tagged(heap)
                .set_library(heap, library.as_tagged(heap));
        }
        Ok(out)
    }

    /// `pointer + elements`, scaled by the pointee size (C semantics).
    pub fn pointer_offset<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        elements: i64,
    ) -> FfiResult<Handle<'s, CPointer>> {
        let (pointee, element_size) = {
            let source = pointer.as_tagged(heap);
            let pointee = source.pointee(heap);
            (pointee, pointee.size().max(1))
        };
        let delta = elements
            .checked_mul(element_size as i64)
            .ok_or(FfiError::AddressOverflow)?;
        let (address, flags, library) = {
            let source = pointer.as_tagged(heap);
            (
                source.address(),
                source.flags(),
                source.library(heap).map(|library| scope.handle(library)),
            )
        };
        let address = (address as usize)
            .checked_add_signed(delta as isize)
            .ok_or(FfiError::AddressOverflow)? as *mut u8;
        let ty = scope.handle(pointee);
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty,
                address,
                flags,
                library: None,
            },
            scope,
        );
        if let Some(library) = library {
            out.as_tagged(heap)
                .set_library(heap, library.as_tagged(heap));
        }
        Ok(out)
    }

    pub fn value_add<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<'_, CValue>,
        elements: i64,
    ) -> FfiResult<Handle<'s, CValue>> {
        if elements < 0 {
            return Err(FfiError::InvalidArgument("negative pointer offset"));
        }
        let (ty, stride) = {
            let info = value.as_tagged(heap).c_type(heap);
            let info = info.as_ref();
            let stride = if info.kind() == TypeKind::Array {
                info.element(heap)
                    .map(|element| element.size())
                    .unwrap_or(1)
            } else {
                info.size()
            };
            (value.as_tagged(heap).c_type(heap), stride.max(1))
        };
        let delta = (elements as usize)
            .checked_mul(stride)
            .ok_or(FfiError::AddressOverflow)?;
        let ty = scope.handle(ty);
        Ok(self.view(heap, scope, value, ty, delta))
    }

    pub fn value_cast<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<'_, CValue>,
        ty: Handle<'_, TypeInfo>,
    ) -> Handle<'s, CValue> {
        self.view(heap, scope, value, ty, 0)
    }

    /// A detached copy of a value, with its own inline storage.
    pub fn copy_value<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
    ) -> FfiResult<Handle<'s, CValue>> {
        let ty = {
            let ty = value.as_tagged(heap).c_type(heap);
            scope.handle(ty)
        };
        let copy = self.value(heap, scope, &ty)?;
        let (source, size) = {
            let source = value.as_tagged(heap).base(heap)?;
            (source, ty.as_tagged(heap).size())
        };
        let destination = copy.as_tagged(heap).base(heap)?;
        memory::copy_bytes(source, destination, size);
        Ok(copy)
    }

    /// A view over `pointer`, typed as `ty`.
    pub fn wrap_value<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        ty: &Handle<TypeInfo>,
        pointer: &Handle<CPointer>,
    ) -> FfiResult<Handle<'s, CValue>> {
        let (expected, actual, map) = {
            let info = ty.as_tagged(heap);
            let pointee = pointer.as_tagged(heap).pointee(heap);
            (info.id(), pointee.id(), info.value_map(heap))
        };
        let same = {
            let info = ty.as_tagged(heap);
            pointer.as_tagged(heap).pointee(heap).ptr_eq(info)
        };
        let void = {
            let info = ty.as_tagged(heap);
            let _ = info;
            pointer.as_tagged(heap).pointee(heap).kind() == TypeKind::Void
        };
        if !same && !void {
            return Err(FfiError::MismatchedType {
                expected,
                found: actual,
            });
        }
        pointer.as_tagged(heap).check_live(heap)?;
        let map = map.map(|map| scope.handle(map));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                backing: pointer.clone().erase(),
                offset: 0,
            },
            scope,
        ))
    }

    /// A callable C function object for an already-resolved target.
    pub fn function<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        trampoline: NativeIndex,
        signature: &Handle<TypeInfo>,
        target: &Handle<CPointer>,
        library: &Handle<ForeignLibrary>,
    ) -> FfiResult<Handle<'s, CFunction>> {
        let info = signature.as_tagged(heap);
        if info.kind() != TypeKind::Function {
            return Err(FfiError::NotAFunction(info.id()));
        }
        let params = info.params(heap).ok_or(FfiError::NotAFunction(info.id()))?;
        let arity = params.len();
        let callable = heap.allocate_handle::<CallableInfoObject>(
            CallableInfoInit::Native {
                index: trampoline,
                mode: mode_from_strict(true),
            },
            scope,
        );
        callable
            .as_tagged(heap)
            .set_metadata_full(heap, None, arity, arity, FunctionKind::Normal);
        Ok(heap.allocate_handle::<CFunction>(
            CFunctionInit {
                info: callable,
                signature: signature.clone(),
                target: target.clone(),
                library: Some(library.clone()),
            },
            scope,
        ))
    }
}
