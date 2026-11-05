use crate::HeapPtr;

/// A thread-local allocation window: memory the owning thread bumps into
/// objects without touching the collector.
///
/// The VM owns the window; collectors only hand out fresh ones through
/// [`LocalHeap::allocate_slow`](crate::LocalHeap::allocate_slow). Every
/// window starts [`Tlab::ALIGN`]-aligned and is consumed in multiples of
/// that alignment, so `try_alloc` never needs to realign the cursor.
#[derive(Debug)]
pub struct Tlab {
    cursor: Option<HeapPtr<u8>>,
    end: usize,
}

impl Tlab {
    /// The allocation alignment guaranteed by the heap: every object starts
    /// at a multiple of this. Larger object alignments are unsupported.
    pub const ALIGN: usize = 16;

    pub const fn empty() -> Self {
        Self {
            cursor: None,
            end: 0,
        }
    }

    /// A window covering `[start, start + len)`. `start` must be
    /// `ALIGN`-aligned and `len` a multiple of it.
    pub fn new(start: HeapPtr<u8>, len: usize) -> Self {
        debug_assert_eq!(start.addr() % Self::ALIGN, 0);
        debug_assert_eq!(len % Self::ALIGN, 0);
        Self {
            cursor: Some(start),
            end: start.addr() + len,
        }
    }

    /// Reserve `size` bytes (a multiple of `ALIGN`) from the window, or
    /// `None` when it does not fit. Never touches the collector.
    #[inline(always)]
    pub fn try_alloc(&mut self, size: usize) -> Option<HeapPtr<u8>> {
        let cursor = self.cursor?;
        let next = cursor.addr() + size;
        if next > self.end {
            return None;
        }
        // Safety: the window is one collector allocation; `next` is in it.
        self.cursor = Some(unsafe { cursor.offset(size) });
        Some(cursor)
    }

    /// Drop the window: after a park or collection the collector may
    /// reclaim the span, so the owner must treat it as gone.
    pub fn invalidate(&mut self) {
        self.cursor = None;
        self.end = 0;
    }
}
