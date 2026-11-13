//! Pointer operations: allocation, liveness, and typed memory access.

use crate::ffi::memory::{allocate_zeroed, copy_in, copy_out, free_external};
use crate::ffi::{FfiError, FfiResult, FfiState, TypeId};
use crate::{CPointer, CPointerFlags, CPointerInit, Handle, HandleScope, Heap};

impl FfiState {
    /// Allocate a typed, zeroed, externally owned C object.
    ///
    /// The returned handle stays rooted in `scope`; the storage is `malloc`
    /// owned and must be released with [`CPointer::free_external`].
    pub fn allocate<'s>(
        &self,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        type_id: TypeId,
    ) -> FfiResult<Handle<'s, CPointer>> {
        let layout = self
            .types()
            .layout(type_id)
            .ok_or(FfiError::NotAnObject(type_id))?;
        let address = allocate_zeroed(layout.size(), layout.align())?;
        Ok(heap.allocate_handle::<CPointer>(
            CPointerInit {
                type_id,
                address,
                flags: CPointerFlags::OWNED_EXTERNAL,
                library: None,
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
        if self.is_null() {
            return Err(FfiError::NullPointer);
        }
        if let Some(library) = self.library(heap)
            && !library.is_live(heap)
        {
            return Err(FfiError::LibraryClosed);
        }
        Ok(())
    }

    /// Release externally owned storage. Idempotent: a second call is a no-op
    /// and other owners are rejected.
    pub fn free_external(&self, heap: &Heap) -> FfiResult<()> {
        if self.is_dead() {
            return Ok(());
        }
        if !self.flags().contains(CPointerFlags::OWNED_EXTERNAL) {
            return Err(FfiError::NotOwned);
        }
        free_external(self.address());
        self.kill(heap);
        Ok(())
    }

    pub fn read_bytes(&self, heap: &Heap, out: &mut [u8]) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_out(self.address(), out);
        Ok(())
    }

    pub fn write_bytes(&self, heap: &Heap, bytes: &[u8]) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_in(self.address(), bytes);
        Ok(())
    }

    /// Pointer-width access, for fields of pointer type in C structs.
    pub fn read_pointer(&self, heap: &Heap) -> FfiResult<usize> {
        self.check_live(heap)?;
        let mut bytes = [0u8; core::mem::size_of::<usize>()];
        copy_out(self.address(), &mut bytes);
        Ok(usize::from_ne_bytes(bytes))
    }

    pub fn write_pointer(&self, heap: &Heap, value: usize) -> FfiResult<()> {
        self.check_live(heap)?;
        copy_in(self.address(), &value.to_ne_bytes());
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
                    copy_out(self.address(), &mut bytes);
                    Ok(<$ty>::from_ne_bytes(bytes))
                }

                pub fn $write(&self, heap: &Heap, value: $ty) -> FfiResult<()> {
                    self.check_live(heap)?;
                    copy_in(self.address(), &value.to_ne_bytes());
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
