use crate::ffi::{FfiError, FfiResult};
use core::ptr;

/// Allocate `size` zeroed bytes with at least `align` alignment.
///
/// The block is released with [`free`], which is the process allocator C code
/// uses, so ownership can be handed across the FFI boundary.
pub fn alloc_zeroed(size: usize, align: usize) -> FfiResult<*mut u8> {
    let size = size.max(1);
    // Safety: `calloc` either allocates a zeroed block or returns null.
    // Callers reject alignments above 16, which is what the allocator
    // guarantees.
    let address = unsafe { libc::calloc(1, size) } as *mut u8;
    if address.is_null() {
        return Err(FfiError::OutOfMemory { size, align });
    }
    Ok(address)
}

pub fn free(address: *mut u8) {
    if address.is_null() {
        return;
    }
    // Safety: `address` came from `alloc_zeroed` and was not freed yet.
    unsafe { libc::free(address.cast()) };
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn copy_out(address: *const u8, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    // Safety: the caller checked the pointer is live and `out.len()` matches
    // the C object's size.
    unsafe { ptr::copy_nonoverlapping(address, out.as_mut_ptr(), out.len()) };
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn copy_in(address: *mut u8, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: the caller checked the pointer is live and writable for
    // `bytes.len()` bytes.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len()) };
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn copy_bytes(src: *const u8, dst: *mut u8, len: usize) {
    if len == 0 {
        return;
    }
    // Safety: the caller checked both regions are live and non-overlapping.
    unsafe { ptr::copy_nonoverlapping(src, dst, len) };
}

pub fn c_str_len(address: *const u8) -> usize {
    let mut len = 0;
    // Safety: the caller guarantees `address` is live for the scan.
    while unsafe { *address.wrapping_add(len) } != 0 {
        len += 1;
    }
    len
}

pub fn c_wstr_len(address: *const u8) -> usize {
    let mut len = 0;
    while len < usize::MAX / 2 {
        let mut bytes = [0u8; 2];
        copy_out_at(address, len * 2, &mut bytes);
        if u16::from_ne_bytes(bytes) == 0 {
            return len;
        }
        len += 1;
    }
    len
}

pub fn copy_out_at(address: *const u8, offset: usize, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    // Safety: the caller checked the base pointer is live for `offset + len`.
    unsafe { ptr::copy_nonoverlapping(address.wrapping_add(offset), out.as_mut_ptr(), out.len()) };
}

pub fn copy_in_at(address: *mut u8, offset: usize, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: the caller checked the base pointer is live for `offset + len`.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), address.wrapping_add(offset), bytes.len()) };
}
