//! Constructors shared by the natives and by Rust callers.

use crate::ffi::{CType, FfiError, FfiResult, FfiState, TypeId};
use crate::{
    CBuffer, CBufferInit, CFunction, CFunctionInit, CPointer, CPointerFlags, CPointerInit,
    CallableInfoInit, CallableInfoObject, FixedByteArray, ForeignLibrary, FunctionKind, Handle,
    HandleScope, Heap, NativeIndex, mode_from_strict,
};

impl FfiState {
    /// VM-owned NUL-terminated UTF-8 C string.
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
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        let data = heap.allocate_handle::<FixedByteArray>(&bytes, scope);
        Ok(heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: ty,
                address: 0,
                flags: CPointerFlags::VM_BACKED.union(CPointerFlags::NUL_TERMINATED),
                library: None,
                owner: Some(data),
            },
            scope,
        ))
    }

    /// Allocate zeroed VM-owned storage for an aggregate type.
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
        if layout.align() > 16 {
            return Err(FfiError::UnsupportedAlignment(layout.align()));
        }
        let size = layout.size().max(1);
        let data = heap.allocate_handle::<FixedByteArray>(&vec![0u8; size], scope);
        Ok(heap.allocate_handle::<CBuffer>(
            CBufferInit {
                type_id,
                data,
                count: 0,
            },
            scope,
        ))
    }

    /// A pointer with the same storage and ownership, retyped to another
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
        let (address, flags, owner, library) = {
            let source = pointer.as_tagged(heap);
            (
                source.raw_address(),
                source.flags(),
                source.owner(heap).map(|owner| scope.handle(owner)),
                source.library(heap).map(|library| scope.handle(library)),
            )
        };
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id: ty,
                address,
                flags,
                library: None,
                owner,
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
        let (address, flags, owner, library) = {
            let source = pointer.as_tagged(heap);
            (
                source.raw_address(),
                source.flags(),
                source.owner(heap).map(|owner| scope.handle(owner)),
                source.library(heap).map(|library| scope.handle(library)),
            )
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
                owner,
            },
            scope,
        );
        if let Some(library) = library {
            out.as_tagged(heap)
                .set_library(heap, library.as_tagged(heap));
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
