use crate::{
    EdgeVisitable, GcSlot, HANDLE_BLOCK_SIZE, Heap, HeapObject, Register, Tagged, Value, Visitor,
    stack::Args,
};
use core::{cell::Cell, cell::UnsafeCell, marker::PhantomData, ptr::NonNull};

/// A rooted reference to a `T` that survives relocation by the GC.
/// Weak references cannot be rooted: they live in `MaybeWeakGcSlot`s as
/// `Tagged<MaybeWeak<T>>` words.
///
/// The handle itself is only a location; reading it back as a
/// [`Tagged`] requires a live borrow of the heap
/// ([`Handle::as_tagged`]) so the snapshot cannot outlive the next GC.
pub struct Handle<'scope, T> {
    location: NonNull<Value>,
    _phantom: PhantomData<(&'scope (), T)>,
}

impl<'s, T> Clone for Handle<'s, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'s, T> Copy for Handle<'s, T> {}

impl<'s, T> core::fmt::Debug for Handle<'s, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Safety: raw word read for diagnostics only.
        let value = unsafe { *self.location.as_ptr() };
        f.debug_struct("Handle").field("value", &value).finish()
    }
}

impl<'s, T> Handle<'s, T> {
    /// # Safety
    /// `location` must point at a live GC-visited slot that holds a strong value
    pub unsafe fn from_location(location: NonNull<Value>) -> Self {
        Handle {
            location,
            _phantom: PhantomData,
        }
    }

    pub fn as_tagged<'a>(self, _heap: &'a Heap) -> Tagged<'a, T> {
        // Safety: handle slots only ever hold strong values, and the
        // anchor borrow proves no GC ran since the load.
        unsafe { Tagged::from_value_unchecked(*self.location.as_ptr()) }
    }

    pub fn raw(self) -> Value {
        unsafe { *self.location.as_ptr() }
    }

    pub fn erase(self) -> Handle<'s, Value> {
        Handle {
            location: self.location,
            _phantom: PhantomData,
        }
    }
}

pub struct HandleData {
    inner: UnsafeCell<HandleDataImpl>,
}

/// Slots handed out while this block was the active one; root scanning
/// never visits past this watermark.
struct HandleBlock {
    slots: Box<[GcSlot]>,
    used: usize,
}

struct HandleDataImpl {
    // Note: the memory MUST stay stable, this is why its necessary to use additional blocks
    // instead of a single Vec<T>, because we can add/remove blocks without moving any blocks.
    blocks: Vec<HandleBlock>,
    next: *mut Value,
    limit: *mut Value,
    fill: Register,
}

impl HandleDataImpl {
    #[inline]
    fn allocate_slot(&mut self) -> *mut Value {
        if self.next == self.limit {
            self.extend();
        }
        let slot = self.next;
        self.next = unsafe { slot.add(1) };
        slot
    }

    fn extend(&mut self) {
        self.extend_sized(HANDLE_BLOCK_SIZE);
    }

    fn extend_sized(&mut self, size: usize) {
        let live = self.live_in_active();
        if let Some(block) = self.blocks.last_mut() {
            block.used = live;
        }
        let block = (0..size)
            .map(|_| {
                // Safety: the fill word is a strong, GC-valid root value.
                unsafe { GcSlot::from_value(self.fill.raw()) }
            })
            .collect::<Box<[GcSlot]>>();
        self.next = block.as_ptr() as *mut Value;
        self.limit = unsafe { self.next.add(block.len()) };
        self.blocks.push(HandleBlock {
            slots: block,
            used: 0,
        });
    }

    fn live_in_active(&self) -> usize {
        let Some(block) = self.blocks.last() else {
            return 0;
        };
        let start = block.slots.as_ptr() as *mut Value;
        debug_assert!(start <= self.next && self.next <= self.limit);
        (self.next as usize - start as usize) / core::mem::size_of::<Value>()
    }

    /// Reserve `n` contiguous slots in the current block, extending first
    /// when the remainder is too small. Oversized requests get a dedicated
    /// block (block memory is boxed and never moves, so any size works).
    #[inline]
    fn allocate_block(&mut self, n: usize) -> *mut Value {
        if n > HANDLE_BLOCK_SIZE {
            self.extend_sized(n);
        } else if self.next.addr() + n * core::mem::size_of::<Value>() > self.limit.addr() {
            self.extend();
        }
        let start = self.next;
        self.next = unsafe { start.add(n) };
        start
    }

    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.fill.as_raw());
        let Some((active, full)) = self.blocks.split_last() else {
            return;
        };
        for block in full {
            for slot in &block.slots[..block.used] {
                visitor.visit(slot.as_raw());
            }
        }
        for slot in &active.slots[..self.live_in_active()] {
            visitor.visit(slot.as_raw());
        }
    }
}

impl HandleData {
    pub fn new(fill: Tagged<'_, Value>) -> Self {
        let mut inner = HandleDataImpl {
            blocks: Vec::new(),
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            fill: unsafe { Register::from_value(fill) },
        };
        inner.extend();
        Self {
            inner: UnsafeCell::new(inner),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn inner(&self) -> &mut HandleDataImpl {
        unsafe { &mut *self.inner.get() }
    }
}

impl EdgeVisitable for HandleData {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.inner().visit_edges(visitor)
    }
}

pub struct HandleScope<'d> {
    data: NonNull<HandleData>,
    prev_next: *mut Value,
    prev_limit: *mut Value,
    prev_block_count: usize,
    _phantom: PhantomData<fn(&'d ()) -> &'d ()>,
}

impl<'d> HandleScope<'d> {
    #[inline(always)]
    pub unsafe fn from_raw(data: NonNull<HandleData>) -> Self {
        let inner = unsafe { &*data.as_ptr() }.inner();
        Self {
            data,
            prev_next: inner.next,
            prev_limit: inner.limit,
            prev_block_count: inner.blocks.len(),
            _phantom: PhantomData,
        }
    }

    #[inline(always)]
    pub fn handle<'x, T: 'x>(&self, value: impl Into<Tagged<'x, T>>) -> Handle<'_, T> {
        let value = value.into();
        debug_assert!(
            !value.raw().is_weak_ptr(),
            "weak value cannot be rooted in a handle"
        );
        let slot = unsafe { &*self.data.as_ptr() }.inner().allocate_slot();
        unsafe { *slot = value.raw() };
        unsafe { Handle::from_location(NonNull::new_unchecked(slot)) }
    }

    /// Root a copy of `values` in contiguous scope slots. The values may be
    /// anchored anywhere: rooting only writes words.
    #[inline(always)]
    pub fn stage<'x, T>(&self, values: &[Tagged<'x, T>]) -> HandleSlice<'_> {
        // SAFETY: a `Tagged` word is a strong, GC-valid reference.
        unsafe { self.stage_words(values.len(), |i| values[i].raw()) }
    }

    /// Root copies of `handles` in contiguous scope slots: their own slots
    /// may be scattered or belong to an enclosing scope.
    #[inline(always)]
    pub fn stage_handles<T>(&self, handles: &[Handle<'_, T>]) -> HandleSlice<'_> {
        // SAFETY: handle slots only ever hold strong values.
        unsafe { self.stage_words(handles.len(), |i| handles[i].raw()) }
    }

    /// # Safety
    /// Every word `word` yields must be strong (non-weak) and GC-valid: the
    /// slots become roots the collector traces.
    #[inline(always)]
    unsafe fn stage_words(&self, len: usize, word: impl Fn(usize) -> Value) -> HandleSlice<'_> {
        let inner = unsafe { &*self.data.as_ptr() }.inner();
        let start = inner.allocate_block(len);
        for i in 0..len {
            let word = word(i);
            debug_assert!(!word.is_weak_ptr(), "weak value staged for a call");
            unsafe { *start.add(i) = word };
        }
        unsafe { HandleSlice::from_slice(core::slice::from_raw_parts(start, len)) }
    }

    #[inline(always)]
    pub fn cast<T: HeapObject>(
        &self,
        heap: &Heap,
        value: Tagged<'_, Value>,
    ) -> Option<Handle<'_, T>> {
        Some(self.handle(value.get_as::<T>(heap)?))
    }

    #[inline(always)]
    pub fn escapable_scope<'a>(&'a mut self) -> EscapableHandleScope<'a, 'd> {
        let data = unsafe { &*self.data.as_ptr() };
        let escape_slot = data.inner().allocate_slot();
        let inner = data.inner();
        EscapableHandleScope {
            parent: self,
            escape_slot,
            prev_next: inner.next,
            prev_limit: inner.limit,
            prev_block_count: inner.blocks.len(),
            escaped: Cell::new(false),
        }
    }
}

impl Drop for HandleScope<'_> {
    fn drop(&mut self) {
        let inner = unsafe { &*self.data.as_ptr() }.inner();
        inner.next = self.prev_next;
        inner.limit = self.prev_limit;
        inner.blocks.truncate(self.prev_block_count);
    }
}

/// An inner `HandleScope` that can move one handle into its parent scope.
pub struct EscapableHandleScope<'i, 'o> {
    parent: &'i mut HandleScope<'o>,
    escape_slot: *mut Value,
    prev_next: *mut Value,
    prev_limit: *mut Value,
    prev_block_count: usize,
    escaped: Cell<bool>,
}
impl<'d> core::ops::Deref for EscapableHandleScope<'_, 'd> {
    type Target = HandleScope<'d>;

    fn deref(&self) -> &Self::Target {
        self.parent
    }
}

impl<'i, 'o> EscapableHandleScope<'i, 'o> {
    fn data(&self) -> &HandleData {
        unsafe { &*self.parent.data.as_ptr() }
    }

    pub fn escape<T>(&self, handle: Handle<'i, T>) -> Handle<'o, T> {
        debug_assert!(!self.escaped.get(), "only one handle can escape a scope");
        self.escaped.set(true);
        // Safety: moving one rooted word into another rooted slot.
        unsafe { *self.escape_slot = handle.raw() };
        unsafe { Handle::from_location(NonNull::new_unchecked(self.escape_slot)) }
    }
}

impl Drop for EscapableHandleScope<'_, '_> {
    fn drop(&mut self) {
        let inner = self.data().inner();
        inner.next = self.prev_next;
        inner.limit = self.prev_limit;
        inner.blocks.truncate(self.prev_block_count);
    }
}

/// A borrowed view of rooted argument memory: a slice of GC-visited
/// slots (handle scope blocks or the register file). The words are
/// updated in place by the GC, so a [`Handle`] can be taken to any
/// element for free — the slot is already a root, no scope allocation is
/// needed. Anchored reads go through [`Handle::as_tagged`].
#[derive(Copy, Clone)]
pub struct HandleSlice<'a> {
    raw: &'a [Value],
}

impl<'a> HandleSlice<'a> {
    /// The empty argument list.
    pub const EMPTY: HandleSlice<'static> = HandleSlice { raw: &[] };

    pub fn as_tagged(&self) -> &[Tagged<'_, Value>] {
        // Safety: Tagged<Value> is layout-identical to Value.
        unsafe { core::slice::from_raw_parts(self.raw.as_ptr().cast(), self.raw.len()) }
    }

    #[inline]
    pub fn as_args(&self) -> Args {
        if self.raw.is_empty() {
            return Args::EMPTY;
        }
        // Safety: the slice covers rooted handle-scope slots.
        unsafe { Args::from_raw(core::ptr::NonNull::from(&self.raw[0]), self.raw.len()) }
    }

    /// The same rooted slots minus the first `n` elements.
    pub fn slice_from(&self, n: usize) -> Self {
        Self {
            raw: &self.raw[n.min(self.raw.len())..],
        }
    }

    /// # Safety
    /// The slice must point at memory the GC visits for as long as it is
    /// alive ([`HandleScope::stage`] slots or the register file), and the
    /// words must be strong (non-weak) values.
    pub unsafe fn from_slice(raw: &'a [Value]) -> Self {
        Self { raw }
    }

    pub fn get(&self, index: usize) -> Option<Handle<'a, Value>> {
        let raw: &'a [Value] = self.raw;
        let word = raw.get(index)?;
        // Safety: `from_slice` guarantees a GC-visited, strong slot.
        Some(unsafe { Handle::from_location(NonNull::from(word)) })
    }

    /// Free handles over every element (see [`Self::get`]).
    pub fn iter(&self) -> impl Iterator<Item = Handle<'a, Value>> + 'a {
        let raw: &'a [Value] = self.raw;
        raw.iter()
            .map(|word| unsafe { Handle::from_location(NonNull::from(word)) })
    }

    /// Raw words, for storage copies into fresh objects (no GC can run
    /// mid-`init`).
    pub fn raw(&self) -> &'a [Value] {
        self.raw
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}
