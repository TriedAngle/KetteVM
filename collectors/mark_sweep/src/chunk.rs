use crate::{block::ALIGN, block::FreeList, block::need_for};
use core::alloc::Layout;
use heap_api::{AllocError, CellRef, GcHost, HeapPtr, ObjectAddr};
use heap_utils::MMapBuffer;
use std::{
    sync::Arc, sync::Mutex, sync::atomic::AtomicU8, sync::atomic::AtomicU64,
    sync::atomic::AtomicUsize, sync::atomic::Ordering,
};

pub const CHUNK_SIZE: usize = 256 * 1024;

const SWEPT: u8 = 0;
const PENDING: u8 = 1;
const SWEEPING: u8 = 2;

const FLAG_YOUNG: u64 = 1 << 0;

#[repr(C)]
pub struct ChunkHeader {
    /// bit 0: the chunk is in the young set
    flags: AtomicU64,
    /// Remember bits: 8 byte granule
    remembered: [AtomicU64; REMEMBERED_WORDS],
    /// Mark bits: one bit per 16-byte granule in the usable region.
    mark: [AtomicU64; MARK_WORDS],
    _pad: [u8; HEADER_SIZE - 8 - REMEMBERED_WORDS * 8 - MARK_WORDS * 8],
}

/// 512 words cover 512·64·8 = 256 KiB of 8-byte slots: a full chunk.
const REMEMBERED_WORDS: usize = 512;
/// 256 words cover 256·64·16 = 256 KiB of 16-byte granules: a full chunk.
const MARK_WORDS: usize = 256;
pub const HEADER_SIZE: usize = 8 * 1024;

const _: () = {
    assert!(
        size_of::<ChunkHeader>() == HEADER_SIZE,
        "header must be fixed-size"
    );
    assert!(
        HEADER_SIZE.is_multiple_of(ALIGN),
        "usable region must start 16-aligned"
    );
    assert!(
        (CHUNK_SIZE - HEADER_SIZE).is_multiple_of(ALIGN),
        "usable region must end 16-aligned"
    );
    assert!(REMEMBERED_WORDS * 64 * 8 >= CHUNK_SIZE - HEADER_SIZE);
    assert!(MARK_WORDS * 64 * 16 >= CHUNK_SIZE - HEADER_SIZE);
};

impl ChunkHeader {
    fn init(&self, young: bool) {
        for word in &self.remembered {
            word.store(0, Ordering::Release);
        }
        for word in &self.mark {
            word.store(0, Ordering::Release);
        }
        self.flags
            .store(if young { FLAG_YOUNG } else { 0 }, Ordering::Release);
    }

    pub fn object_base(&self) -> usize {
        self as *const Self as usize + HEADER_SIZE
    }

    pub fn young(&self) -> bool {
        self.flags.load(Ordering::Acquire) & FLAG_YOUNG != 0
    }

    fn remembered_index(&self, addr: usize) -> (usize, u32) {
        let bit = (addr - self.object_base()) >> 3;
        debug_assert!(
            addr >= self.object_base() && addr & 0b111 == 0,
            "unaligned remembered slot"
        );
        (bit >> 6, (bit & 63) as u32)
    }

    pub fn remember(&self, slot_addr: usize) {
        let (word, bit) = self.remembered_index(slot_addr);
        self.remembered[word].fetch_or(1 << bit, Ordering::AcqRel);
    }

    pub fn remembered_iter(&self, usable: usize) -> impl Iterator<Item = usize> {
        let base = self.object_base();
        let bits = usable >> 3;
        HeaderBits {
            words: &self.remembered,
            base,
            shift: 3,
            word: 0,
            bits: 0,
            limit: bits,
        }
    }

    pub fn clear_remembered(&self) {
        for word in &self.remembered {
            word.store(0, Ordering::Release);
        }
    }

    fn mark_index(&self, addr: usize) -> (usize, u32) {
        debug_assert!(
            addr >= self.object_base() && addr & 0b1111 == 0,
            "unaligned mark address"
        );
        let bit = (addr - self.object_base()) >> 4;
        (bit >> 6, (bit & 63) as u32)
    }

    pub fn mark_set(&self, addr: usize) -> bool {
        let (word, bit) = self.mark_index(addr);
        self.mark[word].fetch_or(1 << bit, Ordering::AcqRel) & (1 << bit) == 0
    }

    pub fn mark_is_set(&self, addr: usize) -> bool {
        let (word, bit) = self.mark_index(addr);
        self.mark[word].load(Ordering::Acquire) & (1 << bit) != 0
    }

    pub fn mark_iter(&self, usable: usize) -> impl Iterator<Item = usize> {
        let base = self.object_base();
        let bits = usable >> 4;
        HeaderBits {
            words: &self.mark,
            base,
            shift: 4,
            word: 0,
            bits: 0,
            limit: bits,
        }
    }

    pub fn clear_mark(&self) {
        for word in &self.mark {
            word.store(0, Ordering::Release);
        }
    }
}

/// The mapped region the chunks are carved from. Cheap to copy, so the
/// collector threads can each hold their own view of it.
#[derive(Clone, Copy)]
pub struct Reservation {
    base: usize,
    size: usize,
}

impl Reservation {
    pub fn base(&self) -> usize {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn end(&self) -> usize {
        self.base + self.size
    }

    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.end()
    }

    pub fn chunk_base(&self, addr: usize) -> usize {
        self.base + (addr - self.base) / CHUNK_SIZE * CHUNK_SIZE
    }

    /// The header of the chunk containing `addr`.
    pub fn header(&self, addr: usize) -> &ChunkHeader {
        debug_assert!(self.contains(addr), "address outside the reservation");
        // Safety: the reservation is mapped read-write for its whole length
        // and each chunk starts with its header.
        unsafe { &*(self.chunk_base(addr) as *const ChunkHeader) }
    }

    /// The object starting at `addr`.
    pub fn object(&self, addr: usize) -> ObjectAddr {
        debug_assert!(self.contains(addr), "object outside the reservation");
        debug_assert_eq!(addr % ALIGN, 0, "unaligned object address");
        // Safety: inside the reservation and aligned.
        unsafe { ObjectAddr::from_addr(addr) }
    }

    /// The traced slot at `addr`.
    pub fn cell(&self, addr: usize) -> CellRef {
        debug_assert!(self.contains(addr), "cell outside the reservation");
        // Safety: inside the reservation.
        unsafe { CellRef::from_addr(addr) }
    }
}

struct HeaderBits<'a> {
    words: &'a [AtomicU64],
    base: usize,
    shift: u32,
    word: usize,
    bits: u64,
    limit: usize,
}

impl Iterator for HeaderBits<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        loop {
            if self.bits != 0 {
                let bit = self.bits.trailing_zeros() as usize;
                self.bits &= self.bits - 1;
                let index = ((self.word - 1) << 6) + bit;
                if index < self.limit {
                    return Some(self.base + (index << self.shift));
                }
                continue;
            }
            if self.word >= self.words.len() {
                return None;
            }
            self.bits = self.words[self.word].load(Ordering::Acquire);
            self.word += 1;
        }
    }
}

pub struct Chunk {
    index: usize,
    base: HeapPtr<u8>,
    header: HeapPtr<ChunkHeader>,
    object_base: HeapPtr<u8>,
    size: usize,
    free: Mutex<FreeList>,
    free_bytes: AtomicUsize,
    state: AtomicU8,
}

impl Chunk {
    fn new(index: usize, base: HeapPtr<u8>, size: usize, young: bool) -> Self {
        let header = base.cast::<ChunkHeader>();
        unsafe { header.as_ref() }.init(young);
        // Safety: a chunk is header-then-objects; `size` covers both.
        let object_base = unsafe { base.offset(HEADER_SIZE) };
        Self {
            free: Mutex::new(FreeList::covering(object_base, size - HEADER_SIZE)),
            free_bytes: AtomicUsize::new(size - HEADER_SIZE),
            state: AtomicU8::new(SWEPT),
            index,
            base,
            header,
            object_base,
            size,
        }
    }

    pub fn base_addr(&self) -> usize {
        self.base.addr()
    }

    pub fn base(&self) -> HeapPtr<u8> {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn header(&self) -> &ChunkHeader {
        // Safety: the chunk header lives in the reservation for the chunk's
        // whole lifetime and is not moved by collection.
        unsafe { self.header.as_ref() }
    }

    pub fn object_base(&self) -> usize {
        self.object_base.addr()
    }

    pub fn object_base_ptr(&self) -> HeapPtr<u8> {
        self.object_base
    }

    pub fn object_size(&self) -> usize {
        self.size - HEADER_SIZE
    }

    pub fn is_young(&self) -> bool {
        self.header().young()
    }

    fn state(&self) -> u8 {
        self.state.load(Ordering::Relaxed)
    }
}

pub struct ChunkedHeap {
    buffer: MMapBuffer,
    chunk_size: usize,
    slots: Vec<Option<Arc<Chunk>>>,
    pooled: Vec<usize>,
    carved: usize,
    pending_chunks: usize,
    sweep_live: usize,
    completed_live: Option<usize>,
    atomic_sweep_block: bool,
    young_chunks: usize,
}

impl ChunkedHeap {
    pub fn new(reserve: usize) -> Result<Self, AllocError> {
        let buffer = MMapBuffer::new(reserve)?;
        let chunk_size = CHUNK_SIZE.min(buffer.size());
        let slots = buffer.size().div_ceil(chunk_size);
        Ok(Self {
            buffer,
            chunk_size,
            slots: (0..slots).map(|_| None).collect(),
            pooled: Vec::new(),
            carved: 0,
            pending_chunks: 0,
            sweep_live: 0,
            completed_live: None,
            atomic_sweep_block: false,
            young_chunks: 0,
        })
    }

    pub fn young_chunk_count(&self) -> usize {
        self.young_chunks
    }

    pub fn reservation(&self) -> Reservation {
        Reservation {
            base: self.buffer.start().addr(),
            size: self.buffer.size(),
        }
    }

    pub fn is_young_addr(&self, addr: usize) -> bool {
        self.reservation().header(addr).young()
    }

    pub fn remembered_slots(&self) -> Vec<CellRef> {
        let reservation = self.reservation();
        let mut slots = Vec::new();
        for chunk in self.slots.iter().flatten() {
            slots.extend(
                chunk
                    .header()
                    .remembered_iter(chunk.object_size())
                    .map(|addr| reservation.cell(addr)),
            );
        }
        slots
    }

    pub fn clear_remembered_sets(&self) {
        for chunk in self.slots.iter().flatten() {
            chunk.header().clear_remembered();
        }
    }

    pub fn young_indices(&self) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().filter(|c| c.is_young()).map(|_| i))
            .collect()
    }

    pub fn remove_chunks(&mut self, indices: &[usize]) {
        for &index in indices {
            let removed = self.slots[index].take().unwrap();
            debug_assert!(removed.is_young(), "only young chunks are freed wholesale");
            self.young_chunks -= 1;
            // Keep the pages resident: minors recycle the same young chunks
            // right away, and MADV_DONTNEED here only buys refault churn on
            // the next allocation cycle.
            drop(removed);
            self.pooled.push(index);
        }
    }

    pub fn finish_promotion(&mut self, chunk: &Chunk, cursor: usize) {
        let remainder = chunk.object_size() - cursor;
        let free = if remainder >= 2 * ALIGN {
            // Safety: the cursor stays inside the chunk's object region.
            let base = unsafe { chunk.object_base_ptr().offset(cursor) };
            FreeList::from_promotion(base, remainder, cursor)
        } else {
            FreeList::full(chunk.object_size())
        };
        chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
        *chunk.free.lock().unwrap() = free;
    }

    pub fn activate_promotion_chunk(&mut self) -> Option<Arc<Chunk>> {
        self.activate_chunk(false)
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn reserve_size(&self) -> usize {
        self.buffer.size()
    }

    pub fn committed_bytes(&self) -> usize {
        self.slots.iter().flatten().map(|c| c.object_size()).sum()
    }

    pub fn live_bytes(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .map(|c| c.free.lock().unwrap().live_bytes())
            .sum()
    }

    pub fn free_bytes(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .map(|c| c.free_bytes.load(Ordering::Relaxed))
            .sum()
    }

    pub fn active_chunks(&self) -> usize {
        self.carved - self.pooled.len()
    }

    pub fn available_chunks(&self) -> usize {
        self.pooled.len() + (self.slots.len() - self.carved)
    }

    pub fn pending_chunks(&self) -> usize {
        self.pending_chunks
    }

    pub fn in_heap(&self, addr: usize) -> bool {
        let base = self.buffer.start().addr();
        addr >= base && addr < base + self.carved * self.chunk_size
    }

    pub fn chunk_of(&self, addr: usize) -> &Chunk {
        let base = self.buffer.start().addr();
        let index = (addr - base) / self.chunk_size;
        debug_assert!(index < self.slots.len(), "address outside reservation");
        self.slots[index]
            .as_ref()
            .expect("pointer into pooled or uncarved chunk")
    }

    /// First-fit
    pub fn allocate(
        &mut self,
        layout: Layout,
        host: Option<&GcHost>,
    ) -> (Option<HeapPtr<u8>>, Option<Arc<Chunk>>) {
        self.allocate_impl(layout, host, false)
    }

    /// Nursery allocation for TLAB refills: only young chunks serve it, so
    /// freshly allocated small objects are always minor-GC candidates.
    pub fn allocate_young(
        &mut self,
        layout: Layout,
        host: Option<&GcHost>,
    ) -> (Option<HeapPtr<u8>>, Option<Arc<Chunk>>) {
        self.allocate_impl(layout, host, true)
    }

    fn allocate_impl(
        &mut self,
        layout: Layout,
        host: Option<&GcHost>,
        young_only: bool,
    ) -> (Option<HeapPtr<u8>>, Option<Arc<Chunk>>) {
        if let Some(ptr) = self.allocate_swept(layout, young_only) {
            return (Some(ptr), None);
        }
        if host.is_some()
            && let Some(chunk) = self.claim_pending(young_only)
        {
            return (None, Some(chunk));
        }
        match self.activate_chunk(true) {
            Some(chunk) => {
                let mut free = chunk.free.lock().unwrap();
                let ptr = free.allocate(layout);
                chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
                (ptr, None)
            }
            None => (None, None),
        }
    }

    fn allocate_swept(&mut self, layout: Layout, young_only: bool) -> Option<HeapPtr<u8>> {
        let need = need_for(layout);
        for slot in &self.slots {
            let Some(chunk) = slot else { continue };
            if young_only && !chunk.is_young() {
                continue;
            }
            if chunk.state() != SWEPT {
                continue;
            }
            if chunk.free_bytes.load(Ordering::Relaxed) < need {
                continue;
            }
            let mut free = chunk.free.lock().unwrap();
            if let Some(ptr) = free.allocate(layout) {
                chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
                return Some(ptr);
            }
        }
        None
    }

    pub fn claim_pending(&mut self, young_only: bool) -> Option<Arc<Chunk>> {
        if self.atomic_sweep_block {
            return None;
        }
        self.claim_pending_unblocked(young_only)
    }

    fn claim_pending_unblocked(&mut self, young_only: bool) -> Option<Arc<Chunk>> {
        for slot in &self.slots {
            let Some(chunk) = slot else { continue };
            if young_only && !chunk.is_young() {
                continue;
            }
            if chunk.state() == PENDING {
                chunk.state.store(SWEEPING, Ordering::Relaxed);
                return Some(Arc::clone(chunk));
            }
        }
        None
    }

    pub fn set_sweep_block(&mut self, blocked: bool) {
        self.atomic_sweep_block = blocked;
    }

    pub fn any_sweeping(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|chunk| chunk.state() == SWEEPING)
    }

    pub fn sweep_claimed(chunk: &Chunk, host: &GcHost) -> usize {
        let mut free = chunk.free.lock().unwrap();
        free.sweep(chunk.header(), chunk.object_size(), host);
        chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
        chunk.header().clear_mark();
        free.live_bytes()
    }

    pub fn publish_swept(&mut self, chunk: &Chunk, live: usize) {
        if live == 0 {
            let index = chunk.index;
            let removed = self.slots[index].take().unwrap();
            if removed.is_young() {
                self.young_chunks -= 1;
            }
            self.buffer.decommit(removed.base(), removed.size);
            drop(removed);
            self.pooled.push(index);
        } else {
            chunk.state.store(SWEPT, Ordering::Relaxed);
        }
        self.sweep_live += live;
        self.pending_chunks -= 1;
        if self.pending_chunks == 0 {
            self.completed_live = Some(self.sweep_live);
        }
    }

    pub fn take_completed_live(&mut self) -> Option<usize> {
        self.completed_live.take()
    }

    fn chunk_region(&self, index: usize) -> (HeapPtr<u8>, usize) {
        let offset = index * self.chunk_size;
        let size = self.chunk_size.min(self.reserve_size() - offset);
        (self.buffer.region(offset, size), size)
    }

    fn activate_chunk(&mut self, young: bool) -> Option<Arc<Chunk>> {
        let index = match self.pooled.pop() {
            Some(index) => index,
            None if self.carved < self.slots.len() => {
                let index = self.carved;
                self.carved += 1;
                index
            }
            None => return None,
        };
        let (base, size) = self.chunk_region(index);
        let chunk = Arc::new(Chunk::new(index, base, size, young));
        if young {
            self.young_chunks += 1;
        }
        self.slots[index] = Some(Arc::clone(&chunk));
        Some(chunk)
    }

    pub fn flag_all_pending(&mut self) {
        let mut count = 0usize;
        for chunk in self.slots.iter().flatten() {
            debug_assert_ne!(chunk.state(), SWEEPING, "cycle started mid-sweep");
            chunk.state.store(PENDING, Ordering::Relaxed);
            count += 1;
        }
        self.pending_chunks = count;
        self.sweep_live = 0;
        if count == 0 {
            self.completed_live = Some(0);
        }
    }

    pub fn finish_pending(&mut self, host: &GcHost) -> bool {
        while let Some(chunk) = self.claim_pending_unblocked(false) {
            let live = Self::sweep_claimed(&chunk, host);
            self.publish_swept(&chunk, live);
        }
        !self.any_sweeping()
    }
}
