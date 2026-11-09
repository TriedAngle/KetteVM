use crate::proxy::Proxy;
use crate::{
    AccessorInfo, AccessorPair, DETAILS_ACCESSOR, DETAILS_CONFIGURABLE, DETAILS_ENUMERABLE,
    DETAILS_WRITABLE, DenseString, FixedArray, Handle, HandleScope, Heap, Key, Object, SlotFlags,
    SlotName, StringOwn, Tagged, Value,
};

/// One resolved position of the walk: where the property lives and
/// what it holds. `Miss` is terminal (chain exhausted).
pub enum Hit<'h> {
    /// Chain (or own lookup) exhausted.
    Miss,
    /// Named data property in a map descriptor.
    Data {
        holder: Tagged<'h, Object>,
        index: usize,
        offset: usize,
        value: Tagged<'h, Value>,
        flags: SlotFlags,
    },
    Accessor {
        holder: Tagged<'h, Object>,
        index: usize,
        pair: Tagged<'h, AccessorPair>,
        flags: SlotFlags,
    },
    NativeAccessor {
        holder: Tagged<'h, Object>,
        index: usize,
        info: Tagged<'h, AccessorInfo>,
        flags: SlotFlags,
    },
    /// Indexed data element: dense backing or dictionary entry.
    Element {
        holder: Tagged<'h, Object>,
        index: usize,
        value: Tagged<'h, Value>,
        flags: SlotFlags,
    },
    /// Primitive-string own property: `length` or one char.
    StringOwn {
        holder: Tagged<'h, Value>,
        own: StringOwn,
    },
    /// Self-style parent-pair slot on the holder's own map.
    Parent {
        holder: Tagged<'h, Object>,
        slot: usize,
        value: Tagged<'h, Value>,
    },
    /// Holder is a proxy; the driver must run the trap.
    Proxy(Tagged<'h, Value>),
}

/// Parent-list descent path; cycle checks are linear scans of it.
#[derive(Clone, Copy)]
enum Link<'s> {
    /// Entered parent list; `next` is the next parent slot (odd index).
    List {
        pairs: Handle<'s, FixedArray>,
        next: usize,
    },
    /// Object descended into from a list: return point and cycle-guard
    /// membership.
    Parent { value: Handle<'s, Value> },
}

/// Property-lookup iterator: [`next`] yields the next resolved
/// position of the walk, or `Miss` when the chain is exhausted.
///
/// [`next`]: Lookup::next
pub struct Lookup<'s> {
    key: Handle<'s, Value>,
    /// `Some(i)` when the key classified as an element index.
    index: Option<usize>,
    /// Whether to stop after the start object's own properties.
    own_only: bool,
    /// Current position; `None` while picking from a parent list.
    holder: Option<Handle<'s, Value>>,
    /// Descent path through parent lists ([`Link`]).
    chain: Vec<Link<'s>>,
}

impl<'s> Lookup<'s> {
    pub fn new(
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        start: Handle<'_, Value>,
        key: Handle<'_, Value>,
        own_only: bool,
    ) -> Self {
        let key = scope.handle(key.as_tagged(heap).erase());
        // unclassifiable keys are treated as name-keyed
        let index = match key.as_tagged(heap).classify_key(heap) {
            Some(Key::Element(i)) => Some(i),
            _ => None,
        };
        let start = scope.handle(start.as_tagged(heap).erase());
        let mut it = Lookup {
            key,
            index,
            own_only,
            holder: None,
            chain: Vec::new(),
        };
        if let Some(pairs) = start.as_tagged(heap).get_as::<FixedArray>(heap)
            && !own_only
        {
            it.enter_list(heap, scope, pairs);
        } else {
            it.holder = Some(start);
        }
        it
    }

    /// The key as a name (indices appear as canonical Smis).
    fn name<'h>(&self, heap: &'h Heap) -> Tagged<'h, SlotName> {
        self.key.as_tagged(heap).as_name()
    }

    /// Yield the next resolved position, advancing past its holder so
    /// a later call continues the walk; `Miss` when exhausted.
    pub fn next<'h>(&mut self, heap: &'h Heap, scope: &'s HandleScope<'_>) -> Hit<'h> {
        let hole = heap.known().the_hole.get(heap).erase();
        loop {
            while self.holder.is_none() {
                if !self.pick_parent(heap, scope) {
                    return Hit::Miss;
                }
            }
            let holder = self.holder.as_ref().copied().unwrap();
            let at = holder.as_tagged(heap);

            if at.is_nullish(heap) || at.ptr_eq(hole) {
                self.retire_position();
                continue;
            }
            if Proxy::is_proxy(heap, at) {
                self.advance_past(heap, scope, at);
                return Hit::Proxy(at);
            }
            if let Some(s) = DenseString::from_receiver(heap, at)
                && let Some(own) = s.own_key(heap, self.name(heap))
            {
                self.advance_past(heap, scope, at);
                return Hit::StringOwn { holder: at, own };
            }
            if let Some(o) = at.as_heap_object()
                && let Some(hit) = self.lookup_in_object(heap, o)
            {
                self.advance_past(heap, scope, at);
                return hit;
            }
            if self.own_only {
                self.retire_position();
                return Hit::Miss;
            }

            self.advance_from(heap, scope, at);
        }
    }

    fn lookup_in_object<'h>(&self, heap: &'h Heap, o: Tagged<'h, Object>) -> Option<Hit<'h>> {
        let name = self.name(heap);

        if let Some(i) = self.index {
            if let Some(dict) = o.element_dictionary(heap)
                && let Some(entry) = dict.find(heap, i)
            {
                let details = dict.details_at(heap, entry);
                let mut flags = SlotFlags::VALUE;
                if details & DETAILS_WRITABLE != 0 {
                    flags = flags.union(SlotFlags::WRITABLE);
                }
                if details & DETAILS_ENUMERABLE != 0 {
                    flags = flags.union(SlotFlags::ENUMERABLE);
                }
                if details & DETAILS_CONFIGURABLE != 0 {
                    flags = flags.union(SlotFlags::CONFIGURABLE);
                }
                if details & DETAILS_ACCESSOR != 0 {
                    let pair = dict
                        .value_at(heap, entry)
                        .get_as::<AccessorPair>(heap)
                        .expect("accessor dictionary entry holds a pair");
                    return Some(Hit::Accessor {
                        holder: o,
                        index: i,
                        pair,
                        flags: flags.union(SlotFlags::ACCESSOR),
                    });
                }
                return Some(Hit::Element {
                    holder: o,
                    index: i,
                    value: dict.value_at(heap, entry),
                    flags,
                });
            }
            if let Some(value) = o.element_value(heap, i) {
                return Some(Hit::Element {
                    holder: o,
                    index: i,
                    value,
                    flags: SlotFlags::WRITABLE
                        .union(SlotFlags::ENUMERABLE)
                        .union(SlotFlags::CONFIGURABLE),
                });
            }
            return None;
        }

        for (index, d) in o.map(heap).descriptors().iter().enumerate() {
            if !d.name(heap).ptr_eq(name) {
                continue;
            }
            if d.flags().is_accessor() {
                let value = d.value.get(heap);
                if let Some(pair) = value.get_as::<AccessorPair>(heap) {
                    return Some(Hit::Accessor {
                        holder: o,
                        index,
                        pair,
                        flags: d.flags(),
                    });
                }
                return Some(Hit::NativeAccessor {
                    holder: o,
                    index,
                    info: value
                        .get_as::<AccessorInfo>(heap)
                        .expect("accessor descriptor holds a pair or accessor info"),
                    flags: d.flags(),
                });
            }
            let offset = d.offset();
            return Some(Hit::Data {
                holder: o,
                index,
                offset,
                value: o.slot(heap, offset).get(heap),
                flags: d.flags(),
            });
        }

        if let Some((slot, value)) = o.parent_name(heap, name) {
            return Some(Hit::Parent {
                holder: o,
                slot,
                value,
            });
        }

        None
    }

    fn pick_parent(&mut self, heap: &Heap, scope: &'s HandleScope<'_>) -> bool {
        let hole = heap.known().the_hole.get(heap).erase();
        loop {
            let top = self.chain.last().copied();
            let Some(link) = top else {
                return false;
            };
            let Link::List { pairs, next } = link else {
                self.chain.pop();
                continue;
            };
            let pairs_at = pairs.as_tagged(heap);
            if next >= pairs_at.len() {
                self.chain.pop();
                if matches!(self.chain.last(), Some(Link::Parent { .. })) {
                    self.chain.pop();
                }
                continue;
            }
            let parent = pairs_at.at(heap, next);
            if let Some(Link::List { next, .. }) = self.chain.last_mut() {
                *next += 2;
            }
            if parent.is_nullish(heap) || parent.ptr_eq(hole) || parent.as_heap_object().is_none() {
                continue;
            }
            if let Some(nested) = parent.get_as::<FixedArray>(heap) {
                self.enter_list(heap, scope, nested);
                continue;
            }
            if self.on_path(heap, parent) {
                continue;
            }
            self.chain.push(Link::Parent {
                value: scope.handle(parent),
            });
            self.holder = Some(scope.handle(parent));
            return true;
        }
    }

    fn advance_past(&mut self, heap: &Heap, scope: &'s HandleScope<'_>, holder: Tagged<'_, Value>) {
        if self.own_only {
            self.retire_position();
        } else {
            self.advance_from(heap, scope, holder);
        }
    }

    fn advance_from(&mut self, heap: &Heap, scope: &'s HandleScope<'_>, holder: Tagged<'_, Value>) {
        let next = if let Some(proto) = holder.primitive_prototype(heap) {
            proto
        } else {
            let Some(o) = holder.as_heap_object() else {
                self.retire_position();
                return;
            };
            o.prototype(heap)
        };
        if let Some(pairs) = next.get_as::<FixedArray>(heap) {
            if !self.enter_list(heap, scope, pairs) {
                // Cyclic list: this subtree is missing.
                self.retire_position();
            }
        } else {
            self.holder = Some(scope.handle(next));
        }
    }

    fn enter_list(
        &mut self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        pairs: Tagged<'_, FixedArray>,
    ) -> bool {
        if self.on_path(heap, pairs.erase()) {
            return false;
        }
        self.chain.push(Link::List {
            pairs: scope.handle(pairs),
            next: 1,
        });
        self.holder = None;
        true
    }

    fn on_path(&self, heap: &Heap, target: Tagged<'_, Value>) -> bool {
        self.chain.iter().any(|link| match link {
            Link::List { pairs, .. } => pairs.as_tagged(heap).erase().ptr_eq(target),
            Link::Parent { value } => value.as_tagged(heap).ptr_eq(target),
        })
    }

    fn retire_position(&mut self) {
        self.holder = None;
        if matches!(self.chain.last(), Some(Link::Parent { .. })) {
            self.chain.pop();
        }
    }
}
