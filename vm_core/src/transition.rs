use crate::VmError;
use crate::bail;
use core::alloc::Layout;

use crate::{
    AccessorPair, AllocToken, Compare, ElementsKind, FixedArray, Handle, HandleScope, Heap, Key,
    Map, MapInit, MapKind, MaybeWeak, Object, Prototype, Result, SlotFlags, SlotName, Smi, Symbol,
    Tagged, ThreadState, VM, Value, WeakFixedArray, WeakFixedArrayInit,
};

pub enum SiblingChange<'s> {
    Prototype(Handle<'s, Value>),
    Holey,
    Dictionary,
}

impl<'s> SiblingChange<'s> {
    fn kind(&self, kind: MapKind) -> MapKind {
        match self {
            Self::Prototype(_) => kind,
            Self::Holey => kind.with_elements(ElementsKind::Holey),
            Self::Dictionary => kind.with_elements(ElementsKind::Dictionary),
        }
    }

    fn prototype(
        &self,
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        parent: Tagged<'_, Map>,
    ) -> Handle<'s, Value> {
        match self {
            Self::Prototype(proto) => *proto,
            Self::Holey | Self::Dictionary => scope.handle(parent.prototype.get(heap)),
        }
    }

    fn find<'a>(&self, heap: &'a Heap, parent: Tagged<'a, Map>) -> Option<Tagged<'a, Map>> {
        match self {
            Self::Prototype(proto) => parent.find_prototype_transition(heap, proto.as_tagged(heap)),
            Self::Holey => parent.find_holey_transition(heap),
            Self::Dictionary => parent.find_dictionary_transition(heap),
        }
    }

    fn sentinel<'a>(&self, heap: &'a Heap) -> Tagged<'a, Symbol> {
        match self {
            Self::Prototype(_) => heap.known().prototype_transition_symbol.get(heap),
            Self::Holey => heap.known().holey_transition_symbol.get(heap),
            Self::Dictionary => heap.known().dictionary_transition_symbol.get(heap),
        }
    }
}

pub struct Transition;

/// The descriptor value a transition target should carry for its replaced
/// or appended row.
enum DescriptorValue<'s> {
    /// Data row: a slot offset chosen by the transition.
    Data,
    /// Accessor row: a freshly allocated getter/setter pair.
    Accessor(Handle<'s, Value>, Handle<'s, Value>),
    /// Native accessor row: keep the existing accessor-info value.
    Native(Handle<'s, Value>),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Change {
    /// Append a new last descriptor.
    Append,
    /// Replace the descriptor at `index`, keeping its position.
    Replace { index: usize },
}

impl Transition {
    fn target<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        parent: impl for<'a> Fn(&'a Heap) -> Tagged<'a, Map>,
        name: Handle<SlotName>,
        flags: SlotFlags,
        row: DescriptorValue<'s>,
        change: Change,
    ) -> Handle<'s, Map> {
        debug_assert_eq!(flags.is_accessor(), !matches!(row, DescriptorValue::Data));
        loop {
            let pair_values = match &row {
                DescriptorValue::Accessor(get, set) => {
                    Some((get.as_tagged(heap), set.as_tagged(heap)))
                }
                _ => None,
            };
            if let Some(target) =
                parent(heap).find_transition(heap, name.as_tagged(heap), flags, pair_values)
            {
                return target.as_handle(scope);
            }

            let parent_ref = parent(heap);
            let old_row = match change {
                Change::Append => None,
                Change::Replace { index } => {
                    let d = &parent_ref.descriptors()[index];
                    let offset = (!d.flags().is_accessor())
                        .then(|| d.value.try_smi().expect("data row offset"));
                    Some((d.flags(), offset))
                }
            };
            let kind = parent_ref.kind();
            let descriptor_count = parent_ref.descriptor_count();
            let value_slot_count = parent_ref.value_slot_count();
            let prototype = scope.handle(parent_ref.prototype.get(heap));
            let pairs_len = parent_ref.transitions.load(heap).map_or(0, |a| a.len());

            let grow = !flags.is_accessor()
                && match change {
                    Change::Append => true,
                    Change::Replace { .. } => old_row.expect("replace row").0.is_accessor(),
                };
            let row_offset = match change {
                Change::Replace { .. } if !grow && !flags.is_accessor() => {
                    old_row.expect("replace row").1.expect("data row offset") as usize
                }
                _ => value_slot_count,
            };
            let appends = usize::from(matches!(change, Change::Append));

            let map_layout = Map::layout_for(descriptor_count + appends);
            let pairs_layout = WeakFixedArray::<Value>::layout_for(pairs_len + 2);
            let total = match &row {
                DescriptorValue::Accessor(..) => AllocToken::total_for(&[
                    Layout::new::<AccessorPair>(),
                    map_layout,
                    pairs_layout,
                ]),
                _ => AllocToken::total_for(&[map_layout, pairs_layout]),
            };

            let attempt = heap.allocate_token_enter_heap(total, |token, heap| {
                let parent_ref = parent(heap);
                let word = parent_ref.transitions.load_word(heap);
                let old = parent_ref.transitions.decode(heap, word);
                if old.map_or(0, |a| a.len()) != pairs_len {
                    token.discard_remaining();
                    return None;
                }

                let name_word = name.as_tagged(heap);
                let row_value = match &row {
                    DescriptorValue::Accessor(get, set) => {
                        scope.handle(token.allocate::<AccessorPair>((*get, *set)).erase())
                    }
                    DescriptorValue::Native(value) => scope.handle(value.as_tagged(heap)),
                    DescriptorValue::Data => scope.handle(Smi::new(row_offset as i64)),
                };

                let mut descriptors: Vec<(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>)> =
                    parent_ref
                        .descriptors()
                        .iter()
                        .map(|d| {
                            (
                                scope.handle(d.name(heap)),
                                d.flags(),
                                scope.handle(d.value.get(heap)),
                            )
                        })
                        .collect();
                let name_handle = scope.handle(name_word);
                match change {
                    Change::Append => descriptors.push((name_handle, flags, row_value)),
                    Change::Replace { index } => {
                        descriptors[index] = (name_handle, flags, row_value)
                    }
                }
                let child = token.allocate::<Map>(MapInit {
                    kind,
                    value_slot_count: value_slot_count + usize::from(grow),
                    descriptors: &descriptors,
                    prototype,
                });
                child.pred.set(heap, child.erase(), parent_ref);

                let mut pairs: Vec<Tagged<'_, MaybeWeak<Value>>> =
                    Vec::with_capacity(pairs_len + 2);
                if let Some(old) = old {
                    for entry in old.as_slice().as_chunks::<2>().0 {
                        pairs.push(entry[0].get(heap));
                        pairs.push(entry[1].get(heap));
                    }
                }
                pairs.push(name_word.erase().as_maybe_weak());
                pairs.push(child.erase().as_weak());
                let pairs = token.allocate::<WeakFixedArray>(WeakFixedArrayInit { values: &pairs });

                match parent_ref
                    .transitions
                    .publish(heap, parent_ref.erase(), word, pairs)
                {
                    Ok(()) => Some(child.as_handle(scope)),
                    Err(_) => None,
                }
            });
            if let Some(child) = attempt {
                return child;
            }
        }
    }

    /// Replace a native-accessor row's flags while keeping its accessor-info
    /// value (array `length` writability).
    pub fn replace_native_accessor_flags(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<'_, Object>,
        name: Handle<'_, SlotName>,
        index: usize,
        flags: SlotFlags,
    ) {
        let map = receiver.as_tagged(heap).map(heap);
        let value = scope.handle(map.descriptors()[index].value.get(heap));
        let target = Self::target(
            heap,
            scope,
            |heap| receiver.as_tagged(heap).map(heap),
            name,
            flags,
            DescriptorValue::Native(value),
            Change::Replace { index },
        );
        let host = receiver.as_tagged(heap).erase();
        Prototype::shape_changed(heap, receiver.as_tagged(heap).map(heap));
        receiver
            .as_tagged(heap)
            .header
            .map
            .set(heap, host, target.as_tagged(heap));
    }

    pub fn sibling_target<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        parent: impl for<'a> Fn(&'a Heap) -> Tagged<'a, Map>,
        change: SiblingChange<'s>,
    ) -> Handle<'s, Map> {
        loop {
            if let Some(target) = change.find(heap, parent(heap)) {
                return target.as_handle(scope);
            }

            let parent_ref = parent(heap);
            let kind = change.kind(parent_ref.kind());
            let descriptor_count = parent_ref.descriptor_count();
            let value_slot_count = parent_ref.value_slot_count();
            let prototype = change.prototype(heap, scope, parent_ref);
            let pairs_len = parent_ref.transitions.load(heap).map_or(0, |a| a.len());

            let map_layout = Map::layout_for(descriptor_count);
            let pairs_layout = WeakFixedArray::<Value>::layout_for(pairs_len + 2);
            let total = AllocToken::total_for(&[map_layout, pairs_layout]);

            let attempt = heap.allocate_token_enter_heap(total, |token, heap| {
                let parent_ref = parent(heap);
                let word = parent_ref.transitions.load_word(heap);
                let old = parent_ref.transitions.decode(heap, word);
                if old.map_or(0, |a| a.len()) != pairs_len {
                    token.discard_remaining();
                    return None;
                }

                let descriptors: Vec<(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>)> =
                    parent_ref
                        .descriptors()
                        .iter()
                        .map(|d| {
                            (
                                scope.handle(d.name(heap)),
                                d.flags(),
                                scope.handle(d.value.get(heap)),
                            )
                        })
                        .collect();
                let child = token.allocate::<Map>(MapInit {
                    kind,
                    value_slot_count,
                    descriptors: &descriptors,
                    prototype,
                });
                child.pred.set(heap, child.erase(), parent_ref);

                let sentinel = change.sentinel(heap);
                let mut pairs: Vec<Tagged<'_, MaybeWeak<Value>>> =
                    Vec::with_capacity(pairs_len + 2);
                if let Some(old) = old {
                    for entry in old.as_slice().as_chunks::<2>().0 {
                        pairs.push(entry[0].get(heap));
                        pairs.push(entry[1].get(heap));
                    }
                }
                pairs.push(sentinel.erase().as_maybe_weak());
                pairs.push(child.erase().as_weak());
                let pairs = token.allocate::<WeakFixedArray>(WeakFixedArrayInit { values: &pairs });

                match parent_ref
                    .transitions
                    .publish(heap, parent_ref.erase(), word, pairs)
                {
                    Ok(()) => Some(child.as_handle(scope)),
                    Err(_) => None,
                }
            });
            if let Some(child) = attempt {
                return child;
            }
            // another thread published first: retry the lookup
        }
    }

    fn grow_slots_and_swap(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        flags: SlotFlags,
        value: Handle<Value>,
    ) {
        // The append's target row and offset, plus the parent's used count
        // and the array's current reservation: a preallocated array can
        // absorb the write in place.
        let (offset, parent_count, old_len) = {
            let receiver_ref = receiver.as_tagged(heap);
            let parent = receiver_ref.map(heap);
            let target = parent
                .find_transition(heap, name.as_tagged(heap), flags, None)
                .expect("transition recorded above");
            let index = target
                .descriptors()
                .iter()
                .position(|d| d.name(heap).ptr_eq(name.as_tagged(heap)) && d.flags() == flags)
                .expect("transition target row");
            (
                target.descriptor(index).offset(),
                parent.value_slot_count(),
                receiver_ref.slots(heap).as_slice().len(),
            )
        };

        if offset < old_len {
            let receiver_ref = receiver.as_tagged(heap);
            let target = receiver_ref
                .map(heap)
                .find_transition(heap, name.as_tagged(heap), flags, None)
                .expect("transition recorded above");
            let host = receiver_ref.erase();
            receiver_ref
                .slot(heap, offset)
                .set(heap, host, value.as_tagged(heap));
            Prototype::shape_changed(heap, receiver_ref.map(heap));
            receiver_ref.header.map.set(heap, host, target);
            return;
        }

        // Reservation exhausted: grow with fresh headroom so the next
        // appends land in place.
        debug_assert_eq!(offset, old_len, "append out of order");
        debug_assert_eq!(offset, parent_count, "slot count desynced from map");
        let capacity = parent_count + 1 + Map::SLACK_MARGIN;
        let slots = heap.allocate_hole_array(capacity).as_handle(scope);
        let receiver_ref = receiver.as_tagged(heap);
        let target = receiver_ref
            .map(heap)
            .find_transition(heap, name.as_tagged(heap), flags, None)
            .expect("transition recorded above");
        {
            let old = receiver_ref.slots(heap);
            let new = slots.as_tagged(heap);
            for k in 0..old_len {
                new.set(heap, k, old.at(heap, k));
            }
            new.set(heap, offset, value.as_tagged(heap));
        }
        let host = receiver_ref.erase();
        receiver_ref.slots.set(heap, host, slots.as_tagged(heap));
        Prototype::shape_changed(heap, receiver_ref.map(heap));
        receiver_ref.header.map.set(heap, host, target);
    }

    fn swap_map(
        heap: &mut Heap,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        flags: SlotFlags,
        pair: Option<(Handle<'_, Value>, Handle<'_, Value>)>,
    ) {
        let receiver_ref = receiver.as_tagged(heap);
        let pair_values = pair.map(|(get, set)| (get.as_tagged(heap), set.as_tagged(heap)));
        let target = receiver_ref
            .map(heap)
            .find_transition(heap, name.as_tagged(heap), flags, pair_values)
            .expect("transition recorded above");
        Prototype::shape_changed(heap, receiver_ref.map(heap));
        receiver_ref
            .header
            .map
            .set(heap, receiver.as_tagged(heap).erase(), target);
    }

    pub fn write_slot(
        heap: &mut Heap,
        receiver: Handle<Object>,
        index: usize,
        value: Handle<'_, Value>,
    ) {
        let offset = receiver.as_tagged(heap).map(heap).descriptors()[index].offset();
        receiver.as_tagged(heap).slot(heap, offset).set(
            heap,
            receiver.as_tagged(heap).erase(),
            value.as_tagged(heap),
        );
    }

    pub fn remove_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
    ) {
        let (kind, prototype, surviving, values) = {
            let name_word = name.as_tagged(heap);
            let obj = receiver.as_tagged(heap);
            let parent = obj.map(heap);
            let descriptors = parent.descriptors();
            let index = descriptors
                .iter()
                .position(|d| d.name(heap).ptr_eq(name_word))
                .expect("caller verified the descriptor exists");
            // the structural region is everything below the first
            // descriptor-owned slot (function info/context; empty for
            // ordinary objects)
            let base = descriptors
                .iter()
                .filter(|d| !d.flags().is_accessor())
                .map(|d| d.offset())
                .min()
                .unwrap_or(parent.value_slot_count());
            // names are rooted here and re-anchored in the allocating
            // closures: a Tagged cannot escape this non-allocating region
            let mut surviving: Vec<(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>)> =
                Vec::with_capacity(descriptors.len() - 1);
            let mut values: Vec<Handle<'_, Value>> = obj.slots(heap).as_slice()[..base]
                .iter()
                .map(|slot| scope.handle(slot.get(heap)))
                .collect();
            for (i, d) in descriptors.iter().enumerate() {
                if i == index {
                    continue;
                }
                if d.flags().is_accessor() {
                    // accessors embed their pair in the descriptor row
                    surviving.push((
                        scope.handle(d.name(heap)),
                        d.flags(),
                        scope.handle(d.value.get(heap)),
                    ));
                } else {
                    // data rows re-dense their offsets; the value rides
                    // along in slot order
                    values.push(scope.handle(obj.slot(heap, d.offset()).get(heap)));
                    surviving.push((
                        scope.handle(d.name(heap)),
                        d.flags(),
                        scope.handle(Smi::new(values.len() as i64 - 1)),
                    ));
                }
            }
            (
                parent.kind(),
                scope.handle(parent.prototype.get(heap)),
                surviving,
                values,
            )
        };

        // the parent map is immutable except its transitions slot, so the
        // descriptor/value snapshot stays valid across every retry below
        loop {
            // shared child map: reuse it instead of growing the tree (the
            // initial check, and the loser's convergence point after a
            // lost publication race)
            let existing = receiver
                .as_tagged(heap)
                .map(heap)
                .find_remove_transition(heap, name.as_tagged(heap))
                .map(|m| m.as_handle(scope));
            if let Some(existing) = existing {
                // only this receiver's slots need compacting
                let values = scope.stage_handles(&values);
                heap.allocate_token_enter_heap(
                    FixedArray::<Value>::layout_for(values.len()),
                    |token, heap| {
                        let obj = receiver.as_tagged(heap);
                        let slots = token.allocate::<FixedArray>(values);
                        let host = receiver.as_tagged(heap).erase();
                        obj.slots.set(heap, host, slots);
                        Prototype::shape_changed(heap, obj.map(heap));
                        obj.header.map.set(heap, host, existing.as_tagged(heap));
                    },
                );
                return;
            }

            let pairs_len = receiver
                .as_tagged(heap)
                .map(heap)
                .transitions
                .load(heap)
                .map_or(0, |a| a.len());

            let map_layout = Map::layout_for(surviving.len());
            let pairs_layout = WeakFixedArray::<Value>::layout_for(pairs_len + 2);
            let slots_layout = FixedArray::<Value>::layout_for(values.len());
            let total = AllocToken::total_for(&[map_layout, pairs_layout, slots_layout]);
            let values = scope.stage_handles(&values);
            let published = heap.allocate_token_enter_heap(total, |token, heap| {
                let obj = receiver.as_tagged(heap);
                let parent = obj.map(heap);
                // re-derive the publication after the reservation; see
                // Transition::target for the same-length invariant
                let word = parent.transitions.load_word(heap);
                let old = parent.transitions.decode(heap, word);
                if old.map_or(0, |a| a.len()) != pairs_len {
                    token.discard_remaining();
                    return None;
                }
                let child = token.allocate::<Map>(MapInit {
                    kind,
                    value_slot_count: values.len(),
                    descriptors: &surviving,
                    prototype,
                });
                child.pred.set(heap, child.erase(), parent);
                let name_word = name.as_tagged(heap);
                let mut pairs: Vec<Tagged<'_, MaybeWeak<Value>>> =
                    Vec::with_capacity(pairs_len + 2);
                if let Some(old) = old {
                    for entry in old.as_slice().as_chunks::<2>().0 {
                        pairs.push(entry[0].get(heap));
                        pairs.push(entry[1].get(heap));
                    }
                }
                pairs.push(name_word.erase().as_maybe_weak());
                pairs.push(child.erase().as_weak());
                let pairs = token.allocate::<WeakFixedArray>(WeakFixedArrayInit { values: &pairs });
                let slots = token.allocate::<FixedArray>(values);
                match parent
                    .transitions
                    .publish(heap, parent.erase(), word, pairs)
                {
                    Ok(()) => {
                        let host = receiver.as_tagged(heap).erase();
                        obj.slots.set(heap, host, slots);
                        Prototype::shape_changed(heap, obj.map(heap));
                        obj.header.map.set(heap, host, child);
                        Some(())
                    }
                    Err(_) => None,
                }
            });
            if published.is_some() {
                return;
            }
            // lost a race: retry — the find above converges on the
            // winner's shared child map, or re-extends the longer array
        }
    }

    pub fn define(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        desc: PropertyDescriptor<'_>,
        change: Change,
    ) {
        let flags = desc.flags();
        match desc {
            PropertyDescriptor::Data { value, .. } => {
                let grow = match change {
                    Change::Append => true,
                    Change::Replace { index } => receiver.as_tagged(heap).map(heap).descriptors()
                        [index]
                        .flags()
                        .is_accessor(),
                };
                Self::target(
                    heap,
                    scope,
                    |heap| receiver.as_tagged(heap).map(heap),
                    name,
                    flags,
                    DescriptorValue::Data,
                    change,
                );
                if grow {
                    Self::grow_slots_and_swap(heap, scope, receiver, name, flags, value);
                } else {
                    let Change::Replace { index } = change else {
                        unreachable!("appends always grow")
                    };
                    Self::write_slot(heap, receiver, index, value);
                    Self::swap_map(heap, receiver, name, flags, None);
                }
            }
            PropertyDescriptor::Accessor { get, set, .. } => {
                Self::target(
                    heap,
                    scope,
                    |heap| receiver.as_tagged(heap).map(heap),
                    name,
                    flags,
                    DescriptorValue::Accessor(get, set),
                    change,
                );
                Self::swap_map(heap, receiver, name, flags, Some((get, set)));
            }
        }
    }

    pub fn is_compatible_property_descriptor(
        heap: &Heap,
        extensible: bool,
        desc: &PartialDescriptor<'_>,
        current: Option<&PartialDescriptor<'_>>,
    ) -> bool {
        let Some(current) = current else {
            return extensible;
        };

        if current.configurable != Some(false) {
            return true;
        }
        if desc.configurable == Some(true) {
            return false;
        }
        if let Some(e) = desc.enumerable
            && e != current.enumerable.unwrap_or(false)
        {
            return false;
        }
        // a non-configurable property cannot change kind
        let cur_is_data = current.is_data_descriptor();
        if !desc.is_generic_descriptor() && desc.is_data_descriptor() != cur_is_data {
            return false;
        }
        let undefined = heap.known().undefined.get(heap).erase();
        if cur_is_data && desc.is_data_descriptor() {
            if current.writable != Some(true) {
                if desc.writable == Some(true) {
                    return false;
                }
                if let Some(v) = desc.value
                    && !Compare::same_value(
                        heap,
                        v.as_tagged(heap),
                        current.value.map_or(undefined, |w| w.as_tagged(heap)),
                    )
                {
                    return false;
                }
            }
        } else if !cur_is_data && desc.is_accessor_descriptor() {
            if let Some(g) = desc.get
                && !Compare::same_value(
                    heap,
                    g.as_tagged(heap),
                    current.get.map_or(undefined, |h| h.as_tagged(heap)),
                )
            {
                return false;
            }
            if let Some(s) = desc.set
                && !Compare::same_value(
                    heap,
                    s.as_tagged(heap),
                    current.set.map_or(undefined, |h| h.as_tagged(heap)),
                )
            {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PropertyDescriptor<'s> {
    Data {
        value: Handle<'s, Value>,
        writable: bool,
        enumerable: bool,
        configurable: bool,
    },
    Accessor {
        get: Handle<'s, Value>,
        set: Handle<'s, Value>,
        enumerable: bool,
        configurable: bool,
    },
}

impl<'s> PropertyDescriptor<'s> {
    pub const fn data(value: Handle<'s, Value>) -> Self {
        Self::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        }
    }

    pub const fn method(value: Handle<'s, Value>) -> Self {
        Self::Data {
            value,
            writable: true,
            enumerable: false,
            configurable: true,
        }
    }

    pub const fn non_enumerable(value: Handle<'s, Value>) -> Self {
        Self::Data {
            value,
            writable: false,
            enumerable: false,
            configurable: true,
        }
    }

    pub const fn is_data_descriptor(&self) -> bool {
        matches!(self, Self::Data { .. })
    }

    pub const fn is_accessor_descriptor(&self) -> bool {
        matches!(self, Self::Accessor { .. })
    }

    pub const fn is_configurable(&self) -> bool {
        match self {
            Self::Data { configurable, .. } | Self::Accessor { configurable, .. } => *configurable,
        }
    }

    pub const fn is_writable(&self) -> bool {
        matches!(self, Self::Data { writable: true, .. })
    }

    pub const fn flags(self) -> SlotFlags {
        match self {
            Self::Data {
                writable,
                enumerable,
                configurable,
                ..
            } => {
                let mut flags = SlotFlags::VALUE;
                if writable {
                    flags = flags.union(SlotFlags::WRITABLE);
                }
                if enumerable {
                    flags = flags.union(SlotFlags::ENUMERABLE);
                }
                if configurable {
                    flags = flags.union(SlotFlags::CONFIGURABLE);
                }
                flags
            }
            Self::Accessor {
                enumerable,
                configurable,
                ..
            } => {
                let mut flags = SlotFlags::ACCESSOR;
                if enumerable {
                    flags = flags.union(SlotFlags::ENUMERABLE);
                }
                if configurable {
                    flags = flags.union(SlotFlags::CONFIGURABLE);
                }
                flags
            }
        }
    }
}

impl Object {
    pub fn add_own_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        desc: PropertyDescriptor<'_>,
    ) -> bool {
        // elements are the only home of canonical indices
        if let Some(Key::Element(i)) = name.as_tagged(heap).erase().classify_key(heap) {
            if Self::element_own_descriptor(heap, scope, receiver.as_tagged(heap), i).is_none()
                && !can_add_element(heap, receiver.as_tagged(heap), i)
            {
                return false;
            }
            return Self::define_element(heap, scope, receiver, i, desc);
        }
        debug_assert!(
            !receiver
                .as_tagged(heap)
                .map(heap)
                .descriptors()
                .iter()
                .any(|d| d.name(heap).ptr_eq(name.as_tagged(heap))),
            "add_own_property requires the name to be absent from the receiver's own map"
        );
        let cond_7 = receiver.as_tagged(heap).is_extendable(heap);
        if !cond_7 {
            return false;
        }
        Transition::define(heap, scope, receiver, name, desc, Change::Append);
        true
    }

    /// Parent pairs are name-only: canonical indices have exactly one
    /// home, the elements store.
    pub fn add_parent(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        value: Handle<'_, Value>,
    ) -> Result<()> {
        if !receiver.as_tagged(heap).is_extendable(heap) {
            bail!(vm, heap, state, VmError::NotExtensible);
        }
        if let Some(Key::Element(i)) = name.as_tagged(heap).erase().classify_key(heap) {
            Self::define_element(heap, scope, receiver, i, PropertyDescriptor::data(value));
            return Ok(());
        }
        let mut pairs: Vec<Handle<'_, Value>> = Vec::new();
        {
            let base = receiver.as_tagged(heap).prototype(heap);
            if let Some(existing) = base.get_as::<FixedArray>(heap) {
                for i in 0..existing.len() {
                    pairs.push(scope.handle(existing.at(heap, i)));
                }
            }
        }
        pairs.push(name.erase());
        pairs.push(value);
        let pairs = heap.allocate_handle::<FixedArray>(scope.stage_handles(&pairs), scope);
        Self::set_prototype(vm, heap, state, scope, receiver, pairs.erase())
    }
}

/// ES 10.4.2.1 steps 2-3 and 10.1.6.1 step 2: a new element needs an
/// extensible receiver and, for arrays, a writable `length`.
pub fn can_add_element(heap: &Heap, receiver: Tagged<'_, Object>, index: usize) -> bool {
    if receiver.is_array(heap)
        && index >= receiver.length()
        && !receiver.map(heap).length_writable(heap)
    {
        return false;
    }
    receiver.is_extendable(heap)
}

impl Object {
    /// - `proto` must be:
    /// - arbitrary normal `Object` for JS semantics
    /// - `FixedArray` of objects (Self-style multiple parents),
    /// - the hole sentinel; other values are
    ///   silently ignored (sloppy `__proto__` semantics)
    /// - cycles and non-extensible receivers throw (TODO: make this optional for Self semantics)
    pub fn set_prototype(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        proto: Handle<Value>,
    ) -> Result<()> {
        let proto_word = proto.as_tagged(heap);
        let known = heap.known();
        let is_null = proto_word.ptr_eq(known.null.get(heap).erase());
        let is_hole = proto_word.ptr_eq(known.the_hole.get(heap).erase());
        if !is_null && !is_hole && (proto_word.is_primitive(heap) || !proto_word.is_strong_ptr()) {
            // silently ignore non-object prototypes (sloppy-mode semantics)
            return Ok(());
        }

        {
            let cond_9 = receiver
                .as_tagged(heap)
                .map(heap)
                .prototype
                .get(heap)
                .ptr_eq(proto.as_tagged(heap));
            if cond_9 {
                return Ok(());
            }
        }
        {
            // ES 10.4.7: an immutable-prototype exotic object may only
            // "set" its current prototype (SameValue → true above).
            let immutable = receiver
                .as_tagged(heap)
                .map(heap)
                .kind()
                .is_immutable_prototype();
            if immutable {
                bail!(vm, heap, state, VmError::ImmutablePrototype);
            }
        }
        {
            let cond_10 = receiver.as_tagged(heap).map(heap).kind().is_extendable();
            if !cond_10 {
                bail!(vm, heap, state, VmError::NotExtensible);
            }
        }

        {
            let null = heap.known().null.get(heap).erase();
            let this = receiver.as_tagged(heap).erase();
            let start = proto.as_tagged(heap);
            fn walk<'b>(
                heap: &'b Heap,
                null: Tagged<'b, Value>,
                this: Tagged<'b, Value>,
                mut p: Tagged<'b, Value>,
            ) -> bool {
                while p.is_strong_ptr() && !p.ptr_eq(null) {
                    if p.ptr_eq(this) {
                        return false;
                    }
                    let Some(o) = p.as_heap_object() else {
                        break;
                    };
                    p = o.prototype(heap);
                }
                true
            }
            if let Some(pairs) = start.get_as::<FixedArray>(heap) {
                let mut i = 1;
                while i < pairs.len() {
                    if !walk(heap, null, this, pairs.at(heap, i)) {
                        bail!(vm, heap, state, VmError::CyclicPrototype);
                    }
                    i += 2;
                }
            } else if !walk(heap, null, this, start) {
                bail!(vm, heap, state, VmError::CyclicPrototype);
            }
        }

        let target = Transition::sibling_target(
            heap,
            scope,
            |heap| receiver.as_tagged(heap).map(heap),
            SiblingChange::Prototype(proto),
        );
        let host = receiver.as_tagged(heap).erase();
        Prototype::shape_changed(heap, receiver.as_tagged(heap).map(heap));
        receiver
            .as_tagged(heap)
            .header
            .map
            .set(heap, host, target.as_tagged(heap));
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PartialDescriptor<'s> {
    pub value: Option<Handle<'s, Value>>,
    pub get: Option<Handle<'s, Value>>,
    pub set: Option<Handle<'s, Value>>,
    pub writable: Option<bool>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl<'s> PartialDescriptor<'s> {
    pub fn value(value: Handle<'s, Value>) -> Self {
        Self {
            value: Some(value),
            ..Self::default()
        }
    }

    /// The full data descriptor OrdinarySet uses when the receiver has no
    /// existing own property: `{ value, writable: true, enumerable: true,
    /// configurable: true }`.
    pub fn new_data(value: Handle<'s, Value>) -> Self {
        Self {
            value: Some(value),
            writable: Some(true),
            enumerable: Some(true),
            configurable: Some(true),
            ..Self::default()
        }
    }

    /// ES 6.2.6.2 IsDataDescriptor.
    pub fn is_data_descriptor(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    /// ES 6.2.6.1 IsAccessorDescriptor.
    pub fn is_accessor_descriptor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    /// ES 6.2.6.3 IsGenericDescriptor.
    pub fn is_generic_descriptor(&self) -> bool {
        !self.is_data_descriptor() && !self.is_accessor_descriptor()
    }

    pub fn complete_against(
        &self,
        undefined: Handle<'s, Value>,
        current: Option<&PropertyDescriptor<'s>>,
    ) -> PropertyDescriptor<'s> {
        // accessor if either side (desc first, then current) says so
        let accessor = self.is_accessor_descriptor()
            || !self.is_data_descriptor()
                && current.is_some_and(|c| matches!(c, PropertyDescriptor::Accessor { .. }));

        if accessor {
            let cur = match current {
                Some(PropertyDescriptor::Accessor { get, set, .. }) => (Some(*get), Some(*set)),
                _ => (None, None),
            };
            PropertyDescriptor::Accessor {
                get: self.get.or(cur.0).unwrap_or(undefined),
                set: self.set.or(cur.1).unwrap_or(undefined),
                enumerable: self
                    .enumerable
                    .or(current.map(c_enumumerable))
                    .unwrap_or(false),
                configurable: self
                    .configurable
                    .or(current.map(c_configurable))
                    .unwrap_or(false),
            }
        } else {
            let cur_value = match current {
                Some(PropertyDescriptor::Data { value, .. }) => Some(*value),
                _ => None,
            };
            PropertyDescriptor::Data {
                value: self.value.or(cur_value).unwrap_or(undefined),
                writable: self
                    .writable
                    .or(current.and_then(c_writable))
                    .unwrap_or(false),
                enumerable: self
                    .enumerable
                    .or(current.map(c_enumumerable))
                    .unwrap_or(false),
                configurable: self
                    .configurable
                    .or(current.map(c_configurable))
                    .unwrap_or(false),
            }
        }
    }
}

impl<'s> From<&PropertyDescriptor<'s>> for PartialDescriptor<'s> {
    fn from(d: &PropertyDescriptor<'s>) -> Self {
        match *d {
            PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => Self {
                value: Some(value),
                get: None,
                set: None,
                writable: Some(writable),
                enumerable: Some(enumerable),
                configurable: Some(configurable),
            },
            PropertyDescriptor::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => Self {
                value: None,
                get: Some(get),
                set: Some(set),
                writable: None,
                enumerable: Some(enumerable),
                configurable: Some(configurable),
            },
        }
    }
}

fn c_enumumerable(d: &PropertyDescriptor<'_>) -> bool {
    match d {
        PropertyDescriptor::Data { enumerable, .. }
        | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
    }
}

fn c_configurable(d: &PropertyDescriptor<'_>) -> bool {
    match d {
        PropertyDescriptor::Data { configurable, .. }
        | PropertyDescriptor::Accessor { configurable, .. } => *configurable,
    }
}

fn c_writable(d: &PropertyDescriptor<'_>) -> Option<bool> {
    match d {
        PropertyDescriptor::Data { writable, .. } => Some(*writable),
        PropertyDescriptor::Accessor { .. } => None,
    }
}
