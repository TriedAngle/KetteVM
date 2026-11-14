//! Pointer operations: allocation, liveness, and typed memory access.

use crate::ffi::memory::{copy_in, copy_out};
use crate::ffi::{FfiError, FfiResult, FfiState};
use crate::{
    CPointer, CPointerFlags, CPointerInit, FixedByteArray, Handle, HandleScope, Heap, TypeInfo,
};

impl FfiState {
    /// Allocate a typed, zeroed C object in a VM-owned byte array.
    ///
    /// The returned pointer is `VM_BACKED`: it keeps the storage alive and
    /// resolves its offset on every access, so it survives collections.
    pub fn allocate<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        ty: &Handle<TypeInfo>,
    ) -> FfiResult<Handle<'s, CPointer>> {
        let (id, layout) = {
            let info = ty.as_tagged(heap);
            (info.id(), info.object_layout())
        };
        let layout = layout.ok_or(FfiError::NotAnObject(id))?;
        if layout.align() > 16 {
            return Err(FfiError::UnsupportedAlignment(layout.align()));
        }
        let size = layout.size().max(1);
        let data = heap.allocate_handle::<FixedByteArray>(&vec![0u8; size], scope);
        Ok(heap.allocate_handle::<CPointer>(
            CPointerInit {
                pointee: ty.clone(),
                address: 0,
                flags: CPointerFlags::VM_BACKED,
                library: None,
                owner: Some(data),
            },
            scope,
        ))
    }
}

impl CPointer {
    /// A dead pointer is never usable; a live null pointer is not
    /// dereferenceable; a pointer whose library was closed is dead.
    pub fn check_live(&self, heap: &Heap) -> FfiResult<()> {
        if self.is_dead() {
            return Err(FfiError::DeadPointer);
        }
        if self.is_null(heap) {
            return Err(FfiError::NullPointer);
        }
        if let Some(library) = self.library(heap)
            && !library.is_live(heap)
        {
            return Err(FfiError::LibraryClosed);
        }
        Ok(())
    }

    pub fn read_bytes(&self, heap: &Heap, out: &mut [u8]) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_out(self.address(heap), out);
        Ok(())
    }

    pub fn write_bytes(&self, heap: &Heap, bytes: &[u8]) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_in(self.address(heap), bytes);
        Ok(())
    }

    /// Pointer-width access, for fields of pointer type in C structs.
    pub fn read_pointer(&self, heap: &Heap) -> FfiResult<usize> {
        self.check_live(heap)?;
        let mut bytes = [0u8; core::mem::size_of::<usize>()];
        copy_out(self.address(heap), &mut bytes);
        Ok(usize::from_ne_bytes(bytes))
    }

    pub fn write_pointer(&self, heap: &Heap, value: usize) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_in(self.address(heap), &value.to_ne_bytes());
        Ok(())
    }
}

macro_rules! scalar_accessors {
    ($($read:ident, $write:ident: $ty:ty, $size:literal;)*) => {
        impl CPointer {
            $(
                pub fn $read(&self, heap: &Heap) -> FfiResult<$ty> {
                    self.check_live(heap)?;
                    let mut bytes = [0u8; $size];
                    copy_out(self.address(heap), &mut bytes);
                    Ok(<$ty>::from_ne_bytes(bytes))
                }

                pub fn $write(&self, heap: &Heap, value: $ty) -> FfiResult<()> {
                    self.check_live(heap)?;
                    copy_in(self.address(heap), &value.to_ne_bytes());
                    Ok(())
                }
            )*
        }
    };
}

scalar_accessors! {
    read_i8, write_i8: i8, 1;
    read_u8, write_u8: u8, 1;
    read_i16, write_i16: i16, 2;
    read_u16, write_u16: u16, 2;
    read_i32, write_i32: i32, 4;
    read_u32, write_u32: u32, 4;
    read_i64, write_i64: i64, 8;
    read_u64, write_u64: u64, 8;
    read_f32, write_f32: f32, 4;
    read_f64, write_f64: f64, 8;
}
