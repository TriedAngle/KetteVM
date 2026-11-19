use crate::{block::ALIGN, block::FreeList, block::need_for};
use core::alloc::Layout;
use heap_api::{AllocError, CellRef, GcHost, HeapPtr, HeapStats, ObjectAddr};
use heap_utils::MMapBuffer;
use std::{
    collections::VecDeque, sync::Arc, sync::Mutex, sync::atomic::AtomicU8, sync::atomic::AtomicU64,
    sync::atomic::AtomicUsize, sync::atomic::Ordering,
};

pub const CHUNK_SIZE: usize = 256 * 1024;

const SWEPT: u8 = 0;
const PENDING: u8 = 1;
const SWEEPING: u8 = 2;
/// Max ready chunks probed per allocation.
const READY_PROBE: usize = 8;

const FLAG_YOUNG: u64 = 1 << 0;

#[repr(C)]
pub struct ChunkHeader {
    /// Young chunk flag.
    flags: AtomicU64,
    /// Remembered bits, 8-byte slots.
    remembered: [AtomicU64; REMEMBERED_WORDS],
    /// Mark bits, 16-byte granules.
    mark: [AtomicU64; MARK_WORDS],
    _pad: [u8; HEADER_SIZE - 8 - REMEMBERED_WORDS * 8 - MARK_WORDS * 8],
}

// 512 words cover a full chunk of 8-byte slots.
const REMEMBERED_WORDS: usize = 512;
// 256 words cover a full chunk of 16-byte granules.
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

/// The mapped region chunks are carved from.
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

    pub fn header(&self, addr: usize) -> &ChunkHeader {
        debug_assert!(self.contains(addr), "address outside the reservation");
        // Safety: the reservation is mapped and chunks start with a header.
        unsafe { &*(self.chunk_base(addr) as *const ChunkHeader) }
    }

    pub fn object(&self, addr: usize) -> ObjectAddr {
        debug_assert!(self.contains(addr), "object outside the reservation");
        debug_assert_eq!(addr % ALIGN, 0, "unaligned object address");
        // Safety: inside the reservation and aligned.
        unsafe { ObjectAddr::from_addr(addr) }
    }

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

    pub fn base(&self) -> HeapPtr<u8> {
        self.base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn header(&self) -> &ChunkHeader {
        // Safety: the header is fixed in the reservation.
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
        self.state.load(Ordering::Acquire)
    }
}

const NO_LIVE: usize = usize::MAX;

pub struct ChunkedHeap {
    buffer: MMapBuffer,
    chunk_size: usize,
    table: Mutex<ChunkTable>,
    ready: Mutex<ReadyLists>,
    pending_chunks: AtomicUsize,
    sweep_live: AtomicUsize,
    completed_live: AtomicUsize,
    young_chunks: AtomicUsize,
}

struct ChunkTable {
    slots: Vec<Option<Arc<Chunk>>>,
    pooled: Vec<usize>,
    carved: usize,
}

struct ReadyLists {
    old: VecDeque<Arc<Chunk>>,
    young: VecDeque<Arc<Chunk>>,
}

impl ChunkedHeap {
    pub fn new(reserve: usize) -> Result<Self, AllocError> {
        let buffer = MMapBuffer::new(reserve)?;
        let chunk_size = CHUNK_SIZE.min(buffer.size());
        let slots = buffer.size().div_ceil(chunk_size);
        Ok(Self {
            buffer,
            chunk_size,
            table: Mutex::new(ChunkTable {
                slots: (0..slots).map(|_| None).collect(),
                pooled: Vec::new(),
                carved: 0,
            }),
            ready: Mutex::new(ReadyLists {
                old: VecDeque::new(),
                young: VecDeque::new(),
            }),
            pending_chunks: AtomicUsize::new(0),
            sweep_live: AtomicUsize::new(0),
            completed_live: AtomicUsize::new(NO_LIVE),
            young_chunks: AtomicUsize::new(0),
        })
    }

    pub fn reservation(&self) -> Reservation {
        Reservation {
            base: self.buffer.start().addr(),
            size: self.buffer.size(),
        }
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk_size
    }

    pub fn reserve_size(&self) -> usize {
        self.buffer.size()
    }

    pub fn young_chunk_count(&self) -> usize {
        self.young_chunks.load(Ordering::Relaxed)
    }

    pub fn is_young_addr(&self, addr: usize) -> bool {
        self.reservation().header(addr).young()
    }

    pub fn remembered_slots(&self) -> Vec<CellRef> {
        let reservation = self.reservation();
        let table = self.table.lock().unwrap();
        let mut slots = Vec::new();
        for chunk in table.slots.iter().flatten() {
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
        let table = self.table.lock().unwrap();
        for chunk in table.slots.iter().flatten() {
            chunk.header().clear_remembered();
        }
    }

    pub fn clear_marks(&self) {
        let table = self.table.lock().unwrap();
        for chunk in table.slots.iter().flatten() {
            chunk.header().clear_mark();
        }
    }

    pub fn young_indices(&self) -> Vec<usize> {
        let table = self.table.lock().unwrap();
        table
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().filter(|c| c.is_young()).map(|_| i))
            .collect()
    }

    pub fn remove_chunks(&self, indices: &[usize]) {
        let mut table = self.table.lock().unwrap();
        self.ready.lock().unwrap().young.clear();
        for &index in indices {
            let removed = table.slots[index].take().unwrap();
            debug_assert!(removed.is_young(), "only young chunks are freed wholesale");
            self.young_chunks.fetch_sub(1, Ordering::Relaxed);
            // Keep pages resident; minors reuse these chunks immediately.
            drop(removed);
            table.pooled.push(index);
        }
    }

    pub fn finish_promotion(&self, chunk: &Arc<Chunk>, cursor: usize) {
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
        self.ready.lock().unwrap().push(Arc::clone(chunk));
    }

    pub fn activate_promotion_chunk(&self) -> Option<Arc<Chunk>> {
        let mut table = self.table.lock().unwrap();
        self.activate_chunk(&mut table, false)
    }

    pub fn stats(&self) -> HeapStats {
        let table = self.table.lock().unwrap();
        let mut used = 0;
        let mut free = 0;
        let mut capacity = 0;
        for chunk in table.slots.iter().flatten() {
            let free_list = chunk.free.lock().unwrap();
            used += free_list.live_bytes();
            free += free_list.free_bytes();
            capacity += chunk.object_size();
        }
        HeapStats {
            used,
            capacity,
            free,
        }
    }

    pub fn active_chunks(&self) -> usize {
        let table = self.table.lock().unwrap();
        table.carved - table.pooled.len()
    }

    pub fn available_chunks(&self) -> usize {
        let table = self.table.lock().unwrap();
        table.pooled.len() + (table.slots.len() - table.carved)
    }

    pub fn pending_chunks(&self) -> usize {
        self.pending_chunks.load(Ordering::Relaxed)
    }

    pub fn in_heap(&self, addr: usize) -> bool {
        let table = self.table.lock().unwrap();
        let base = self.buffer.start().addr();
        addr >= base && addr < base + table.carved * self.chunk_size
    }

    pub fn chunk_of(&self, addr: usize) -> Arc<Chunk> {
        let table = self.table.lock().unwrap();
        let base = self.buffer.start().addr();
        let index = (addr - base) / self.chunk_size;
        debug_assert!(index < table.slots.len(), "address outside reservation");
        Arc::clone(
            table.slots[index]
                .as_ref()
                .expect("pointer into pooled or uncarved chunk"),
        )
    }

    /// Allocate any size from a ready chunk, then a fresh one.
    /// `nursery` restricts reuse and pending claims to young chunks.
    pub fn allocate(
        &self,
        layout: Layout,
        host: Option<&GcHost>,
        nursery: bool,
    ) -> (Option<HeapPtr<u8>>, Option<Arc<Chunk>>) {
        if let Some(ptr) = self.ready.lock().unwrap().take_ready(layout, nursery) {
            return (Some(ptr), None);
        }

        let fresh = {
            let mut table = self.table.lock().unwrap();
            self.activate_chunk(&mut table, true)
        };
        if let Some(chunk) = fresh {
            let ptr = {
                let mut free = chunk.free.lock().unwrap();
                let ptr = free.allocate(layout);
                chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
                ptr
            };
            self.ready.lock().unwrap().push(Arc::clone(&chunk));
            if ptr.is_some() {
                return (ptr, None);
            }
        }

        let claimed = if host.is_some() {
            let mut table = self.table.lock().unwrap();
            table.claim_pending(nursery)
        } else {
            None
        };
        (None, claimed)
    }

    pub fn claim_pending(&self, nursery: bool) -> Option<Arc<Chunk>> {
        self.table.lock().unwrap().claim_pending(nursery)
    }

    pub fn any_sweeping(&self) -> bool {
        self.table.lock().unwrap().any_sweeping()
    }

    pub fn sweep_claimed(chunk: &Chunk, host: &GcHost) -> usize {
        let mut free = chunk.free.lock().unwrap();
        free.sweep(chunk.header(), chunk.object_size(), host);
        chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
        chunk.header().clear_mark();
        free.live_bytes()
    }

    pub fn publish_swept(&self, chunk: Arc<Chunk>, live: usize) {
        if live == 0 {
            let mut table = self.table.lock().unwrap();
            let index = chunk.index;
            let removed = table.slots[index].take().unwrap();
            if removed.is_young() {
                self.young_chunks.fetch_sub(1, Ordering::Relaxed);
            }
            self.buffer.decommit(removed.base(), removed.size);
            drop(removed);
            table.pooled.push(index);
        } else {
            chunk.state.store(SWEPT, Ordering::Release);
            self.ready.lock().unwrap().push(chunk);
        }
        self.sweep_live.fetch_add(live, Ordering::SeqCst);
        if self.pending_chunks.fetch_sub(1, Ordering::SeqCst) == 1 {
            let live = self.sweep_live.load(Ordering::SeqCst);
            self.completed_live.store(live, Ordering::Release);
        }
    }

    pub fn take_completed_live(&self) -> Option<usize> {
        match self.completed_live.swap(NO_LIVE, Ordering::AcqRel) {
            NO_LIVE => None,
            live => Some(live),
        }
    }

    fn chunk_region(&self, index: usize) -> (HeapPtr<u8>, usize) {
        let offset = index * self.chunk_size;
        let size = self.chunk_size.min(self.reserve_size() - offset);
        (self.buffer.region(offset, size), size)
    }

    fn activate_chunk(&self, table: &mut ChunkTable, young: bool) -> Option<Arc<Chunk>> {
        let index = match table.pooled.pop() {
            Some(index) => index,
            None if table.carved < table.slots.len() => {
                let index = table.carved;
                table.carved += 1;
                index
            }
            None => return None,
        };
        let (base, size) = self.chunk_region(index);
        let chunk = Arc::new(Chunk::new(index, base, size, young));
        if young {
            self.young_chunks.fetch_add(1, Ordering::Relaxed);
        }
        table.slots[index] = Some(Arc::clone(&chunk));
        Some(chunk)
    }

    pub fn flag_all_pending(&self) {
        let table = self.table.lock().unwrap();
        {
            let mut ready = self.ready.lock().unwrap();
            ready.old.clear();
            ready.young.clear();
        }
        let mut count = 0usize;
        for chunk in table.slots.iter().flatten() {
            debug_assert_ne!(chunk.state(), SWEEPING, "cycle started mid-sweep");
            chunk.state.store(PENDING, Ordering::Release);
            count += 1;
        }
        drop(table);
        self.pending_chunks.store(count, Ordering::Release);
        self.sweep_live.store(0, Ordering::Relaxed);
        self.completed_live
            .store(if count == 0 { 0 } else { NO_LIVE }, Ordering::Release);
    }

    pub fn finish_pending(&self, host: &GcHost) -> bool {
        while let Some(chunk) = self.claim_pending(false) {
            let live = Self::sweep_claimed(&chunk, host);
            self.publish_swept(chunk, live);
        }
        !self.any_sweeping()
    }
}

impl ChunkTable {
    fn claim_pending(&mut self, nursery: bool) -> Option<Arc<Chunk>> {
        for slot in &self.slots {
            let Some(chunk) = slot else { continue };
            if nursery && !chunk.is_young() {
                continue;
            }
            if chunk.state() == PENDING {
                chunk.state.store(SWEEPING, Ordering::Release);
                return Some(Arc::clone(chunk));
            }
        }
        None
    }

    fn any_sweeping(&self) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|chunk| chunk.state() == SWEEPING)
    }
}

impl ReadyLists {
    fn push(&mut self, chunk: Arc<Chunk>) {
        if chunk.free_bytes.load(Ordering::Relaxed) == 0 {
            return;
        }
        if chunk.is_young() {
            self.young.push_back(chunk);
        } else {
            self.old.push_back(chunk);
        }
    }

    fn take_ready(&mut self, layout: Layout, nursery: bool) -> Option<HeapPtr<u8>> {
        if let Some(ptr) = self.take_from(layout, nursery) {
            return Some(ptr);
        }
        if !nursery {
            self.take_from(layout, true)
        } else {
            None
        }
    }

    fn take_from(&mut self, layout: Layout, young: bool) -> Option<HeapPtr<u8>> {
        let need = need_for(layout);
        let mut deferred = Vec::new();
        let mut attempts = READY_PROBE;
        while attempts > 0 {
            attempts -= 1;
            let next = if young {
                self.young.pop_front()
            } else {
                self.old.pop_front()
            };
            let Some(chunk) = next else { break };
            if chunk.state() != SWEPT || chunk.free_bytes.load(Ordering::Relaxed) < need {
                deferred.push(chunk);
                continue;
            }
            let ptr = {
                let mut free = chunk.free.lock().unwrap();
                let ptr = free.allocate(layout);
                chunk.free_bytes.store(free.free_bytes(), Ordering::Relaxed);
                ptr
            };
            if ptr.is_some() {
                self.push(chunk);
                for chunk in deferred {
                    self.push(chunk);
                }
                return ptr;
            }
            deferred.push(chunk);
        }
        for chunk in deferred {
            self.push(chunk);
        }
        None
    }
}
