//! Raw C memory access and external allocation.
//!
//! This is the only module that dereferences foreign addresses. Every public
//! operation here is a plain memory primitive; the caller is responsible for
//! checking pointer liveness before calling. `CPointer` does that in its
//! accessors.
//!
//! Accesses are byte copies so unaligned C pointers (packed structs, `char*`
//! arithmetic) are handled without alignment requirements.

use crate::ffi::FfiError;
use core::ptr;

const MIN_ALIGN: usize = core::mem::align_of::<*const ()>();

/// Allocate zeroed external memory, honoring `align` when it exceeds the C
/// allocator's default. Returns the raw address, or `OutOfMemory`.
pub fn allocate_zeroed(size: usize, align: usize) -> Result<usize, FfiError> {
    let size = size.max(1);
    let align = align.max(MIN_ALIGN).next_power_of_two();

    #[cfg(unix)]
    let address = {
        let mut out: *mut core::ffi::c_void = ptr::null_mut();
        // Safety: posix_memalign writes one pointer into `out`; `align` is a
        // power of two and a multiple of `size_of::<*const ()>()`.
        let status = unsafe { libc::posix_memalign(&mut out, align, size) };
        if status == 0 { out } else { ptr::null_mut() }
    };

    #[cfg(windows)]
    // Safety: `_aligned_malloc` is the matching allocator for `_aligned_free`.
    let address = unsafe { libc::_aligned_malloc(size, align) };

    if address.is_null() {
        return Err(FfiError::OutOfMemory { size, align });
    }

    // Safety: `address` owns `size` writable bytes.
    unsafe { ptr::write_bytes(address.cast::<u8>(), 0, size) };
    Ok(address as usize)
}

/// Free memory returned by [`allocate_zeroed`]. Called once per address.
pub fn free_external(address: usize) {
    if address == 0 {
        return;
    }
    // Safety: the caller only passes addresses from `allocate_zeroed`, once.
    unsafe {
        #[cfg(unix)]
        libc::free(address as *mut core::ffi::c_void);
        #[cfg(windows)]
        libc::_aligned_free(address as *mut core::ffi::c_void);
    }
}

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

/// Copy `out.len()` bytes from `address + offset` into `out`.
pub fn copy_out_at(address: usize, offset: usize, out: &mut [u8]) {
    if out.is_empty() {
        return;
    }
    let at = address
        .checked_add(offset)
        .expect("C buffer offset fits usize");
    // Safety: the caller checked the pointer is live and the range is in
    // bounds for the containing object.
    unsafe { ptr::copy_nonoverlapping(at as *const u8, out.as_mut_ptr(), out.len()) };
}

/// Copy `bytes` into `address + offset`.
pub fn copy_in_at(address: usize, offset: usize, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let at = address
        .checked_add(offset)
        .expect("C buffer offset fits usize");
    // Safety: the caller checked the pointer is live and writable for the
    // range.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len()) };
}
