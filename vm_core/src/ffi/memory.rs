//! Raw C memory access.
//!
//! This is the only module that dereferences foreign addresses. Every public
//! operation here is a plain memory primitive; the caller is responsible for
//! checking pointer liveness before calling. `CPointer` does that in its
//! accessors.
//!
//! Accesses are byte copies or borrowed byte views, so unaligned C pointers
//! (packed structs, `char*` arithmetic) are handled without alignment
//! requirements.

use core::ptr;

/// Copy `out.len()` bytes from `address` into `out`.
pub fn copy_out(address: usize, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    // Safety: the caller checked the pointer is live and `out.len()` matches
    // the C object's size.
    unsafe { ptr::copy_nonoverlapping(address as *const u8, out.as_mut_ptr(), out.len()) };
}

/// Copy `bytes` into `address`.
pub fn copy_in(address: usize, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: the caller checked the pointer is live and writable for
    // `bytes.len()` bytes.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len()) };
}

/// Copy `len` bytes from `src` to `dst`.
pub fn copy_bytes(src: usize, dst: usize, len: usize) {
    if len == 0 {
        return;
    }
    // Safety: the caller checked both regions are live and non-overlapping.
    unsafe { ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, len) };
}

/// Copy `out.len()` bytes from `offset` bytes past `address` into `out`.
pub fn copy_out_at(address: usize, offset: usize, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    // Safety: the caller checked the base pointer is live for `offset + len`.
    unsafe {
        ptr::copy_nonoverlapping(
            (address as *const u8).add(offset),
            out.as_mut_ptr(),
            out.len(),
        )
    };
}

/// Copy `bytes` to `offset` bytes past `address`.
pub fn copy_in_at(address: usize, offset: usize, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Safety: the caller checked the base pointer is live for `offset + len`.
    unsafe {
        ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            (address as *mut u8).add(offset),
            bytes.len(),
        )
    };
}
