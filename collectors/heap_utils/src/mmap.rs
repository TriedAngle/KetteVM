use core::ptr::NonNull;

use heap_api::{AllocError, HeapPtr};

/// Lazily-committed anonymous memory mapping. Pages are provided by the OS
/// on first touch; nothing is committed up front.
pub struct MMapBuffer {
    start: HeapPtr<u8>,
    size: usize,
}

impl MMapBuffer {
    pub fn new(size: usize) -> Result<Self, AllocError> {
        let page = page_size();
        let size = size.next_multiple_of(page);
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON | map_noreserve(),
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(AllocError::OutOfMemory(
                core::alloc::Layout::from_size_align(size, page).unwrap(),
            ));
        }
        // Safety: mmap returned a live mapping of `size` bytes.
        let start = unsafe { HeapPtr::from_non_null(NonNull::new_unchecked(ptr.cast())) };
        Ok(Self { start, size })
    }

    pub fn start(&self) -> HeapPtr<u8> {
        self.start
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn contains(&self, addr: usize) -> bool {
        (self.start.addr()..self.start.addr() + self.size).contains(&addr)
    }

    /// The `len` bytes at `offset`, as a pointer into the mapping.
    pub fn region(&self, offset: usize, len: usize) -> HeapPtr<u8> {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.size),
            "region outside the mapping"
        );
        // Safety: the mapping covers `offset..offset + len` and never moves.
        unsafe { self.start.offset(offset) }
    }

    pub fn decommit(&self, addr: HeapPtr<u8>, len: usize) {
        debug_assert!(self.contains(addr.addr()));
        unsafe {
            libc::madvise(addr.as_non_null().as_ptr().cast(), len, libc::MADV_DONTNEED);
        }
    }
}

impl Drop for MMapBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.start.as_non_null().as_ptr().cast(), self.size);
        }
    }
}

#[cfg(target_os = "linux")]
fn map_noreserve() -> libc::c_int {
    libc::MAP_NORESERVE
}

#[cfg(not(target_os = "linux"))]
fn map_noreserve() -> libc::c_int {
    0
}

fn page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}
