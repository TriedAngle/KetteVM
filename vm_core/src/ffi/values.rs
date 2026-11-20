use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId, TypeKind};
use crate::{
    CValue, CValueFlags, CValueInit, CallableInfoInit, CallableInfoObject, DenseString,
    FixedByteArray, ForeignLibrary, FunctionKind, Handle, HandleScope, Heap, NativeIndex, TypeInfo,
    Utf8String, mode_from_strict,
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

    /// Allocate zeroed external storage for a C object.
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
        let address = memory::alloc_zeroed(layout.size(), layout.align())?;
        let map = map.map(|map| scope.handle(map));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                address,
                flags: CValueFlags::OWNED,
                storage: None,
                library: None,
                callable: None,
            },
            scope,
        ))
    }

    /// A view `delta` bytes into `value`, typed as `ty`.
    ///
    /// The view borrows the storage: heap storage stays alive through the
    /// value, external storage stays valid as long as the owner does.
    pub fn view<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: Handle<CValue>,
        ty: Handle<TypeInfo>,
        delta: usize,
    ) -> Handle<'s, CValue> {
        let (address, flags, storage, library, map) = {
            let source = value.as_tagged(heap);
            (
                (source.address() as usize).wrapping_add(delta) as *mut u8,
                source.flags().without(CValueFlags::OWNED),
                source.storage(heap),
                source.library(heap),
                ty.as_tagged(heap).value_map(heap),
            )
        };
        let storage = storage.map(|storage| scope.handle(storage));
        let library = library.map(|library| scope.handle(library));
        let map = map.map(|map| scope.handle(map));
        heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                address,
                flags,
                storage,
                library,
                callable: None,
            },
            scope,
        )
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
            let stride = if info.as_ref().kind() == TypeKind::Array {
                info.as_ref()
                    .element(heap)
                    .map(|element| element.size())
                    .unwrap_or(1)
            } else {
                info.as_ref().size()
            };
            (info, stride.max(1))
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

    /// A detached copy of a value, with its own external storage.
    pub fn copy_value<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        value: &Handle<CValue>,
    ) -> FfiResult<Handle<'s, CValue>> {
        if value.as_tagged(heap).is_pointer() {
            return Err(FfiError::InvalidArgument(
                "cannot copy a C pointer; read through it first",
            ));
        }
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

    /// A borrowed view over `value`, typed as `ty`.
    pub fn wrap_value<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        ty: &Handle<TypeInfo>,
        value: &Handle<CValue>,
    ) -> FfiResult<Handle<'s, CValue>> {
        let (expected, actual) = {
            let source = value.as_tagged(heap);
            (ty.as_tagged(heap).id(), source.c_type(heap).id())
        };
        let same = value
            .as_tagged(heap)
            .c_type(heap)
            .ptr_eq(ty.as_tagged(heap));
        let void = value.as_tagged(heap).c_type(heap).kind() == TypeKind::Void;
        if !same && !void {
            return Err(FfiError::MismatchedType {
                expected,
                found: actual,
            });
        }
        value.as_tagged(heap).check_live(heap)?;
        let map = ty
            .as_tagged(heap)
            .value_map(heap)
            .map(|map| scope.handle(map));
        let storage = value.as_tagged(heap).storage(heap).map(|s| scope.handle(s));
        let library = value.as_tagged(heap).library(heap).map(|l| scope.handle(l));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                address: value.as_tagged(heap).address(),
                flags: CValueFlags::NONE,
                storage,
                library,
                callable: None,
            },
            scope,
        ))
    }

    /// A borrowed view over VM-owned bytes, typed as `ty`.
    pub fn wrap_bytes<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        ty: &Handle<TypeInfo>,
        bytes: &Handle<FixedByteArray>,
    ) -> Handle<'s, CValue> {
        let address = bytes.as_tagged(heap).as_ptr() as *mut u8;
        let map = ty
            .as_tagged(heap)
            .value_map(heap)
            .map(|map| scope.handle(map));
        heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                address,
                flags: CValueFlags::NONE,
                storage: Some(bytes.clone()),
                library: None,
                callable: None,
            },
            scope,
        )
    }

    /// Release external storage owned by `value`; idempotent.
    pub fn free_value(&self, heap: &Heap, value: &Handle<CValue>) -> FfiResult<()> {
        let value = value.as_tagged(heap);
        if value.is_dead() {
            return Ok(());
        }
        if !value.is_owned() {
            return Err(FfiError::NotOwned);
        }
        memory::free(value.address());
        value.kill(heap);
        Ok(())
    }

    /// A callable C function object for an already-resolved target.
    pub fn function<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        trampoline: NativeIndex,
        signature: &Handle<TypeInfo>,
        target: &Handle<CValue>,
        library: &Handle<ForeignLibrary>,
    ) -> FfiResult<Handle<'s, CValue>> {
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
        let map = heap.known().c_function_map.get_handle(heap, scope);
        let storage = target
            .as_tagged(heap)
            .storage(heap)
            .map(|s| scope.handle(s));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: signature.clone(),
                map: Some(map),
                address: target.as_tagged(heap).address(),
                flags: CValueFlags::NONE,
                storage,
                library: Some(library.clone()),
                callable: Some(callable),
            },
            scope,
        ))
    }
}
