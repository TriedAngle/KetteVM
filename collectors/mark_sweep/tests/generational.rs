use core::alloc::Layout;
use heap_api::{AllocError, GcHost, HeapPtr, RawCell, STRONG_PTR, TAG_MASK, Tlab, Visitor, Word};
use mark_sweep::{heap::MarkSweepConfig, heap::MarkSweepLocal, heap::MarkSweepState};
use std::sync::Arc;

/// Fixed node size for the fake host.
const NODE_SIZE: usize = 32;

struct Roots {
    slots: Vec<RawCell>,
}

impl Roots {
    fn empty() -> Self {
        Self { slots: Vec::new() }
    }

    fn install(self, state: &MarkSweepState) -> Box<Self> {
        let boxed = Box::new(self);
        state.set_host(GcHost {
            ctx: boxed.as_ref() as *const Roots as *const (),
            visit_roots,
            layout_of,
            visit_object,
        });
        boxed
    }

    fn strong(&mut self, ptr: HeapPtr<u8>) {
        self.slots
            .push(unsafe { RawCell::from_word(ptr.as_ptr() as Word | STRONG_PTR) });
    }
}

fn visit_roots(ctx: *const (), visitor: &mut dyn Visitor) {
    let roots = unsafe { &*(ctx as *const Roots) };
    for slot in &roots.slots {
        visitor.visit(slot);
    }
}

fn layout_of(_addr: HeapPtr<()>) -> Layout {
    Layout::from_size_align(NODE_SIZE, 8).unwrap()
}

/// Traces the first payload word as a tagged link.
fn visit_object(addr: HeapPtr<()>, visitor: &mut dyn Visitor) {
    let cell = unsafe { &*(addr.as_ptr().cast::<Word>().add(1) as *const RawCell) };
    if cell.load() & TAG_MASK == STRONG_PTR {
        visitor.visit(cell);
    }
}

fn backend(
    heap_size: usize,
    roots: Roots,
) -> (Arc<MarkSweepState>, Box<MarkSweepLocal>, Box<Roots>) {
    let state = MarkSweepState::new(MarkSweepConfig { heap_size }).unwrap();
    let roots = roots.install(&state);
    let local = MarkSweepLocal::new(Arc::clone(&state));
    (state, local, roots)
}

/// Allocation fast path: bump the window, refill on a miss.
fn alloc(local: &MarkSweepLocal, tlab: &mut Tlab, size: usize) -> Result<HeapPtr<u8>, AllocError> {
    if let Some(ptr) = tlab.try_alloc(size) {
        return Ok(ptr);
    }
    *tlab = local.allocate_slow(size)?;
    Ok(tlab
        .try_alloc(size)
        .expect("allocate_slow must supply a TLAB of at least min_size"))
}

fn collect(local: &MarkSweepLocal, tlab: &mut Tlab) {
    tlab.invalidate();
    local.collect();
    while local.state().pending_chunks() != 0 {
        std::thread::yield_now();
    }
}

fn collect_minor(local: &MarkSweepLocal, tlab: &mut Tlab) {
    tlab.invalidate();
    local.collect_minor();
    while local.state().pending_chunks() != 0 {
        std::thread::yield_now();
    }
}

/// Allocates a node whose link word is `link`.
fn node(local: &MarkSweepLocal, tlab: &mut Tlab, link: Word) -> HeapPtr<u8> {
    let ptr = alloc(local, tlab, NODE_SIZE.next_multiple_of(Tlab::ALIGN)).unwrap();
    unsafe { *(ptr.as_ptr().cast::<Word>().add(1)) = link };
    ptr
}

#[test]
fn minor_then_full_conserves_arena() {
    let (state, local, mut roots) = backend(8 * 1024 * 1024, Roots::empty());
    let mut tlab = Tlab::empty();
    let keep = node(&local, &mut tlab, 0);
    roots.strong(keep);
    for _ in 0..64 {
        node(&local, &mut tlab, 0);
    }

    collect_minor(&local, &mut tlab);
    collect(&local, &mut tlab);

    assert!(state.minor_cycles() >= 1);
    assert_eq!(
        state.stats().used + state.stats().free,
        state.stats().capacity
    );
    assert_eq!(state.stats().used, NODE_SIZE.next_multiple_of(16));
}

#[test]
fn minors_fire_from_the_allocation_path() {
    let (state, local, _roots) = backend(8 * 1024 * 1024, Roots::empty());
    let mut tlab = Tlab::empty();
    // Pure garbage; many minors so the young chunk budget is crossed.
    for _ in 0..(2 * 1024 * 1024 / NODE_SIZE) {
        node(&local, &mut tlab, 0);
    }
    assert!(state.minor_cycles() > 0);
    assert!(state.active_chunks() < 4);
}

#[test]
fn starved_minor_runs_full_collection_instead() {
    // Rooted survivors fill both chunks; the minor must fall back.
    let (state, local, mut roots) = backend(512 * 1024, Roots::empty());
    let mut tlab = Tlab::empty();
    let fill = 2 * (256 * 1024 - mark_sweep::chunk::HEADER_SIZE);
    for _ in 0..(fill / NODE_SIZE) {
        roots.strong(node(&local, &mut tlab, 0));
    }
    assert_eq!(state.active_chunks(), 2);

    let keepers: Vec<Word> = roots.slots.iter().map(|c| c.load()).collect();

    collect_minor(&local, &mut tlab);

    assert_eq!(state.minor_cycles(), 0);
    assert!(state.cycles() >= 1);
    for (i, word) in keepers.iter().enumerate() {
        assert_eq!(roots.slots[i].load(), *word);
    }
    assert_eq!(state.active_chunks(), 2);
}
