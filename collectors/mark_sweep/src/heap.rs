use crate::{
    block::ALIGN, block::need_for, chunk::Chunk, chunk::ChunkHeader, chunk::ChunkedHeap,
    chunk::Reservation,
};
use core::alloc::Layout;
use heap_api::{
    AllocError, CLEARED, CellRef, GcHost, HeapBackend, HeapPtr, HeapStats, LocalHeap, ObjectAddr,
    RawCell, STRONG_PTR, SharedHeap, TAG_MASK, Tlab, Visitor, WEAK_PTR, Word,
};
use heap_utils::{LocalNode, Safepoint};
use std::{
    sync::Arc, sync::Condvar, sync::Mutex, sync::Weak, sync::atomic::AtomicBool,
    sync::atomic::AtomicUsize, sync::atomic::Ordering, thread::JoinHandle,
};

#[derive(Debug, Clone, Copy)]
pub struct MarkSweepConfig {
    pub heap_size: usize,
}

impl Default for MarkSweepConfig {
    fn default() -> Self {
        Self {
            heap_size: 1024 * 1024 * 1024,
        }
    }
}

const FORWARD_TAG: Word = 0b10;

const MIN_YOUNG_CHUNKS: usize = 8;
const MAX_YOUNG_CHUNKS: usize = 32;

pub struct MarkSweepState {
    reservation: Reservation,
    heap: ChunkedHeap,
    safepoint: Safepoint,
    host: Mutex<Option<GcHost>>,
    mark_job: Mutex<Option<Arc<MarkJob>>>,
    mark_assists: AtomicUsize,
    cycles: AtomicUsize,
    minor_cycles: AtomicUsize,
    young_limit: AtomicUsize,
    gc_threshold: AtomicUsize,
    /// Bytes allocated since the last threshold update.
    gc_allocated: AtomicUsize,
    sweep_signal: Arc<SweepSignal>,
    /// Mark/sweep assistant threads.
    assistants: Mutex<Vec<JoinHandle<()>>>,
}

/// Coordinates the assistant pool; marking has priority.
struct SweepSignal {
    state: Mutex<SweepSignalState>,
    cond: Condvar,
    paused: AtomicBool,
}

struct SweepSignalState {
    epoch: usize,
    shutdown: bool,
    /// Assistants currently sweeping.
    active: usize,
    /// Nested pause count.
    depth: usize,
    /// A mark is running; assistants join it instead of sweeping.
    marking: bool,
}

struct SweepPause<'a>(&'a SweepSignal);

impl Drop for SweepPause<'_> {
    fn drop(&mut self) {
        self.0.resume();
    }
}

impl SweepSignal {
    fn new() -> Self {
        Self {
            state: Mutex::new(SweepSignalState {
                epoch: 0,
                shutdown: false,
                active: 0,
                depth: 0,
                marking: false,
            }),
            cond: Condvar::new(),
            paused: AtomicBool::new(false),
        }
    }

    fn paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Stop assistants at the next chunk boundary.
    fn pause(&self) {
        self.paused.store(true, Ordering::Release);
        let mut signal = self.state.lock().unwrap();
        signal.depth += 1;
        while signal.active > 0 {
            signal = self.cond.wait(signal).unwrap();
        }
    }

    /// Release a pause; the outermost resume wakes the pool.
    fn resume(&self) {
        let mut signal = self.state.lock().unwrap();
        signal.depth -= 1;
        if signal.depth == 0 {
            self.paused.store(false, Ordering::Release);
            signal.epoch += 1;
            self.cond.notify_all();
        }
    }

    fn begin_mark(&self) {
        let mut signal = self.state.lock().unwrap();
        signal.marking = true;
        self.cond.notify_all();
    }

    fn end_mark(&self) {
        let mut signal = self.state.lock().unwrap();
        signal.marking = false;
        self.cond.notify_all();
    }
}

impl MarkSweepState {
    pub fn new(config: MarkSweepConfig) -> Result<Arc<Self>, AllocError> {
        let heap = ChunkedHeap::new(config.heap_size)?;
        let reservation = heap.reservation();
        let this = Arc::new(Self {
            reservation,
            heap,
            safepoint: Safepoint::new(),
            host: Mutex::new(None),
            mark_job: Mutex::new(None),
            mark_assists: AtomicUsize::new(0),
            cycles: AtomicUsize::new(0),
            minor_cycles: AtomicUsize::new(0),
            young_limit: AtomicUsize::new(MIN_YOUNG_CHUNKS),
            gc_threshold: AtomicUsize::new(MIN_GC_THRESHOLD),
            gc_allocated: AtomicUsize::new(0),
            sweep_signal: Arc::new(SweepSignal::new()),
            assistants: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&this);
        this.safepoint.set_work(Arc::new({
            let weak = weak.clone();
            move || {
                let Some(state) = weak.upgrade() else { return };
                let Some(job) = state.mark_job.lock().unwrap().clone() else {
                    return;
                };
                if !job.done.load(Ordering::Acquire) {
                    state.mark_assists.fetch_add(1, Ordering::Relaxed);
                    join_marking(&job, false);
                }
            }
        }));
        let count = ASSIST_THREADS;
        let mut assistants = this.assistants.lock().unwrap();
        for index in 0..count {
            let handle = std::thread::Builder::new()
                .name(format!("mark-sweep-assist-{index}"))
                .spawn({
                    let weak = weak.clone();
                    let signal = Arc::clone(&this.sweep_signal);
                    move || assistant_loop(&weak, &signal)
                });
            if let Ok(handle) = handle {
                assistants.push(handle);
            }
        }
        drop(assistants);
        Ok(this)
    }

    pub fn set_host(&self, host: GcHost) {
        *self.host.lock().unwrap() = Some(host);
    }

    fn header_of(&self, addr: usize) -> &ChunkHeader {
        self.reservation.header(addr)
    }

    pub fn is_young_addr(&self, addr: usize) -> bool {
        debug_assert!(
            self.reservation.contains(addr),
            "young-check on a non-heap address {addr:#x}"
        );
        self.header_of(addr).young()
    }

    pub fn write_barrier(&self, slot: &RawCell, value: Word) {
        let value_addr = (value & !TAG_MASK) as usize;
        debug_assert!(self.reservation.contains(value_addr));
        let slot_addr = slot as *const RawCell as usize;
        let slot_header = self.header_of(slot_addr);
        if !slot_header.young() && self.header_of(value_addr).young() {
            slot_header.remember(slot_addr);
        }
    }

    pub fn cycles(&self) -> usize {
        self.cycles.load(Ordering::Relaxed)
    }

    pub fn mark_assists(&self) -> usize {
        self.mark_assists.load(Ordering::Relaxed)
    }

    pub fn active_chunks(&self) -> usize {
        self.heap.active_chunks()
    }

    pub fn pending_chunks(&self) -> usize {
        self.heap.pending_chunks()
    }

    pub fn contains(&self, addr: usize) -> bool {
        self.heap.in_heap(addr)
    }

    pub fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    pub fn collect_now(&self) {
        self.collect_internal(None);
    }

    pub fn minor_cycles(&self) -> usize {
        self.minor_cycles.load(Ordering::Relaxed)
    }

    pub fn should_minor_gc(&self) -> bool {
        self.heap.young_chunk_count() >= self.young_limit.load(Ordering::Relaxed)
    }

    fn update_young_limit(&self, survivors: usize, chunk_size: usize) {
        let want = survivors
            .saturating_mul(2)
            .div_ceil(chunk_size)
            .clamp(MIN_YOUNG_CHUNKS, MAX_YOUNG_CHUNKS);
        self.young_limit.store(want, Ordering::Relaxed);
    }

    fn full_collect_stw(&self, host: &GcHost) -> Option<usize> {
        while self.heap.any_sweeping() {
            std::thread::yield_now();
        }
        self.mark(&self.heap, host);
        self.heap.flag_all_pending();
        self.heap.take_completed_live()
    }

    fn collect_minor_internal(&self, requester: Option<&MarkSweepLocal>) {
        if self.heap.young_chunk_count() == 0 {
            return;
        }
        let host = self.host();
        // Only a fallback full collection stops the pool.
        let mut full = false;
        self.safepoint
            .stop_the_world(requester.map(|local| &local.node), || {
                full = self.collect_minor_stw(&host, true);
            });
        if full {
            self.kick_assistants(&host, requester);
        }
    }

    /// Evacuate the nursery. The world must be stopped.
    /// `fallback_to_full` runs a full collection when promotion space is short.
    fn collect_minor_stw(&self, host: &GcHost, fallback_to_full: bool) -> bool {
        let young = self.heap.young_indices();
        if young.is_empty() {
            return false;
        }
        let starved = self.heap.available_chunks() < 2 * young.len();
        if starved {
            if fallback_to_full {
                // A full collection re-flags every chunk.
                self.sweep_signal.pause();
                let completed = self.full_collect_stw(host);
                if let Some(live) = completed {
                    self.update_threshold(live);
                }
                self.cycles.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            return false;
        }

        let mut job = MinorJob {
            state: self,
            host: *host,
            promote: Vec::new(),
            worklist: Vec::new(),
            deferred_weak: Vec::new(),
            survivors: 0,
        };
        {
            let mut scanner = MinorScanner { job: &mut job };
            (host.visit_roots)(host.ctx, &mut scanner);
        }
        let remembered = self.heap.remembered_slots();
        for cell in remembered {
            let mut scanner = MinorScanner { job: &mut job };
            // Safety: remembered slots live in collector memory.
            scanner.visit(unsafe { cell.as_ref() });
        }
        job.drain();

        // Weak young targets settle after the strong closure.
        for cell in job.deferred_weak {
            // Safety: deferred weak cells are root slots owned by the heap.
            let cell = unsafe { cell.as_ref() };
            let word = cell.load();
            if word & TAG_MASK == WEAK_PTR && word != CLEARED {
                let addr = (word & !TAG_MASK) as usize;
                if self.is_young_addr(addr) {
                    let target = self.reservation.object(addr);
                    let header = target.header_word();
                    if header & TAG_MASK == FORWARD_TAG {
                        cell.store_raw((header & !TAG_MASK) as Word | WEAK_PTR);
                    } else {
                        cell.store_raw(CLEARED);
                    }
                }
            }
        }

        for (chunk, cursor) in &job.promote {
            self.heap.finish_promotion(chunk, *cursor);
        }
        self.heap.remove_chunks(&young);
        self.heap.clear_remembered_sets();
        let survivors = job.survivors;
        self.update_young_limit(survivors, self.heap.chunk_size());
        self.minor_cycles.fetch_add(1, Ordering::Relaxed);
        false
    }

    fn pause_sweeping(&self) -> SweepPause<'_> {
        self.sweep_signal.pause();
        self.wait_for_chunk_boundary();
        SweepPause(&self.sweep_signal)
    }

    fn collect_internal(&self, requester: Option<&MarkSweepLocal>) {
        let host = self.host();
        // Pause sweeping; the mark re-flags every chunk.
        self.sweep_signal.pause();
        self.wait_for_chunk_boundary();
        self.safepoint
            .stop_the_world(requester.map(|local| &local.node), || {
                // Empty the nursery before marking.
                self.collect_minor_stw(&host, false);
                self.mark(&self.heap, &host);
                self.heap.flag_all_pending();
                let completed = self.heap.take_completed_live();
                if let Some(live) = completed {
                    self.update_threshold(live);
                }
            });
        self.cycles.fetch_add(1, Ordering::Relaxed);
        self.kick_assistants(&host, requester);
    }

    /// Wait for an in-flight chunk sweep.
    fn wait_for_chunk_boundary(&self) {
        while self.heap.any_sweeping() {
            std::thread::yield_now();
        }
    }

    /// Resume the pool, or sweep inline if it failed to spawn.
    fn kick_assistants(&self, host: &GcHost, requester: Option<&MarkSweepLocal>) {
        self.sweep_signal.resume();
        if self.assistants.lock().unwrap().is_empty() {
            self.sweep_to_completion(host, requester);
        }
    }

    /// Publish a swept chunk and update the threshold when the cycle ends.
    fn publish_swept(&self, chunk: Arc<Chunk>, live: usize) {
        self.heap.publish_swept(chunk, live);
        if let Some(live) = self.heap.take_completed_live() {
            self.update_threshold(live);
        }
    }

    /// Inline sweep when no assistant thread exists.
    fn sweep_to_completion(&self, host: &GcHost, requester: Option<&MarkSweepLocal>) {
        loop {
            if let Some(local) = requester {
                local.park_if_requested();
            }
            let chunk = self.heap.claim_pending(false);
            if let Some(chunk) = chunk {
                let live = ChunkedHeap::sweep_claimed(&chunk, host);
                self.publish_swept(chunk, live);
                continue;
            }
            let drained = self.heap.pending_chunks() == 0 && !self.heap.any_sweeping();
            if drained {
                return;
            }
            std::thread::yield_now();
        }
    }

    fn drain_pending(&self) {
        let Some(host) = self.try_host() else { return };
        loop {
            if self.sweep_signal.paused() {
                return;
            }
            let chunk = self.heap.claim_pending(false);
            let Some(chunk) = chunk else { return };
            let live = ChunkedHeap::sweep_claimed(&chunk, &host);
            self.publish_swept(chunk, live);
        }
    }

    fn update_threshold(&self, live: usize) {
        let threshold = (live * 2)
            .max(MIN_GC_THRESHOLD)
            .min(self.heap.reserve_size());
        self.gc_threshold.store(threshold, Ordering::Relaxed);
        self.gc_allocated.store(0, Ordering::Relaxed);
    }

    fn collect_terminal(
        &self,
        requester: Option<&MarkSweepLocal>,
        layout: Layout,
    ) -> TerminalAllocation {
        let host = self.host();
        self.sweep_signal.pause();
        self.wait_for_chunk_boundary();
        let mut executed = false;
        let mut allocated = None;
        self.safepoint
            .stop_the_world(requester.map(|local| &local.node), || {
                executed = true;
                self.wait_for_chunk_boundary();
                self.mark(&self.heap, &host);
                self.heap.flag_all_pending();
                loop {
                    self.heap.finish_pending(&host);
                    if self.heap.pending_chunks() == 0 && !self.heap.any_sweeping() {
                        break;
                    }
                    std::thread::yield_now();
                }
                let completed = self.heap.take_completed_live();
                if let Some(live) = completed {
                    self.update_threshold(live);
                }
                allocated = self.alloc_slow(layout, false);
            });
        self.sweep_signal.resume();
        if executed {
            self.cycles.fetch_add(1, Ordering::Relaxed);
            TerminalAllocation::Done(allocated)
        } else {
            TerminalAllocation::Absorbed
        }
    }

    fn used_exceeds_threshold(&self) -> bool {
        self.gc_allocated.load(Ordering::Relaxed) > self.gc_threshold.load(Ordering::Relaxed)
    }

    /// Allocate any size; may claim and sweep a pending chunk.
    fn alloc_slow(&self, layout: Layout, nursery: bool) -> Option<HeapPtr<u8>> {
        let host = self.try_host();
        loop {
            let claim_host = if self.sweep_signal.paused() {
                None
            } else {
                host.as_ref()
            };
            let (ptr, claimed) = self.heap.allocate(layout, claim_host, nursery);
            let completed = self.heap.take_completed_live();
            if let Some(live) = completed {
                self.update_threshold(live);
            }
            let Some(ptr) = ptr else {
                let Some(chunk) = claimed else { return None };
                let host = host.expect("claimed pending chunk without host");
                let live = ChunkedHeap::sweep_claimed(&chunk, &host);
                self.publish_swept(chunk, live);
                continue;
            };
            self.gc_allocated
                .fetch_add(layout.size(), Ordering::Relaxed);
            return Some(ptr);
        }
    }

    fn try_host(&self) -> Option<GcHost> {
        self.host.lock().unwrap().as_ref().copied()
    }

    fn host(&self) -> GcHost {
        self.host
            .lock()
            .unwrap()
            .as_ref()
            .copied()
            .expect("host not registered before collection")
    }

    fn mark(&self, heap: &ChunkedHeap, host: &GcHost) {
        self.gc_allocated.store(0, Ordering::Relaxed);
        // Old mark bits may belong to an unfinished cycle.
        heap.clear_marks();
        let job = Arc::new(MarkJob::new(heap, *host));
        *self.mark_job.lock().unwrap() = Some(Arc::clone(&job));
        self.safepoint.publish_work();
        {
            let mut scanner = RootScanner { job: &job };
            (host.visit_roots)(host.ctx, &mut scanner);
        }
        job.roots_scanned.store(true, Ordering::Release);
        self.sweep_signal.begin_mark();
        self.safepoint.publish_work();
        join_marking(&job, true);
        self.sweep_signal.end_mark();
        job.settle_weaks();
        *self.mark_job.lock().unwrap() = None;
    }
}

enum AssistantJob {
    Mark,
    Sweep,
}

fn assistant_loop(state: &Weak<MarkSweepState>, signal: &Arc<SweepSignal>) {
    let mut seen_epoch = 0;
    loop {
        let job = {
            let mut sig = signal.state.lock().unwrap();
            loop {
                if sig.shutdown {
                    return;
                }
                if sig.marking {
                    break AssistantJob::Mark;
                }
                if !signal.paused() && sig.epoch != seen_epoch {
                    sig.active += 1;
                    seen_epoch = sig.epoch;
                    break AssistantJob::Sweep;
                }
                sig = signal.cond.wait(sig).unwrap();
            }
        };
        // Drop must be able to join the threads.
        let upgraded = state.upgrade();
        let Some(state) = upgraded else {
            if matches!(job, AssistantJob::Sweep) {
                let mut sig = signal.state.lock().unwrap();
                sig.active -= 1;
                signal.cond.notify_all();
            }
            return;
        };
        match job {
            AssistantJob::Mark => {
                if let Some(job) = state.mark_job.lock().unwrap().clone()
                    && !job.done.load(Ordering::Acquire)
                {
                    join_marking(&job, false);
                }
                let mut sig = signal.state.lock().unwrap();
                while sig.marking && !sig.shutdown {
                    sig = signal.cond.wait(sig).unwrap();
                }
                if sig.shutdown {
                    return;
                }
            }
            AssistantJob::Sweep => {
                state.drain_pending();
                let mut sig = signal.state.lock().unwrap();
                sig.active -= 1;
                signal.cond.notify_all();
            }
        }
    }
}

impl Drop for MarkSweepState {
    fn drop(&mut self) {
        {
            let mut signal = self.sweep_signal.state.lock().unwrap();
            signal.shutdown = true;
            self.sweep_signal.cond.notify_all();
        }
        let current = std::thread::current().id();
        for handle in self.assistants.get_mut().unwrap().drain(..) {
            // An assistant can hold the last Arc; never join the current thread.
            if handle.thread().id() == current {
                continue;
            }
            let _ = handle.join();
        }
    }
}

/// Fixed assistant pool; all join marking and sweep when idle.
const ASSIST_THREADS: usize = 4;

const MARK_BATCH: usize = 32;
const MARK_SPILL: usize = 512;
const ASSIST_SPINS: usize = 256;

struct MarkJob {
    reservation: Reservation,
    host: GcHost,
    worklist: Mutex<Vec<ObjectAddr>>,
    /// Weak cells settled after the closure.
    deferred_weak: Mutex<Vec<CellRef>>,
    roots_scanned: AtomicBool,
    active: AtomicUsize,
    done: AtomicBool,
}

impl MarkJob {
    fn new(heap: &ChunkedHeap, host: GcHost) -> Self {
        Self {
            reservation: heap.reservation(),
            host,
            worklist: Mutex::new(Vec::new()),
            deferred_weak: Mutex::new(Vec::new()),
            roots_scanned: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            done: AtomicBool::new(false),
        }
    }

    fn in_heap(&self, addr: usize) -> bool {
        self.reservation.contains(addr)
    }

    fn header_of(&self, addr: usize) -> &ChunkHeader {
        self.reservation.header(addr)
    }

    fn claim_word(&self, word: Word) -> Option<ObjectAddr> {
        if word & TAG_MASK != STRONG_PTR {
            return None;
        }
        let addr = (word & !TAG_MASK) as usize;
        if !self.in_heap(addr) || !self.header_of(addr).mark_set(addr) {
            return None;
        }
        Some(self.reservation.object(addr))
    }

    fn claim(&self, cell: &RawCell) -> Option<ObjectAddr> {
        self.claim_word(cell.load())
    }

    fn steal(&self, local: &mut Vec<ObjectAddr>) {
        let mut worklist = self.worklist.lock().unwrap();
        let take = MARK_BATCH.min(worklist.len());
        if take > 0 {
            let split = worklist.len() - take;
            local.append(&mut worklist.split_off(split));
        }
    }

    fn spill(&self, local: &mut Vec<ObjectAddr>) {
        let mut worklist = self.worklist.lock().unwrap();
        let split = local.len() / 2;
        worklist.append(&mut local.split_off(split));
    }

    fn settle_weaks(&self) {
        let cells = self
            .deferred_weak
            .lock()
            .unwrap()
            .drain(..)
            .collect::<Vec<_>>();
        for cell in cells {
            // Safety: deferred weak cells are root slots owned by the heap.
            let cell = unsafe { cell.as_ref() };
            let word = cell.load();
            if word & TAG_MASK == WEAK_PTR && word != CLEARED {
                let addr = (word & !TAG_MASK) as usize;
                let dead = !self.in_heap(addr) || !self.header_of(addr).mark_is_set(addr);
                if dead {
                    cell.store_raw(CLEARED);
                }
            }
        }
    }
}

struct RootScanner<'a> {
    job: &'a MarkJob,
}

impl Visitor for RootScanner<'_> {
    fn visit(&mut self, cell: &RawCell) {
        let word = cell.load();
        let tag = word & TAG_MASK;
        if tag == STRONG_PTR {
            if let Some(addr) = self.job.claim(cell) {
                self.job.worklist.lock().unwrap().push(addr);
            }
        } else if tag == WEAK_PTR && word != CLEARED {
            self.job
                .deferred_weak
                .lock()
                .unwrap()
                .push(CellRef::from_ref(cell));
        }
    }
}

struct Marker<'a> {
    job: &'a MarkJob,
    local: Vec<ObjectAddr>,
}

impl Visitor for Marker<'_> {
    fn visit(&mut self, cell: &RawCell) {
        let word = cell.load();
        let tag = word & TAG_MASK;
        if tag == STRONG_PTR {
            if let Some(addr) = self.job.claim_word(word) {
                self.local.push(addr);
                if self.local.len() > MARK_SPILL {
                    self.job.spill(&mut self.local);
                }
            }
        } else if tag == WEAK_PTR && word != CLEARED {
            self.job
                .deferred_weak
                .lock()
                .unwrap()
                .push(CellRef::from_ref(cell));
        }
    }
}

fn join_marking(job: &MarkJob, persistent: bool) {
    let mut marker = Marker {
        job,
        local: Vec::new(),
    };
    let mut spins = 0;
    job.active.fetch_add(1, Ordering::AcqRel);
    loop {
        if job.done.load(Ordering::Acquire) {
            break;
        }
        if marker.local.is_empty() {
            job.steal(&mut marker.local);
        }
        if marker.local.is_empty() {
            if job.roots_scanned.load(Ordering::Acquire)
                && job.active.load(Ordering::Acquire) == 1
                && job.worklist.lock().unwrap().is_empty()
            {
                job.done.store(true, Ordering::Release);
                break;
            }
            if !persistent && spins >= ASSIST_SPINS {
                break;
            }
            spins += 1;
            std::thread::yield_now();
            continue;
        }
        spins = 0;
        let addr = marker.local.pop().unwrap();
        (job.host.visit_object)(addr, &mut marker);
    }
    job.active.fetch_sub(1, Ordering::Release);
}

struct MinorJob<'a> {
    state: &'a MarkSweepState,
    host: GcHost,
    promote: Vec<(Arc<Chunk>, usize)>,
    worklist: Vec<ObjectAddr>,
    deferred_weak: Vec<CellRef>,
    survivors: usize,
}

impl MinorJob<'_> {
    fn is_young(&self, addr: usize) -> bool {
        self.state.is_young_addr(addr)
    }

    fn evacuate(&mut self, addr: ObjectAddr) -> ObjectAddr {
        let header = addr.header_word();
        if header & TAG_MASK == FORWARD_TAG {
            return self.state.reservation.object((header & !TAG_MASK) as usize);
        }
        let layout = (self.host.layout_of)(addr);
        let size = need_for(layout);
        let new = self.promote_alloc(size);
        // Safety: `new` is a fresh promotion block of at least `size` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(
                addr.as_non_null().as_ptr().cast::<u8>(),
                new.as_non_null().as_ptr().cast::<u8>(),
                layout.size(),
            );
            addr.set_header_word(new.addr() as Word | FORWARD_TAG);
        }
        self.survivors += size;
        self.worklist.push(new);
        new
    }

    fn promote_alloc(&mut self, size: usize) -> ObjectAddr {
        if let Some((chunk, cursor)) = self.promote.last_mut() {
            let next = *cursor + size;
            if next <= chunk.object_size() {
                // Safety: the cursor stays inside the chunk's object region.
                let at = unsafe { chunk.object_base_ptr().offset(*cursor) };
                *cursor = next;
                return at.cast();
            }
        }
        let chunk = self
            .state
            .heap
            .activate_promotion_chunk()
            .expect("promotion chunks guaranteed by the pre-flight gate");
        let at = chunk.object_base_ptr();
        self.promote.push((chunk, size));
        at.cast()
    }

    fn drain(&mut self) {
        let visit_object = self.host.visit_object;
        while let Some(addr) = self.worklist.pop() {
            let mut scanner = MinorScanner { job: self };
            visit_object(addr, &mut scanner);
        }
    }
}

struct MinorScanner<'a, 'b> {
    job: &'a mut MinorJob<'b>,
}

impl Visitor for MinorScanner<'_, '_> {
    fn visit(&mut self, cell: &RawCell) {
        let word = cell.load();
        let tag = word & TAG_MASK;
        if tag == STRONG_PTR {
            let addr = (word & !TAG_MASK) as usize;
            if self.job.is_young(addr) {
                let addr = self.job.state.reservation.object(addr);
                cell.store_raw(self.job.evacuate(addr).addr() as Word | STRONG_PTR);
            }
        } else if tag == WEAK_PTR && word != CLEARED {
            let addr = (word & !TAG_MASK) as usize;
            if self.job.is_young(addr) {
                self.job.deferred_weak.push(CellRef::from_ref(cell));
            }
        }
    }
}

/// Per-thread local heap.
pub struct MarkSweepLocal {
    state: Arc<MarkSweepState>,
    node: LocalNode,
}

const TLAB_SIZES: [usize; 2] = [32 * 1024, 8 * 1024];
const MIN_GC_THRESHOLD: usize = 16 * 1024 * 1024;
const TRANSIENT_ATTEMPTS: usize = 2;

enum TerminalAllocation {
    Absorbed,
    Done(Option<HeapPtr<u8>>),
}

impl MarkSweepLocal {
    pub fn new(state: Arc<MarkSweepState>) -> Box<Self> {
        let local = Box::new(Self {
            state,
            node: LocalNode::detached(),
        });
        local.state.safepoint.attach(&local.node);
        local
    }

    pub fn state(&self) -> &MarkSweepState {
        &self.state
    }

    pub fn collect(&self) {
        self.state.collect_internal(Some(self));
    }

    pub fn collect_minor(&self) {
        self.state.collect_minor_internal(Some(self));
    }

    /// Slow allocation; returns a TLAB of at least `min_size`.
    pub fn allocate_slow(&self, min_size: usize) -> Result<Tlab, AllocError> {
        self.park_if_requested();
        let layout = Layout::from_size_align(min_size, ALIGN).expect("allocation layout");
        if self.state.should_minor_gc() {
            self.collect_minor();
        }
        if self.state.used_exceeds_threshold() {
            self.collect();
        }
        if let Some(tlab) = self.alloc_block(min_size) {
            return Ok(tlab);
        }
        for _ in 0..TRANSIENT_ATTEMPTS {
            if let Some(ptr) = self.state.alloc_slow(layout, false) {
                return Ok(Tlab::new(ptr, min_size));
            }
            if self.state.should_minor_gc() {
                self.collect_minor();
            }
            self.collect();
            if let Some(tlab) = self.alloc_block(min_size) {
                return Ok(tlab);
            }
        }
        loop {
            if let Some(ptr) = self.state.alloc_slow(layout, false) {
                return Ok(Tlab::new(ptr, min_size));
            }
            match self.state.collect_terminal(Some(self), layout) {
                TerminalAllocation::Done(Some(ptr)) => {
                    return Ok(Tlab::new(ptr, min_size));
                }
                TerminalAllocation::Done(None) => {
                    return Err(AllocError::OutOfMemory(layout));
                }
                TerminalAllocation::Absorbed => continue,
            }
        }
    }

    fn park_if_requested(&self) -> bool {
        if self.state.safepoint.requested() || self.node.cancelled() {
            return self.state.safepoint.park_for_collection(&self.node);
        }
        false
    }

    /// Allocate a TLAB block; tries the configured sizes.
    fn alloc_block(&self, need: usize) -> Option<Tlab> {
        if need > *TLAB_SIZES.last().unwrap() {
            return None;
        }
        for &size in &TLAB_SIZES {
            let layout = Layout::from_size_align(size, ALIGN).unwrap();
            if let Some(block) = self.state.alloc_slow(layout, true) {
                return Some(Tlab::new(block, size));
            }
        }
        None
    }
}

impl Drop for MarkSweepLocal {
    fn drop(&mut self) {
        self.state.safepoint.detach(&self.node);
    }
}

pub struct MarkSweep {
    inner: Arc<MarkSweepState>,
}

impl MarkSweep {
    pub fn new(config: MarkSweepConfig) -> Result<Self, AllocError> {
        Ok(Self {
            inner: MarkSweepState::new(config)?,
        })
    }

    pub fn state(&self) -> &MarkSweepState {
        &self.inner
    }
}

impl LocalHeap for MarkSweepLocal {
    fn allocate_slow(&self, min_size: usize) -> Result<Tlab, AllocError> {
        self.allocate_slow(min_size)
    }

    fn write_barrier(&self, _host: Word, slot: &RawCell, value: Word) {
        self.state.write_barrier(slot, value);
    }

    fn collection_requested(&self) -> bool {
        self.state.safepoint.requested() || self.node.cancelled()
    }

    fn park_for_collection(&self) -> bool {
        self.park_if_requested()
    }

    fn take_cancel(&self) -> bool {
        self.node.take_cancelled()
    }

    fn block(&self) -> bool {
        self.state.safepoint.block(&self.node)
    }

    fn unblock(&self) {
        self.state.safepoint.unblock(&self.node);
    }

    fn cancel_executions(&self, protocol: &dyn Fn()) {
        self.state
            .safepoint
            .cancel_executions(&self.node, || protocol());
    }

    fn force_collect(&self) {
        self.collect();
    }

    fn collect_minor(&self) {
        self.collect_minor();
    }

    fn exclusive(&self, work: &mut dyn FnMut()) {
        let _pause = self.state.pause_sweeping();
        self.state
            .safepoint
            .stop_the_world_exclusive(Some(&self.node), || work());
    }

    fn gc_in_progress(&self) -> bool {
        self.state.safepoint.is_armed()
    }
}

impl SharedHeap for MarkSweep {
    fn new_local(&self) -> Box<dyn LocalHeap> {
        MarkSweepLocal::new(Arc::clone(&self.inner))
    }

    fn set_host(&self, host: GcHost) {
        self.inner.set_host(host);
    }

    fn iterate_roots(&self, _roots: &mut dyn Visitor) {}

    fn should_collect(&self) -> bool {
        false
    }

    fn gc_in_progress(&self) -> bool {
        self.inner.safepoint.is_armed()
    }

    fn force_collect(&self) {
        self.inner.collect_now();
    }

    fn contains(&self, addr: Word) -> bool {
        self.inner.contains(addr as usize)
    }

    fn is_young(&self, value: Word) -> bool {
        if value & TAG_MASK != STRONG_PTR {
            return false;
        }
        let addr = (value & !TAG_MASK) as usize;
        self.inner.reservation.contains(addr) && self.inner.header_of(addr).young()
    }

    fn stats(&self) -> HeapStats {
        self.inner.stats()
    }
}

impl HeapBackend for MarkSweep {
    type Config = MarkSweepConfig;

    fn new(config: Self::Config) -> Result<Self, AllocError> {
        MarkSweep::new(config)
    }

    fn into_shared(self) -> Arc<dyn SharedHeap> {
        Arc::new(self)
    }
}
