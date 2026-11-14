//! Constructors shared by the natives and by Rust callers.

use crate::ffi::memory;
use crate::ffi::{FfiError, FfiResult, FfiState, TypeKind};
use crate::{
    CFunction, CFunctionInit, CPointer, CPointerFlags, CPointerInit, CValue, CValueInit,
    CallableInfoInit, CallableInfoObject, FixedByteArray, ForeignLibrary, FunctionKind, Handle,
    HandleScope, Heap, NativeIndex, TypeInfo, mode_from_strict,
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
        let ty = scope.handle(self.builtin(heap, TypeKind::Char));
        let mut bytes = Vec::with_capacity(text.len() + 1);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
        let data = heap.allocate_handle::<FixedByteArray>(&bytes, scope);
        Ok(heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty,
                address: 0,
                flags: CPointerFlags::VM_BACKED.union(CPointerFlags::NUL_TERMINATED),
                library: None,
                owner: Some(data),
            },
            scope,
        ))
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
                inline: Some(inline),
                external: None,
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
        value: &Handle<CValue>,
        ty: &Handle<TypeInfo>,
        delta: usize,
    ) -> Handle<'s, CValue> {
        let (inline, external, offset, map) = {
            let source = value.as_tagged(heap);
            (
                source.inline_data(heap).map(|inline| scope.handle(inline)),
                source
                    .external_pointer(heap)
                    .map(|external| scope.handle(external)),
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
                inline,
                external,
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
    ) -> Handle<'s, CValue> {
        let map = ty
            .as_tagged(heap)
            .value_map(heap)
            .map(|map| scope.handle(map));
        heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                inline: None,
                external: Some(pointer.clone()),
                offset: delta,
            },
            scope,
        )
    }

    /// A pointer with the same storage and ownership, retyped.
    pub fn cast_pointer<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        pointer: &Handle<CPointer>,
        ty: &Handle<TypeInfo>,
    ) -> FfiResult<Handle<'s, CPointer>> {
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
                pointee: ty.clone(),
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
        let (pointee, element_size) = {
            let source = pointer.as_tagged(heap);
            let pointee = source.pointee(heap);
            (pointee, pointee.size().max(1))
        };
        let delta = elements
            .checked_mul(element_size as i64)
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
        let ty = scope.handle(pointee);
        let out = heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty,
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
            let source = value
                .as_tagged(heap)
                .base(heap)
                .ok_or(FfiError::InvalidArgument("C value has no storage"))?;
            (source, ty.as_tagged(heap).size())
        };
        let destination = copy
            .as_tagged(heap)
            .base(heap)
            .ok_or(FfiError::InvalidArgument("C value has no storage"))?;
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
        let map = map.map(|map| scope.handle(map));
        Ok(heap.allocate_handle::<CValue>(
            CValueInit {
                ty: ty.clone(),
                map,
                inline: None,
                external: Some(pointer.clone()),
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
