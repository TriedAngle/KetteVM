//! Constructors shared by the natives and by Rust callers.

use crate::ffi::memory;
use crate::ffi::{CType, FfiError, FfiResult, FfiState, TypeId};
use crate::{
    CBuffer, CBufferInit, CFunction, CFunctionInit, CPointer, CPointerFlags, CPointerInit,
    CallableInfoInit, CallableInfoObject, ForeignLibrary, FunctionKind, Handle, HandleScope, Heap,
    NativeIndex, mode_from_strict,
};

impl FfiState {
    /// Owned NUL-terminated UTF-8 C string.
    pub fn cstring<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        text: &str,
    ) -> FfiResult<Handle<'s, CPointer>> {
        if text.contains('\0') {
            return Err(FfiError::EmbeddedNul);
        }
        let ty = self.types_mut().pointer_to(TypeId::CHAR);
        let bytes = text.as_bytes();
        let address = memory::allocate_zeroed(bytes.len() + 1, 1)?;
        self.track(address);
        memory::copy_in(address, bytes);
        Ok(heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: ty,
                address,
                flags: CPointerFlags::OWNED_EXTERNAL.union(CPointerFlags::NUL_TERMINATED),
                library: None,
            },
            scope,
        ))
    }

    /// Allocate zeroed external storage for an aggregate type.
    pub fn buffer<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        type_id: TypeId,
    ) -> FfiResult<Handle<'s, CBuffer>> {
        let layout = self
            .types()
            .layout(type_id)
            .ok_or(FfiError::NotAnObject(type_id))?;
        let address = memory::allocate_zeroed(layout.size().max(1), layout.align())?;
        self.track(address);
        let pointer_type = self.types_mut().pointer_to(type_id);
        let storage = heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: pointer_type,
                address,
                flags: CPointerFlags::OWNED_EXTERNAL,
                library: None,
            },
            scope,
        );
        Ok(heap.allocate_handle::<CBuffer>(
            CBufferInit {
                type_id,
                storage,
                count: 0,
            },
            scope,
        ))
    }

    /// A pointer with the same address and ownership, retyped to another
    /// pointer type.
    pub fn cast_pointer<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        ty: TypeId,
    ) -> FfiResult<Handle<'s, CPointer>> {
        if !matches!(self.types().get(ty), CType::Pointer(_)) {
            return Err(FfiError::NotAPointer(ty));
        }
        let (address, flags) = {
            let source = pointer.as_tagged(heap);
            (source.address(), source.flags())
        };
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: ty,
                address,
                flags,
                library: None,
            },
            scope,
        );
        if let Some(library) = pointer.as_tagged(heap).library(heap) {
            out.as_tagged(heap).set_library(heap, library);
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
        let types = self.types();
        let ty = pointer.as_tagged(heap).c_type();
        let CType::Pointer(pointee) = types.get(ty) else {
            return Err(FfiError::NotAPointer(ty));
        };
        let element_size = types
            .layout(*pointee)
            .map(|layout| layout.size().max(1))
            .unwrap_or(1) as i64;
        let delta = elements
            .checked_mul(element_size)
            .ok_or(FfiError::AddressOverflow)?;
        let (address, flags) = {
            let source = pointer.as_tagged(heap);
            (source.address(), source.flags())
        };
        let address = address
            .checked_add_signed(delta as isize)
            .ok_or(FfiError::AddressOverflow)?;
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: ty,
                address,
                flags,
                library: None,
            },
            scope,
        );
        if let Some(library) = pointer.as_tagged(heap).library(heap) {
            out.as_tagged(heap).set_library(heap, library);
        }
        Ok(out)
    }

    /// A callable C function object for an already-resolved target.
    pub fn function<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        trampoline: NativeIndex,
        signature: TypeId,
        target: &Handle<CPointer>,
        library: &Handle<ForeignLibrary>,
    ) -> FfiResult<Handle<'s, CFunction>> {
        let arity = match self.types().get(signature) {
            CType::Function(function) => function.params.len(),
            _ => return Err(FfiError::NotAFunction(signature)),
        };
        let info = heap.allocate_handle::<CallableInfoObject>(
            CallableInfoInit::Native {
                index: trampoline,
                mode: mode_from_strict(true),
            },
            scope,
        );
        info.as_tagged(heap)
            .set_metadata_full(heap, None, arity, arity, FunctionKind::Normal);
        Ok(heap.allocate_handle::<CFunction>(
            CFunctionInit {
                info,
                signature,
                target: target.clone(),
                library: Some(library.clone()),
            },
            scope,
        ))
    }
}
