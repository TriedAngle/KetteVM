use core::alloc::Layout;

use heap_api::{GcHost, HeapPtr, ObjectAddr};

use crate::chunk::ChunkHeader;

pub const ALIGN: usize = 16;

pub fn need_for(layout: Layout) -> usize {
    debug_assert!(layout.size() > 0, "zero-sized allocation");
    debug_assert!(
        layout.align() <= ALIGN,
        "alignment above {ALIGN} unsupported"
    );
    layout.size().next_multiple_of(ALIGN)
}

/// Header of a free run
#[repr(C)]
pub struct FreeHeader {
    size: usize,
    next: Option<HeapPtr<FreeHeader>>,
}

pub struct FreeList {
    head: Option<HeapPtr<FreeHeader>>,
    free_bytes: usize,
    live_bytes: usize,
}

impl FreeList {
    fn header_at(addr: usize) -> HeapPtr<FreeHeader> {
        // Safety: free runs live in the chunk's object region.
        unsafe { HeapPtr::from_addr(addr) }
    }

    fn prepend(head: Option<HeapPtr<FreeHeader>>, at: HeapPtr<FreeHeader>, size: usize) -> Self {
        // Safety: `at` starts a free run of `size` bytes.
        unsafe { at.write(FreeHeader { size, next: head }) };
        Self {
            head: Some(at),
            free_bytes: size,
            live_bytes: 0,
        }
    }

    pub fn covering(base: HeapPtr<u8>, size: usize) -> Self {
        Self::prepend(None, base.cast(), size)
    }

    pub fn free_bytes(&self) -> usize {
        self.free_bytes
    }

    pub fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    pub fn from_promotion(base: HeapPtr<u8>, size: usize, live: usize) -> Self {
        let mut list = Self::prepend(None, base.cast(), size);
        list.live_bytes = live;
        list
    }

    pub fn full(live_bytes: usize) -> Self {
        Self {
            head: None,
            free_bytes: 0,
            live_bytes,
        }
    }

    /// First-fit
    pub fn allocate(&mut self, layout: Layout) -> Option<HeapPtr<u8>> {
        let need = need_for(layout);
        let mut prev: Option<HeapPtr<FreeHeader>> = None;
        let mut run = self.head?;
        loop {
            let available = run.as_ref().size;
            if available >= need {
                let remainder = available - need;
                let next = run.as_ref().next;
                let given = if remainder >= 2 * ALIGN {
                    let rest = Self::header_at(run.addr() + need);
                    // Safety: `rest` is the unused tail of `run`.
                    unsafe {
                        rest.write(FreeHeader {
                            size: remainder,
                            next,
                        })
                    };
                    Self::splice(&mut prev, Some(rest), &mut self.head);
                    need
                } else {
                    Self::splice(&mut prev, next, &mut self.head);
                    available
                };
                self.free_bytes -= given;
                self.live_bytes += given;
                return Some(run.cast());
            }
            let next = run.as_ref().next?;
            prev = Some(run);
            run = next;
        }
    }

    /// Point `prev.next` (or the head, when `prev` is `None`) at `new`.
    fn splice(
        prev: &mut Option<HeapPtr<FreeHeader>>,
        new: Option<HeapPtr<FreeHeader>>,
        head: &mut Option<HeapPtr<FreeHeader>>,
    ) {
        match prev {
            // Safety: the free-list mutex is held.
            Some(prev) => unsafe { prev.as_mut() }.next = new,
            None => *head = new,
        }
    }

    /// Rebuilds the free list from the chunk's mark bits
    pub fn sweep(&mut self, header: &ChunkHeader, usable: usize, host: &GcHost) {
        let start = header.object_base();
        let end = start + usable;
        self.head = None;
        self.free_bytes = 0;
        let mut live = 0usize;
        let mut tail: Option<HeapPtr<FreeHeader>> = None;
        let mut gap_start = start;
        for addr in header.mark_iter(usable) {
            // Safety: mark bits only cover the chunk's object region.
            let object = unsafe { ObjectAddr::from_addr(addr) };
            let object_size = (host.layout_of)(object).size().next_multiple_of(ALIGN);
            let gap = addr - gap_start;
            if gap >= 2 * ALIGN {
                self.push_free(&mut tail, gap_start, gap);
            } else {
                live += gap;
            }
            live += object_size;
            gap_start = addr + object_size;
        }
        let gap = end - gap_start;
        if gap >= 2 * ALIGN {
            self.push_free(&mut tail, gap_start, gap);
        } else {
            live += gap;
        }
        self.live_bytes = live;
    }

    fn push_free(&mut self, tail: &mut Option<HeapPtr<FreeHeader>>, addr: usize, size: usize) {
        let header = Self::header_at(addr);
        // Safety: `addr` starts a gap of `size` bytes, written under the
        // free-list mutex.
        unsafe { header.write(FreeHeader { size, next: None }) };
        match *tail {
            // Safety: the free-list mutex is held.
            Some(mut tail) => unsafe { tail.as_mut() }.next = Some(header),
            None => self.head = Some(header),
        }
        *tail = Some(header);
        self.free_bytes += size;
    }
}
