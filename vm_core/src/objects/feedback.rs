use crate::{
    AtomicMaybeWeakGcSlot, EdgeVisitable, GcSlot, Header, Heap, HeapObject, Map, MaybeWeak,
    ObjectKind, Smi, Tagged, Value, Visitor, WeakFixedArray,
};
use core::{alloc::Layout, sync::atomic::Ordering};
use structures::seqlock::SeqLock;

/// Shared feedback storage. Each site is a `[state, handler]` pair:
/// hole (unset), weak map + handler (mono), strong `WeakFixedArray`
/// (poly), or strong megamorphic sentinel.
///
/// Slots are atomic; pair writes use a per-vector sequence lock.
#[repr(C)]
pub struct FeedbackVector {
    pub header: Header,
    pub length: GcSlot<Smi>,
    seq: SeqLock,
    pub slots: [AtomicMaybeWeakGcSlot; 0],
}

pub struct FeedbackVectorInit {
    pub length: usize,
}

/// A coherent snapshot of one site's `[state, handler]` pair.
#[derive(Clone, Copy)]
pub struct SiteRead<'a> {
    pub state: Tagged<'a, MaybeWeak<Value>>,
    pub handler: Tagged<'a, MaybeWeak<Value>>,
}

impl FeedbackVector {
    pub fn layout_for(len: usize) -> Layout {
        let slots_layout =
            Layout::array::<AtomicMaybeWeakGcSlot>(len).expect("feedback slots layout");
        Layout::new::<Self>()
            .extend(slots_layout)
            .expect("feedback vector layout")
            .0
    }

    #[allow(clippy::len_without_is_empty)]
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.length.to_smi_unchecked().value() as usize
    }

    #[inline(always)]
    fn slot(&self, i: usize) -> &AtomicMaybeWeakGcSlot {
        debug_assert!(i < self.len());
        unsafe { &*self.slots.as_ptr().add(i) }
    }

    /// Returns the current state word.
    #[inline(always)]
    pub fn read_site<'a>(&self, _heap: &'a Heap, slot: usize) -> Tagged<'a, MaybeWeak<Value>> {
        debug_assert!(slot < self.len());
        let word = self.slot(slot).load_word(Ordering::Acquire);
        // Safety: a state slot holds a Smi, the hole, or a maybe-weak word.
        unsafe { Tagged::from_maybe_weak_unchecked(Value::from_bits(word)) }
    }

    /// Returns a coherent `[state, handler]` snapshot.
    #[inline(always)]
    pub fn read_pair<'a>(&self, _heap: &'a Heap, slot: usize) -> SiteRead<'a> {
        debug_assert!(slot + 1 < self.len());
        let (state, handler) = self.seq.read(|| {
            (
                self.slot(slot).load_word(Ordering::Relaxed),
                self.slot(slot + 1).load_word(Ordering::Relaxed),
            )
        });
        SiteRead {
            // Safety: feedback slots hold Smis, the hole, or maybe-weak words.
            state: unsafe { Tagged::from_maybe_weak_unchecked(Value::from_bits(state)) },
            handler: unsafe { Tagged::from_maybe_weak_unchecked(Value::from_bits(handler)) },
        }
    }

    /// Runs `f` with the writer lock held. `f` must not allocate, park, or
    /// panic.
    #[inline]
    pub fn write_site<T>(
        &self,
        heap: &Heap,
        slot: usize,
        f: impl FnOnce(&Heap, &AtomicMaybeWeakGcSlot, &AtomicMaybeWeakGcSlot) -> T,
    ) -> T {
        debug_assert!(slot + 1 < self.len());
        self.seq
            .write(|| f(heap, self.slot(slot), self.slot(slot + 1)))
    }

    /// Writes a monomorphic `[weak map, handler]` pair.
    pub fn set_mono(
        &self,
        heap: &Heap,
        slot: usize,
        map: Tagged<'_, Map>,
        handler: Tagged<'_, MaybeWeak<Value>>,
    ) {
        let host = self.tagged(heap);
        self.write_site(heap, slot, |heap, state, handler_slot| {
            state.set_release(heap, host, map.erase().as_weak());
            handler_slot.set_release(heap, host, handler);
        });
    }

    /// Replaces the handler if the state still refers to `map`.
    ///
    /// Example: a prototype-chain change invalidated the old handler.
    pub fn set_handler_if_mono(
        &self,
        heap: &Heap,
        slot: usize,
        map: Tagged<'_, Map>,
        handler: Tagged<'_, MaybeWeak<Value>>,
    ) -> bool {
        let host = self.tagged(heap);
        self.write_site(heap, slot, |heap, state, handler_slot| {
            if !state.get_acquire(heap).is_weak_ref_to(map.erase()) {
                return false;
            }
            handler_slot.set_release(heap, host, handler);
            true
        })
    }

    /// Publishes an immutable polymorphic pair array.
    pub fn set_poly(&self, heap: &Heap, slot: usize, pairs: Tagged<'_, WeakFixedArray>) {
        debug_assert!(slot < self.len());
        let host = self.tagged(heap);
        self.slot(slot).set_release(heap, host, pairs.erase());
    }

    /// Publishes the megamorphic sentinel.
    pub fn set_megamorphic(&self, heap: &Heap, slot: usize) {
        debug_assert!(slot < self.len());
        let symbol = heap.known().megamorphic_symbol.get(heap);
        let host = self.tagged(heap);
        self.slot(slot).set_release(heap, host, symbol.erase());
    }
}

impl HeapObject for FeedbackVector {
    const KIND: ObjectKind = ObjectKind::FeedbackVector;
    type Init<'a> = FeedbackVectorInit;

    fn layout_for(config: &Self::Init<'_>) -> Layout {
        Self::layout_for(config.length)
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.seq.reset();
        self.header
            .map
            .init(heap.known().feedback_vector_map.get(heap));
        self.length.set(heap, host, Smi::new(config.length as i64));
        let fill = heap.known().the_hole.get(heap).erase();
        for i in 0..config.length {
            self.slot(i).set(heap, host, fill);
        }
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for(self.len())
    }
}

impl EdgeVisitable for FeedbackVector {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        for i in 0..self.len() {
            visitor.visit(self.slot(i).as_raw());
        }
    }
}
