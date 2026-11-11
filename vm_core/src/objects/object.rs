use crate::VmError;
use crate::bail;
use core::alloc::Layout;

use crate::errors::Errors;
use crate::{
    AccessorPair, CallableInfoObject, ContextObject, Convert, DETAILS_ACCESSOR,
    DETAILS_CONFIGURABLE, DETAILS_DATA, DETAILS_ENUMERABLE, DETAILS_WRITABLE, DenseString,
    EdgeVisitable, ElementsKind, EntryClass, FixedArray, Float, FunctionKind, GcSlot, Handle,
    HandleScope, HandleSlice, Header, Heap, HeapObject, Hint, HostCtx, Key, Map, MapInit, MapKind,
    NativeIndex, NumberDictionary, NumberDictionaryInit, ObjectKind, PropertyDescriptor, Prototype,
    Result, SiblingChange, SlotName, Smi, Symbol, Tagged, ThreadState, Transition, VM, Value,
    Visitor, WrapperKind, raise_runtime,
};

/// What a set action did, for callers that only need to update caches.
pub enum SetApplied {
    Done,
    Transition,
    CallSetter,
    Rejected,
}

#[repr(C)]
pub struct Object {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    /// Dense elements are a `FixedArray`; dictionary elements are a
    /// `NumberDictionary`. The map's elements kind selects the shape.
    pub elements: GcSlot<Value>,
    pub length: GcSlot<Smi>,
}

impl Object {
    pub fn layout_for() -> Layout {
        Layout::new::<Self>()
    }

    pub fn callable_info<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, CallableInfoObject>> {
        let kind = self.map(heap).kind();
        if !kind.is_callable() || kind.is_bound() || kind.is_proxy() {
            return None;
        }
        // every non-exotic callable is laid out as [info, context]
        self.slots(heap)
            .at(heap, 0)
            .get_as::<CallableInfoObject>(heap)
    }

    pub fn closure_context<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ContextObject>> {
        let kind = self.map(heap).kind();
        if !kind.is_callable() || kind.is_bound() || kind.is_proxy() {
            return None;
        }
        self.slots
            .get(heap)
            .at(heap, 1)
            .get_as::<ContextObject>(heap)
    }

    /// The `idx`-th entry of the callable's constant pool, as a slot name.
    pub fn constant_slot_name<'a>(&self, heap: &'a Heap, idx: usize) -> Tagged<'a, SlotName> {
        self.callable_info(heap)
            .expect("callable must have callable info")
            .constant_slot_name(heap, idx)
    }

    #[inline(always)]
    pub fn is_array_exotic(&self, heap: &Heap) -> bool {
        self.map(heap).kind().is_array()
    }

    /// The JSArray `length` internal slot when `self` is an array named
    /// `name`, for the IC's field-accessor check. `None` for any other name
    /// or non-array.
    #[inline]
    pub fn array_length<'a>(
        &self,
        heap: &'a Heap,
        name: Tagged<'a, SlotName>,
    ) -> Option<Tagged<'a, Value>> {
        if !self.is_array_exotic(heap) {
            return None;
        }
        name.erase()
            .ptr_eq(heap.known().strings.length.get(heap))
            .then(|| self.length.get(heap).erase())
    }

    /// The object's map (shape).
    #[inline(always)]
    pub fn map<'a>(&self, heap: &'a Heap) -> Tagged<'a, Map> {
        self.header.map.get(heap)
    }

    /// The map's prototype link.
    #[inline(always)]
    pub fn prototype<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.map(heap).prototype.get(heap)
    }

    /// The object's slots backing store.
    #[inline(always)]
    pub fn slots<'a>(&self, heap: &'a Heap) -> Tagged<'a, FixedArray> {
        self.slots.get(heap)
    }

    /// The raw elements backing store word. Dense receivers hold a
    /// `FixedArray`; dictionary receivers hold a `NumberDictionary`.
    #[inline(always)]
    pub fn elements<'a>(&self, heap: &'a Heap) -> Tagged<'a, Value> {
        self.elements.get(heap)
    }

    /// Whether the object's map allows adding new properties.
    pub fn is_extendable(&self, heap: &Heap) -> bool {
        self.map(heap).kind().is_extendable()
    }

    /// The slot holding the value of the data slot at `offset`.
    #[inline]
    pub fn slot<'a>(&self, heap: &'a Heap, offset: usize) -> &'a GcSlot {
        self.slots(heap).as_ref().element_slot(offset)
    }

    #[inline(always)]
    pub fn length(&self) -> usize {
        self.length.to_smi_unchecked().value() as usize
    }

    #[inline(always)]
    pub fn elements_array<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, FixedArray>> {
        let kind = self.map(heap).kind();
        // JSReceiver layout guard: non-receiver heap objects (strings,
        // pairs, ...) do not have this layout
        if !kind.is_js_receiver() || !kind.is_dense_elements() {
            // dictionary-backed: not a FixedArray
            return None;
        }
        // Safety: the receiver map's elements kind says the backing store
        // is a FixedArray.
        Some(unsafe { self.elements.get(heap).cast::<FixedArray>() })
    }

    /// The sparse-elements backing store, when the receiver is in
    /// dictionary-elements mode.
    #[inline]
    pub fn element_dictionary<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, NumberDictionary>> {
        let kind = self.map(heap).kind();
        if !kind.is_js_receiver() || !kind.is_dictionary_elements() {
            return None;
        }
        // Safety: the receiver map's elements kind says the backing store
        // is a NumberDictionary.
        Some(unsafe { self.elements.get(heap).cast::<NumberDictionary>() })
    }

    pub fn promote_holey(heap: &mut Heap, scope: &HandleScope<'_>, receiver: &Handle<'_, Object>) {
        let obj = receiver.as_tagged(heap);
        let map = obj.map(heap);
        // one-way lattice: only packed maps promote; holey and
        // dictionary maps are already past this rung
        if map.kind().elements() != ElementsKind::Packed {
            return;
        }
        let map = scope.handle(map);
        // the map swap must invalidate chains keyed on the old map:
        // later shape changes fire on the new map lineage only
        Prototype::shape_changed(heap, map.as_tagged(heap));
        let target =
            Transition::sibling_target(heap, scope, |h| map.as_tagged(h), SiblingChange::Holey);
        let obj = receiver.as_tagged(heap);
        obj.header
            .map
            .set(heap, obj.erase(), target.as_tagged(heap));
    }

    /// Self-style parent-name slot in this object's parent pairs.
    pub fn parent_name<'a>(
        &self,
        heap: &'a Heap,
        name: Tagged<'a, SlotName>,
    ) -> Option<(usize, Tagged<'a, Value>)> {
        let pairs = self
            .map(heap)
            .prototype
            .get(heap)
            .get_as::<FixedArray>(heap)?;
        let name = name.erase();
        let mut i = 0;
        while i < pairs.len() {
            if pairs.at(heap, i).ptr_eq(name) {
                return Some((i + 1, pairs.at(heap, i + 1)));
            }
            i += 2;
        }
        None
    }

    /// Arrays bound element visibility by the `length` field; every
    /// other receiver by the elements capacity.
    #[inline]
    pub fn max_element_index(&self, heap: &Heap) -> usize {
        if self.is_array_exotic(heap) {
            return self.length();
        }
        let kind = self.map(heap).kind();
        if kind.is_dictionary_elements() {
            self.element_dictionary(heap)
                .map_or(0, |dict| dict.capacity())
        } else {
            self.elements_array(heap)
                .map_or(0, |elements| elements.len())
        }
    }

    /// Own element indices in ascending order: dense hits in index
    /// order, dictionary entries collected and sorted.
    pub fn element_keys(&self, heap: &Heap) -> Vec<usize> {
        let mut out = Vec::new();
        if let Some(dict) = self.element_dictionary(heap) {
            dict.for_each_entry(heap, |k, _, _| out.push(k));
            out.sort_unstable();
        } else if let Some(elements) = self.elements_array(heap) {
            let hole = heap.known().the_hole.get(heap).erase();
            let bound = self.max_element_index(heap).min(elements.len());
            for i in 0..bound {
                if elements.at(heap, i) != hole {
                    out.push(i);
                }
            }
        }
        out
    }

    #[inline(always)]
    pub fn element_value_dense<'a>(&self, heap: &'a Heap, i: usize) -> Option<Tagged<'a, Value>> {
        let kind = self.map(heap).kind();
        // JSReceiver layout guard (see `elements_array`)
        if !kind.is_js_receiver() || !kind.is_dense_elements() {
            return None;
        }
        let elements = self.elements.get(heap).get_as::<FixedArray>(heap)?;
        if kind.is_holey() {
            // `length`/capacity can exceed the backing store: those
            // indices are holes
            if i >= elements.len() {
                return None;
            }
            let v = elements.at(heap, i);
            if v == heap.known().the_hole.get(heap) {
                return None;
            }
            Some(v)
        } else {
            // packed: arrays promise capacity covers `length`, so one
            // bounds check suffices; other receivers are bounded by
            // capacity alone
            if self.is_array_exotic(heap) {
                if i >= self.length() {
                    return None;
                }
            } else if i >= elements.len() {
                return None;
            }
            Some(elements.at(heap, i))
        }
    }

    #[inline]
    pub fn element_value<'a>(&self, heap: &'a Heap, i: usize) -> Option<Tagged<'a, Value>> {
        if let Some(v) = self.element_value_dense(heap, i) {
            return Some(v);
        }
        // dictionary probe: arrays are additionally bounded by `length`;
        // every other receiver is bounded by the dictionary itself
        if self.is_array_exotic(heap) && i >= self.length() {
            return None;
        }
        let dict = self.element_dictionary(heap)?;
        let entry = dict.find(heap, i)?;
        if dict.is_accessor_at(heap, entry) {
            return None;
        }
        Some(dict.value_at(heap, entry))
    }
}

pub enum CallTarget<'a> {
    Bytecode {
        target: Tagged<'a, Object>,
        info: Tagged<'a, CallableInfoObject>,
        context: Tagged<'a, ContextObject>,
        kind: FunctionKind,
    },
    Native(NativeIndex),
    /// A callable proxy: `[[Call]]` dispatches through the `apply` trap.
    Proxy(Tagged<'a, Object>),
    /// A bound function (ES 10.4.2): slots are
    /// `[target, boundThis, boundArgs]` and both entry points forward.
    Bound {
        target: Tagged<'a, Value>,
        bound_this: Tagged<'a, Value>,
        bound_args: Tagged<'a, Value>,
    },
}

impl<'a> Tagged<'a, Value> {
    pub fn is_callable(self, heap: &Heap) -> bool {
        let Some(obj) = self.as_heap_object() else {
            return false;
        };
        obj.map(heap).kind().is_callable()
    }

    /// ES 7.3.26 PrivateElementFind restricted to fields: an own data
    /// descriptor matching the private Symbol key (no prototype walk — private
    /// elements live only on the instance itself).
    pub fn private_find(self, heap: &'a Heap, key: Tagged<'a, Value>) -> Option<&'a GcSlot> {
        let o = self.as_heap_object()?;
        let map = o.map(heap);
        for d in map.descriptors() {
            if d.name(heap).ptr_eq(key.as_name()) && !d.flags().is_accessor() {
                return Some(o.slot(heap, d.offset()));
            }
        }
        None
    }

    /// Write an existing element slot in place: never allocates, never
    /// grows, no handle scope required — takes anchored `Tagged` words
    /// under a single shared heap borrow (safe for fast paths).
    #[inline]
    pub fn store_array_element_in_place(
        self,
        heap: &Heap,
        i: usize,
        value: Tagged<'_, Value>,
    ) -> bool {
        let Some(obj) = self.as_heap_object() else {
            return false;
        };
        let kind = obj.map(heap).kind();
        if !kind.is_array() || !kind.is_dense_elements() || i >= obj.length() {
            return false;
        }
        let Some(elements) = obj.elements_array(heap) else {
            return false;
        };
        if i >= elements.len() {
            return false;
        }
        if kind.is_holey() && elements.at(heap, i) == heap.known().the_hole.get(heap) {
            return false;
        }
        elements.set(heap, i, value);
        Prototype::element_mutated_kind(heap, obj, kind);
        true
    }

    #[inline(always)]
    pub fn call_target(self, heap: &'a Heap) -> Option<CallTarget<'a>> {
        let obj = self.as_heap_object()?;
        let kind = obj.map(heap).kind();
        if kind.is_proxy() {
            return Some(CallTarget::Proxy(obj));
        }
        if kind.is_bound() {
            let slots = obj.slots(heap);
            return Some(CallTarget::Bound {
                target: slots.at(heap, 0),
                bound_this: slots.at(heap, 1),
                bound_args: slots.at(heap, 2),
            });
        }
        if !kind.is_callable() {
            return None;
        }
        // every non-runtime callable object is laid out with
        // `[callable info, context]` in its first two slots
        let slots = obj.slots(heap);
        let info = unsafe { slots.at(heap, 0).cast::<CallableInfoObject>() };
        if let Some(index) = info.native_index(heap) {
            return Some(CallTarget::Native(index));
        }
        let context = unsafe { slots.at(heap, 1).cast::<ContextObject>() };
        let descriptor = info.descriptor.to_smi_unchecked().value() as u64;
        let register_count = (descriptor & 0xffff) as usize;
        let formal_min = ((descriptor >> 16) & 0xffff) as usize;
        let kind = FunctionKind::decode(((descriptor >> 32) & 0xf) as i64);
        debug_assert_eq!(
            register_count,
            info.register_count.to_smi().value() as usize
        );
        debug_assert_eq!(formal_min, info.formal_parameter_count() + 1);
        debug_assert_eq!(kind, info.function_kind());
        Some(CallTarget::Bytecode {
            target: obj,
            info,
            context,
            kind,
        })
    }
}

impl Object {
    pub fn store_array_element(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        i: usize,
        value: &Handle<'_, Value>,
    ) {
        let new_len = i.checked_add(1).expect("element index fits usize");

        let (grows, logical_len, capacity, dictionary, is_array) = {
            let obj = receiver.as_tagged(heap);
            let dictionary = obj.element_dictionary(heap).is_some();
            let capacity = obj.elements_array(heap).map(|e| e.len()).unwrap_or(0);
            // arrays track a logical length; other receivers end at
            // their elements capacity
            let is_array = obj.is_array_exotic(heap);
            let logical = if is_array { obj.length() } else { capacity };
            (i >= capacity, logical, capacity, dictionary, is_array)
        };

        if dictionary {
            Self::store_dictionary_element(heap, scope, receiver, i, value);
            return;
        }

        if i > logical_len {
            Self::promote_holey(heap, scope, receiver);
        }

        if grows {
            Self::grow_elements(
                heap,
                scope,
                receiver,
                i,
                value,
                new_len,
                logical_len,
                is_array,
            );
            // the grown capacity is holes past the written index; arrays
            // hide them behind `length`, other receivers are bounded by
            // capacity
            if !is_array {
                Self::promote_holey(heap, scope, receiver);
            }
        } else {
            // the same applies to a write that leaves trailing capacity
            if !is_array && new_len < capacity {
                Self::promote_holey(heap, scope, receiver);
            }
            let obj = receiver.as_tagged(heap);
            let elements = obj
                .elements_array(heap)
                .expect("dense elements have a backing array");
            elements.set(heap, i, value.as_tagged(heap));
            // a store inside the physical capacity but past an array's
            // logical length still extends the array
            if is_array && i >= obj.length() {
                obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
            }
            Prototype::element_mutated(heap, obj);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn grow_elements(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        i: usize,
        value: &Handle<'_, Value>,
        new_len: usize,
        logical_len: usize,
        is_array: bool,
    ) {
        let obj = receiver.as_tagged(heap);
        let elements = obj
            .elements_array(heap)
            .expect("dense elements have a backing array");
        let (normalize, capacity) = {
            let new_capacity = (new_len + (new_len >> 1) + 16).max(elements.len());
            if i - elements.len() >= 1024 {
                (true, new_capacity)
            } else if new_capacity > 1000 {
                let mut used = 0usize;
                for k in 0..elements.len() {
                    if elements.at(heap, k) != heap.known().the_hole.get(heap) {
                        used += 1;
                    }
                }
                let dict_words =
                    NumberDictionary::compute_capacity(used) * NumberDictionary::ENTRY_SIZE;
                (3 * dict_words <= new_capacity, new_capacity)
            } else {
                (false, new_capacity)
            }
        };
        if normalize {
            Self::normalize_elements(heap, scope, receiver);
            Self::store_dictionary_element(heap, scope, receiver, i, value);
            return;
        }
        let keep = logical_len.min(
            receiver
                .as_tagged(heap)
                .elements_array(heap)
                .map(|e| e.len())
                .unwrap_or(0),
        );
        let elements = heap.allocate_hole_array(capacity).as_handle(scope);
        {
            let obj = receiver.as_tagged(heap);
            let old = obj
                .elements_array(heap)
                .expect("dense elements have a backing array");
            let new = elements.as_tagged(heap);
            for k in 0..keep {
                new.set(heap, k, old.at(heap, k));
            }
            new.set(heap, i, value.as_tagged(heap));
        }
        let obj = receiver.as_tagged(heap);
        obj.elements
            .set(heap, obj.erase(), elements.as_tagged(heap).erase());
        if is_array && new_len > logical_len {
            obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
        }
        Prototype::element_mutated(heap, obj);
    }

    /// Plain element store into a dictionary-mode array. Accessor and
    /// read-only entries are the keyed cold path's business; reaching
    /// them here (builtin callers), the store is a spec-ignored no-op.
    fn store_dictionary_element(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        i: usize,
        value: &Handle<'_, Value>,
    ) {
        let new_len = i.checked_add(1).expect("element index fits usize");
        let (dict, length, is_array) = {
            let obj = receiver.as_tagged(heap);
            let dict = obj
                .element_dictionary(heap)
                .expect("dictionary elements have a dictionary");
            let is_array = obj.is_array_exotic(heap);
            (scope.handle(dict), obj.length(), is_array)
        };
        let existing = match dict.as_tagged(heap).classify(heap, i) {
            EntryClass::ReadOnly | EntryClass::Accessor => return,
            EntryClass::Data => dict
                .as_tagged(heap)
                .find(heap, i)
                .map(|entry| dict.as_tagged(heap).details_at(heap, entry)),
            EntryClass::Absent => None,
        };
        if existing.is_none() {
            let cap_words = dict.as_tagged(heap).capacity() * NumberDictionary::ENTRY_SIZE;
            // only arrays have a logical length to preserve when densifying
            let needed = if is_array {
                new_len.max(length)
            } else {
                new_len
            };
            if !dict.as_tagged(heap).requires_slow_elements()
                && needed <= cap_words * 2
                && needed < (1 << 26)
            {
                Self::densify_elements(heap, scope, receiver, needed);
                Self::store_array_element(heap, scope, receiver, i, value);
                return;
            }
        }
        let details = existing.unwrap_or(DETAILS_DATA);
        let table = NumberDictionary::insert(heap, scope, &dict, i, *value, details);
        let obj = receiver.as_tagged(heap);
        if table.as_tagged(heap) != dict.as_tagged(heap) {
            obj.elements
                .set(heap, obj.erase(), table.as_tagged(heap).erase());
        }
        if is_array && i >= length {
            obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
        }
        Prototype::element_mutated(heap, obj);
    }

    /// The current own descriptor of an element, if present (dense hits
    /// are plain `w,e,c` data; dictionary entries carry their details).
    pub fn element_own_descriptor<'s>(
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        receiver: Tagged<'_, Object>,
        i: usize,
    ) -> Option<PropertyDescriptor<'s>> {
        if let Some(v) = receiver.element_value_dense(heap, i) {
            return Some(PropertyDescriptor::Data {
                value: scope.handle(v),
                writable: true,
                enumerable: true,
                configurable: true,
            });
        }
        let dict = receiver.element_dictionary(heap)?;
        let entry = dict.find(heap, i)?;
        let details = dict.details_at(heap, entry);
        let flags = |bit: i64| details & bit != 0;
        if dict.is_accessor_at(heap, entry) {
            let pair = dict
                .value_at(heap, entry)
                .get_as::<AccessorPair>(heap)
                .expect("accessor dictionary entry holds a pair");
            return Some(PropertyDescriptor::Accessor {
                get: scope.handle(pair.get.get(heap)),
                set: scope.handle(pair.set.get(heap)),
                enumerable: flags(DETAILS_ENUMERABLE),
                configurable: flags(DETAILS_CONFIGURABLE),
            });
        }
        Some(PropertyDescriptor::Data {
            value: scope.handle(dict.value_at(heap, entry)),
            writable: flags(DETAILS_WRITABLE),
            enumerable: flags(DETAILS_ENUMERABLE),
            configurable: flags(DETAILS_CONFIGURABLE),
        })
    }

    /// `[[DefineOwnProperty]]` for a canonical index. Plain
    /// `w,e,c` data defines use the dense store; accessors and
    /// non-default attributes become dictionary entries carrying their
    /// details. `desc` must already be completed against the current
    /// element descriptor (see [`Self::element_own_descriptor`]).
    pub fn define_element(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        i: usize,
        desc: PropertyDescriptor<'_>,
    ) -> bool {
        let is_array = receiver.as_tagged(heap).is_array_exotic(heap);
        if let PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        } = desc
        {
            Self::store_array_element(heap, scope, &receiver, i, &value);
            return true;
        }
        if receiver
            .as_tagged(heap)
            .map(heap)
            .kind()
            .is_dense_elements()
        {
            Self::normalize_elements(heap, scope, &receiver);
        }
        let dict = scope.handle(
            receiver
                .as_tagged(heap)
                .element_dictionary(heap)
                .expect("dictionary elements have a dictionary"),
        );
        let (value, details, accessor) = match desc {
            PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => (
                value,
                (DETAILS_WRITABLE * writable as i64)
                    | (DETAILS_ENUMERABLE * enumerable as i64)
                    | (DETAILS_CONFIGURABLE * configurable as i64),
                false,
            ),
            PropertyDescriptor::Accessor {
                get,
                set,
                enumerable,
                configurable,
            } => {
                let pair = heap.allocate_handle::<AccessorPair>((get, set), scope);
                (
                    pair.erase(),
                    DETAILS_ACCESSOR
                        | (DETAILS_ENUMERABLE * enumerable as i64)
                        | (DETAILS_CONFIGURABLE * configurable as i64),
                    true,
                )
            }
        };
        let table = NumberDictionary::insert(heap, scope, &dict, i, value, details);
        if accessor {
            table.as_tagged(heap).require_slow(heap);
        }
        let obj = receiver.as_tagged(heap);
        let host = obj.erase();
        obj.elements.set(heap, host, table.as_tagged(heap).erase());
        if is_array && i >= obj.length() {
            obj.length.set(heap, host, Smi::new((i + 1) as i64));
        }
        Prototype::element_mutated(heap, obj);
        true
    }

    /// ES 10.4.2.3 ArraySetLength after the caller has performed
    /// ToNumber/ToUint32 and range validation. Shrinking punches holes and
    /// drops configurable dictionary entries; non-configurable entries
    /// clamp the effective length.
    pub fn set_array_length(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        new_len: usize,
    ) -> bool {
        if !receiver.as_tagged(heap).is_array_exotic(heap) {
            return false;
        }
        if new_len > 32 * 1024 * 1024
            && receiver
                .as_tagged(heap)
                .map(heap)
                .kind()
                .is_dense_elements()
        {
            Self::normalize_elements(heap, scope, receiver);
        }
        if let Some(dict) = receiver.as_tagged(heap).element_dictionary(heap) {
            let old_len = receiver.as_tagged(heap).length();
            if new_len < old_len {
                let mut doomed: Vec<(usize, i64)> = Vec::new();
                dict.as_ref().for_each_entry(heap, |k, _, details| {
                    if k >= new_len {
                        doomed.push((k, details));
                    }
                });
                doomed.sort_unstable_by(|a, b| b.0.cmp(&a.0));
                let mut effective = new_len;
                let mut blocked = false;
                for (k, details) in doomed {
                    if details & DETAILS_CONFIGURABLE == 0 {
                        effective = k + 1;
                        blocked = true;
                        break;
                    }
                    dict.as_ref().delete(heap, k);
                }
                Prototype::element_mutated(heap, receiver.as_tagged(heap));
                let obj = receiver.as_tagged(heap);
                obj.as_ref()
                    .length
                    .set(heap, obj.erase(), Smi::new(effective as i64));
                return !blocked;
            }
            let obj = receiver.as_tagged(heap);
            obj.as_ref()
                .length
                .set(heap, obj.erase(), Smi::new(new_len as i64));
            return true;
        }

        let old_len = receiver.as_tagged(heap).length();
        if new_len < old_len {
            let hole = heap.known().the_hole.get(heap).erase();
            if let Some(elements) = receiver.as_tagged(heap).elements_array(heap) {
                for i in new_len..old_len.min(elements.len()) {
                    elements.as_ref().set(heap, i, hole);
                }
            }
            // a later length grow re-exposes the punched holes
            Self::promote_holey(heap, scope, receiver);
            Prototype::element_mutated(heap, receiver.as_tagged(heap));
        } else if new_len > old_len {
            // the indices [old_len, new_len) are holes: the map must stop
            // promising packed elements (and packed capacity covers length)
            Self::promote_holey(heap, scope, receiver);
        }
        let obj = receiver.as_tagged(heap);
        obj.as_ref()
            .length
            .set(heap, obj.erase(), Smi::new(new_len as i64));
        true
    }

    pub fn normalize_elements(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
    ) {
        let obj = receiver.as_tagged(heap);
        let map = obj.map(heap);
        if map.kind().is_dictionary_elements() {
            return;
        }
        let map = scope.handle(map);
        let mut entries: Vec<(usize, Handle<'_, Value>)> = Vec::new();
        let obj = receiver.as_tagged(heap);
        if let Some(elements) = obj.elements_array(heap) {
            for k in 0..elements.len() {
                let v = elements.at(heap, k);
                if v != heap.known().the_hole.get(heap) {
                    entries.push((k, scope.handle(v)));
                }
            }
        }
        let mut dict = heap.allocate_handle::<NumberDictionary>(
            NumberDictionaryInit {
                capacity: NumberDictionary::compute_capacity(entries.len()),
            },
            scope,
        );
        for (k, v) in &entries {
            dict = NumberDictionary::insert(heap, scope, &dict, *k, *v, DETAILS_DATA);
        }
        // the map swap must invalidate chains keyed on the old map
        Prototype::shape_changed(heap, map.as_tagged(heap));
        let target = Transition::sibling_target(
            heap,
            scope,
            |h| map.as_tagged(h),
            SiblingChange::Dictionary,
        );
        let obj = receiver.as_tagged(heap);
        obj.elements
            .set(heap, obj.erase(), dict.as_tagged(heap).erase());
        obj.header
            .map
            .set(heap, obj.erase(), target.as_tagged(heap));
    }

    fn densify_elements(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        capacity: usize,
    ) {
        let obj = receiver.as_tagged(heap);
        let map = obj.map(heap);
        if !map.kind().is_dictionary_elements() {
            return;
        }
        let map = scope.handle(map);
        let mut entries: Vec<(usize, Handle<'_, Value>)> = Vec::new();
        let obj = receiver.as_tagged(heap);
        if let Some(dict) = obj.element_dictionary(heap) {
            dict.for_each_entry(heap, |k, v, _| {
                if k < capacity {
                    entries.push((k, scope.handle(v)));
                }
            });
        }
        let elements = heap.allocate_hole_array(capacity).as_handle(scope);
        for (k, v) in &entries {
            elements.as_tagged(heap).set(heap, *k, v.as_tagged(heap));
        }
        Prototype::shape_changed(heap, map.as_tagged(heap));
        let target =
            Transition::sibling_target(heap, scope, |h| map.as_tagged(h), SiblingChange::Holey);
        let obj = receiver.as_tagged(heap);
        obj.elements
            .set(heap, obj.erase(), elements.as_tagged(heap).erase());
        obj.header
            .map
            .set(heap, obj.erase(), target.as_tagged(heap));
    }
}

impl<'a> Tagged<'a, Object> {
    /// `[[ParameterMap]]`: `None` for unmapped arguments objects.
    /// Layout of the internal map: `[context, index0, slot0, ...]`; deleted
    /// entries are holes (the map itself is never observable).
    pub fn arguments_parameter_map(self, heap: &'a Heap) -> Option<Tagged<'a, FixedArray>> {
        if !self.map(heap).kind().is_arguments() {
            return None;
        }
        self.slots(heap).at(heap, 0).get_as::<FixedArray>(heap)
    }

    /// The environment binding an arguments index maps to.
    pub fn arguments_mapping(
        self,
        heap: &'a Heap,
        index: usize,
    ) -> Option<(Tagged<'a, ContextObject>, usize)> {
        let map = self.arguments_parameter_map(heap)?;
        if map.len() < 3 {
            return None;
        }
        let context = map.at(heap, 0).get_as::<ContextObject>(heap)?;
        let mut i = 1;
        while i + 1 < map.len() {
            if map.at(heap, i).to_i64() == Some(index as i64) {
                let slot = map.at(heap, i + 1).to_i64()?;
                return usize::try_from(slot).ok().map(|slot| (context, slot));
            }
            i += 2;
        }
        None
    }

    /// `Get(map, P)`: the current value of the mapped parameter binding.
    pub fn arguments_mapped_value(self, heap: &'a Heap, index: usize) -> Option<Tagged<'a, Value>> {
        let (context, slot) = self.arguments_mapping(heap, index)?;
        let slots = context.slots.get(heap);
        (slot < slots.len()).then(|| slots.at(heap, slot))
    }

    /// `Set(map, P, V, false)`: writes the mapped parameter binding.
    pub fn arguments_mapped_set(self, heap: &Heap, index: usize, value: Tagged<'_, Value>) {
        let Some((context, slot)) = self.arguments_mapping(heap, index) else {
            return;
        };
        let slots = context.slots.get(heap);
        if slot < slots.len() {
            slots.set(heap, slot, value);
        }
    }

    /// `Delete(map, P)`: breaks the index-to-parameter correspondence.
    pub fn arguments_remove_mapping(self, heap: &Heap, index: usize) {
        let Some(map) = self.arguments_parameter_map(heap) else {
            return;
        };
        let hole = heap.known().the_hole.get(heap).erase();
        let mut i = 1;
        while i + 1 < map.len() {
            if map.at(heap, i).to_i64() == Some(index as i64) {
                map.set(heap, i, hole);
                return;
            }
            i += 2;
        }
    }
}

pub struct ObjectInit<'a> {
    pub map: Handle<'a, Map>,
    pub slots: Handle<'a, FixedArray>,
    pub elements: Handle<'a, FixedArray>,
    pub length: usize,
}

/// Fresh arguments object inputs (ES 10.4.4.6 / 10.4.4.7).
pub struct ArgumentsCreate<'a> {
    /// The actual argument list (`argList`).
    pub values: HandleSlice<'a>,
    /// `callee`: the function being invoked (mapped arguments only).
    pub function: Handle<'a, Value>,
    /// Internal `[[ParameterMap]]` metadata; `None` for unmapped objects.
    pub parameter_map: Option<Handle<'a, FixedArray>>,
    pub iterator_key: Handle<'a, SlotName>,
    pub iterator: Handle<'a, Value>,
    pub throw_type_error: Handle<'a, Value>,
}

pub struct ObjectSlotsInit<'m, 'v> {
    pub map: Handle<'m, Map>,
    pub values: HandleSlice<'v>,
    pub elements: Handle<'m, FixedArray>,
    pub length: usize,
}

impl Object {
    /// Initialize fresh storage directly from anchored words, without
    /// `Handle`s. The bump-allocation fast paths call this after reading
    /// well-known words and guarantee no GC between the reads and here.
    pub fn init_raw(
        &mut self,
        heap: &Heap,
        map: Tagged<'_, Map>,
        slots: Tagged<'_, FixedArray>,
        elements: Tagged<'_, FixedArray>,
        length: usize,
    ) {
        let host = self.tagged(heap);
        self.header.map.set(heap, host, map);
        self.slots.set(heap, host, slots);
        self.elements.set(heap, host, elements.erase());
        self.length.set(heap, host, Smi::new(length as i64));
    }
}

impl HeapObject for Object {
    const KIND: ObjectKind = ObjectKind::Object;

    fn matches_kind(kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Object
                | ObjectKind::Array
                | ObjectKind::ByteArray
                | ObjectKind::String
                | ObjectKind::Oddball
        )
    }
    type Init<'a> = ObjectInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Self::layout_for()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.init_raw(
            heap,
            config.map.as_tagged(heap),
            config.slots.as_tagged(heap),
            config.elements.as_tagged(heap),
            config.length,
        );
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for()
    }
}

impl EdgeVisitable for Object {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.slots.as_raw());
        visitor.visit(self.elements.as_raw());
    }
}

impl Object {
    pub fn create_closure<'a>(
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        info: Handle<'_, CallableInfoObject>,
        context: Handle<'_, ContextObject>,
    ) -> Tagged<'a, Object> {
        let info_ref = info.as_tagged(heap);
        let kind = info_ref.function_kind();
        let formal_length = info_ref.formal_length();
        let function_name = match info_ref.name(heap) {
            Some(name) => scope.handle(name),
            None => heap.known().strings.empty.get_handle(heap, scope).erase(),
        };
        let map = scope.handle(if kind.is_class_constructor() {
            heap.known().class_constructor_map.get(heap)
        } else if kind.is_constructible() {
            heap.known().function_map.get(heap)
        } else {
            heap.known().non_constructor_function_map.get(heap)
        });
        // class constructors carry a third hidden slot: the instance-field
        // array ([key0, init0, ...]); undefined until SetClassFields
        let args = if kind.is_class_constructor() {
            scope.stage(&[
                info.as_tagged(heap).erase(),
                context.as_tagged(heap).erase(),
                heap.known().undefined.get(heap).erase(),
            ])
        } else {
            scope.stage(&[
                info.as_tagged(heap).erase(),
                context.as_tagged(heap).erase(),
            ])
        };
        let function = heap.new_object(scope, map, args).as_handle(scope);

        let length_key = heap.known().strings.length.get_handle(heap, scope);
        let name_key = heap.known().strings.name.get_handle(heap, scope);
        assert!(
            Object::define_own_property_ordinary(
                heap,
                scope,
                function,
                length_key,
                PropertyDescriptor::Data {
                    value: scope.handle(Smi::new(formal_length as i64)),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("fresh closure length define"),
            "fresh closure accepts length"
        );
        assert!(
            Object::define_own_property_ordinary(
                heap,
                scope,
                function,
                name_key,
                PropertyDescriptor::Data {
                    value: function_name.erase(),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("fresh closure name define"),
            "fresh closure accepts name"
        );

        if kind.needs_prototype() {
            let proto_map = heap.known().object_initial_map.get_handle(heap, scope);
            let proto = heap
                .new_object(scope, proto_map, HandleSlice::EMPTY)
                .as_handle(scope);
            let constructor = heap.known().strings.constructor.get_handle(heap, scope);
            let prototype = heap.known().strings.prototype.get_handle(heap, scope);
            assert!(
                Object::define_own_property_ordinary(
                    heap,
                    scope,
                    proto,
                    constructor,
                    PropertyDescriptor::Data {
                        value: function.erase(),
                        writable: true,
                        enumerable: false,
                        configurable: true,
                    },
                )
                .expect("fresh prototype constructor define"),
                "fresh prototype accepts constructor"
            );
            assert!(
                Object::define_own_property_ordinary(
                    heap,
                    scope,
                    function,
                    prototype,
                    PropertyDescriptor::Data {
                        value: proto.erase(),
                        writable: true,
                        enumerable: false,
                        configurable: false,
                    },
                )
                .expect("fresh closure prototype define"),
                "fresh closure accepts prototype"
            );
        }

        function.as_tagged(heap)
    }

    /// `CreateUnmappedArgumentsObject ( argList )`
    /// [ES 10.4.4.6](https://tc39.es/ecma262/#sec-createunmappedargumentsobject)
    /// `CreateMappedArgumentsObject` shares this shape; `parameter_map`
    /// carries the internal mapping metadata (never exposed to JS).
    pub fn create_arguments<'h>(
        heap: &'h mut Heap,
        scope: &HandleScope<'_>,
        init: ArgumentsCreate<'_>,
    ) -> Tagged<'h, Object> {
        let kind = MapKind::OBJECT
            .union(MapKind::EXTENDABLE)
            .union(MapKind::ARGUMENTS);
        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind,
                value_slot_count: 1,
                descriptors: &[],
                prototype: heap
                    .known()
                    .object_prototype
                    .get_handle(heap, scope)
                    .erase(),
            },
            scope,
        );
        let elements = if init.values.is_empty() {
            scope.handle(heap.known().empty_fixed_array.get(heap))
        } else {
            heap.allocate_handle::<FixedArray>(init.values, scope)
        };
        let parameter_map = match init.parameter_map {
            Some(map) => map.as_tagged(heap).erase(),
            None => heap.known().undefined.get(heap).erase(),
        };
        let obj = scope.handle(heap.allocate_object(
            scope,
            ObjectSlotsInit {
                map,
                values: scope.stage(&[parameter_map]),
                elements,
                length: init.values.len(),
            },
        ));

        let length_key = heap.known().strings.length.get_handle(heap, scope);
        assert!(
            Object::define_own_property_ordinary(
                heap,
                scope,
                obj,
                length_key,
                PropertyDescriptor::Data {
                    value: scope.handle(Smi::new(init.values.len() as i64).into_tagged()),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("arguments length define"),
            "arguments length is configurable"
        );
        let callee_key = heap.known().strings.callee.get_handle(heap, scope);
        let callee = match init.parameter_map {
            Some(_) => PropertyDescriptor::Data {
                value: init.function,
                writable: true,
                enumerable: false,
                configurable: true,
            },
            None => PropertyDescriptor::Accessor {
                get: init.throw_type_error,
                set: init.throw_type_error,
                enumerable: false,
                configurable: false,
            },
        };
        assert!(
            Object::define_own_property_ordinary(heap, scope, obj, callee_key, callee)
                .expect("arguments callee define"),
            "fresh arguments object accepts callee"
        );
        assert!(
            Object::define_own_property_ordinary(
                heap,
                scope,
                obj,
                init.iterator_key,
                PropertyDescriptor::Data {
                    value: init.iterator,
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("arguments iterator define"),
            "arguments iterator is configurable"
        );
        obj.as_tagged(heap)
    }

    pub fn to_primitive<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        value: Handle<'_, Value>,
        hint: Hint,
    ) -> Result<Tagged<'a, Value>> {
        if value.as_tagged(heap).is_primitive(heap) {
            return Ok(value.as_tagged(heap));
        }
        let runtime = state.runtime(vm, heap);
        HostCtx::new(vm, runtime, heap, state).handle_scope(|vm, heap, state, scope| {
            // 1. exotic @@toPrimitive (GetMethod)
            let exotic = scope.handle(Object::get_property(
                vm,
                heap,
                state,
                value,
                value,
                heap.known()
                    .to_primitive_symbol
                    .get_handle(heap, &scope)
                    .erase(),
                false,
            )?);
            if exotic.as_tagged(heap) != heap.known().undefined.get(heap)
                && exotic.as_tagged(heap) != heap.known().null.get(heap)
            {
                if !exotic.as_tagged(heap).is_callable(heap) {
                    // GetMethod: a non-callable, non-nullish method is a TypeError
                    bail!(vm, heap, state, VmError::SymbolToPrimitiveNotCallable);
                }
                let s = &heap.known().strings;
                let hint_string = scope.handle(match hint {
                    Hint::Default => s.default.get(heap).erase(),
                    Hint::Number => s.number.get(heap).erase(),
                    Hint::String => s.string.get(heap).erase(),
                });
                let args = scope.stage(&[
                    value.as_tagged(heap).erase(),
                    hint_string.as_tagged(heap).erase(),
                ]);
                let result = scope.handle(HostCtx::enter(
                    vm, runtime, &mut *heap, state, exotic, args, None,
                )?);
                return if result.as_tagged(heap).is_primitive(heap) {
                    Ok(result.as_tagged(heap))
                } else {
                    bail!(vm, heap, state, VmError::SymbolToPrimitiveNotPrimitive)
                };
            }

            // 2. OrdinaryToPrimitive: hint string → toString first, else valueOf first
            let method_names: [Handle<'_, Value>; 2] = {
                let s = &heap.known().strings;
                if hint == Hint::String {
                    [
                        scope.handle(s.to_string.get(heap).erase()),
                        scope.handle(s.value_of.get(heap).erase()),
                    ]
                } else {
                    [
                        scope.handle(s.value_of.get(heap).erase()),
                        scope.handle(s.to_string.get(heap).erase()),
                    ]
                }
            };
            for name in method_names {
                let method = scope.handle(Object::get_property(
                    vm, heap, state, value, value, name, false,
                )?);
                if !method.as_tagged(heap).is_callable(heap) {
                    continue;
                }
                let args = scope.stage(&[value.as_tagged(heap)]);
                let result = scope.handle(HostCtx::enter(
                    vm, runtime, &mut *heap, state, method, args, None,
                )?);
                if result.as_tagged(heap).is_primitive(heap) {
                    return Ok(result.as_tagged(heap));
                }
                // object result: try the next method name
            }
            bail!(vm, heap, state, VmError::CannotConvertObjectToPrimitive)
        })
    }

    pub fn to_numeric(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<f64> {
        state.handle_scope(|scope| {
            // root the anchored result to release the heap borrow
            let v = scope.handle(Self::to_primitive(vm, heap, state, value, Hint::Number)?);
            v.as_tagged(heap)
                .to_number(heap)
                .ok_or_else(|| Errors::throw(vm, heap, state, VmError::CannotConvertToNumber))
        })
    }

    pub fn to_string<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let primitive: Handle<'_, Value> = if value.as_tagged(heap).is_primitive(heap) {
                value
            } else {
                scope.handle(Self::to_primitive(vm, heap, state, value, Hint::String)?)
            };
            let stringified = Convert::to_string(vm, heap, state, &scope, primitive)?;
            Ok(scope.handle(stringified).as_tagged(heap))
        })
    }

    pub fn numeric_op<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        a: Handle<'_, Value>,
        b: Handle<'_, Value>,
        op: fn(f64, f64) -> f64,
    ) -> Result<Tagged<'a, Value>> {
        // to_numeric runs user code (valueOf): both operands are rooted by
        // the caller's handles, so the sibling survives the first coercion.
        let a = Self::to_numeric(vm, heap, state, a)?;
        let b = Self::to_numeric(vm, heap, state, b)?;
        Ok(heap.new_number(op(a, b)))
    }

    pub fn to_property_key<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        v: Handle<'_, Value>,
    ) -> Result<Tagged<'a, SlotName>> {
        if let Some(Key::Element(i)) = v.as_tagged(heap).classify_key(heap) {
            return Ok(Tagged::from(Smi::new(i as i64)));
        }
        state.handle_scope(|scope| {
            if v.as_tagged(heap).get_as::<Symbol>(heap).is_some() {
                return Ok(v.as_tagged(heap).as_name());
            }
            let primitive = if v.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
                v
            } else {
                scope.handle(Self::to_primitive(vm, heap, state, v, Hint::String)?)
            };
            // ToPrimitive may itself yield a Symbol (ES 7.1.19 step 3).
            if primitive.as_tagged(heap).get_as::<Symbol>(heap).is_some() {
                return Ok(primitive.as_tagged(heap).as_name());
            }
            // Named lookup compares interned strings by pointer:
            // canonicalize exactly once here, then every downstream
            // bits-compare is sound.
            let stringified = Convert::to_string(vm, heap, state, &scope, primitive)?;
            // Root the word: the `Tagged<'a>` keeps the `&mut Heap` borrow
            // alive, which would conflict with the cast's shared map read.
            let stringified = scope.handle(stringified);
            let interned = match scope.cast::<DenseString>(heap, stringified.as_tagged(heap)) {
                Some(s) => vm.interner().intern_value(heap, &scope, &s),
                // ToString never yields a non-string.
                None => bail!(vm, heap, state, VmError::CannotConvertSymbolToString),
            };
            let word = interned.as_tagged(heap);
            // canonical index strings name the same property as their
            // numeric form: canonicalize to the Smi spelling exactly
            // once, so every downstream bits-compare is sound
            if let Some(Key::Element(i)) = word.erase().classify_key(heap) {
                return Ok(Tagged::from(Smi::new(i as i64)));
            }
            // fresh anchored re-read of the rooted interned word
            Ok(word.into())
        })
    }

    pub fn type_of<'a>(heap: &'a Heap, v: Tagged<'a, Value>) -> Tagged<'a, Value> {
        let known = heap.known();
        let s = &known.strings;
        // `null` needs no arm of its own: it is a non-callable object, and
        // so is anything that is not one of the primitive kinds above
        let type_name = if v.is_smi() || v.get_as::<Float>(heap).is_some() {
            s.number.get(heap)
        } else if v == known.undefined.get(heap) || v == known.the_hole.get(heap) {
            s.undefined.get(heap)
        } else if v == known.true_object.get(heap) || v == known.false_object.get(heap) {
            s.boolean.get(heap)
        } else if v.get_as::<DenseString>(heap).is_some() {
            s.string.get(heap)
        } else if v.get_as::<Symbol>(heap).is_some() {
            s.symbol.get(heap)
        } else if v.is_callable(heap) {
            s.function.get(heap)
        } else {
            s.object.get(heap)
        };
        type_name.erase()
    }

    pub fn instance_of(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        object: Handle<'_, Value>,
        callable: Handle<'_, Value>,
    ) -> Result<bool> {
        if !callable.as_tagged(heap).is_callable(heap) {
            bail!(vm, heap, state, VmError::InstanceofRightNotCallable);
        }
        // 4. P = Get(C, "prototype") — full [[Get]]; a getter may run user
        // code and move the heap, so only its result has to be rooted here
        state.handle_scope(|scope| {
            let proto = scope.handle(Object::get_property(
                vm,
                heap,
                state,
                callable,
                callable,
                heap.known()
                    .strings
                    .prototype
                    .get_handle(heap, &scope)
                    .erase(),
                false,
            )?);
            // 5. P must be an object
            if proto.as_tagged(heap).is_primitive(heap) {
                bail!(vm, heap, state, VmError::FunctionPrototypeNotObject);
            }
            Ok(Self::has_proto_in_chain(
                heap,
                object.as_tagged(heap),
                proto.as_tagged(heap),
            ))
        })
    }

    pub fn has_proto_in_chain<'a>(
        heap: &'a Heap,
        object: Tagged<'a, Value>,
        target: Tagged<'a, Value>,
    ) -> bool {
        let Some(obj) = object.as_heap_object() else {
            return false;
        };
        let proto = obj.prototype(heap);
        if proto.ptr_eq(target) {
            return true;
        }
        if proto == heap.known().null.get(heap) {
            return false;
        }
        if let Some(parents) = proto.get_as::<FixedArray>(heap) {
            for i in 0..parents.len() {
                if Self::has_proto_in_chain(heap, parents.at(heap, i), target) {
                    return true;
                }
            }
            return false;
        }
        Self::has_proto_in_chain(heap, proto, target)
    }

    /// `OrdinaryCreateFromConstructor ( newTarget, fallbackProto, slots )`
    /// [ES 10.1.14](https://tc39.es/ecma262/#sec-ordinarycreatefromconstructor)
    pub fn create_from_constructor<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        new_target: Handle<'_, Value>,
        map: Handle<'_, Map>,
        slots: HandleSlice<'_>,
    ) -> Result<Handle<'s, Object>> {
        let proto_name = heap
            .known()
            .strings
            .prototype
            .get_handle(heap, scope)
            .erase();
        let proto = scope.handle(Object::get_property(
            vm, heap, state, new_target, new_target, proto_name, false,
        )?);
        let obj = scope.handle(heap.new_object(scope, map, slots));
        // a non-object `newTarget.prototype` falls back to the map's own one
        if let Some(proto) = scope.cast::<Object>(heap, proto.as_tagged(heap)) {
            Object::set_prototype(vm, heap, state, scope, obj, proto.erase())?;
        }
        Ok(obj)
    }

    /// The forwarding tail of a `[[Construct]]` (proxy without a `construct`
    /// trap, bound functions): synthesize the receiver from `newTarget` — the
    /// hole for derived class constructors — run the target's `[[Construct]]`
    /// with `args`, and prefer an object result.
    pub fn construct_forward<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        target: Handle<'_, Value>,
        args: &[Handle<'_, Value>],
        new_target: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        let runtime = state.runtime(vm, heap);
        let is_bound = target
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|function| function.map(heap).kind().is_bound());
        if is_bound {
            let value = match HostCtx::enter_bound(
                vm,
                runtime,
                heap,
                state,
                target,
                args,
                Some(new_target),
            ) {
                Ok(value) => crate::extend_lifetime!(heap, value),
                Err(err) => raise_runtime(heap, err),
            };
            return Ok(value);
        }
        state.handle_scope(|scope| {
            let derived = target
                .as_tagged(heap)
                .as_heap_object()
                .filter(|obj| obj.map(heap).kind().is_class_constructor())
                .and_then(|obj| obj.callable_info(heap))
                .is_some_and(|info| info.function_kind().is_derived_class_constructor());
            let receiver = if derived {
                heap.known().the_hole.get_handle(heap, &scope).erase()
            } else {
                scope.handle(Self::create_construct_receiver_value(
                    vm, heap, state, new_target,
                )?)
            };
            let mut all: Vec<Tagged<'_, Value>> = Vec::with_capacity(args.len() + 1);
            all.push(receiver.as_tagged(heap));
            all.extend(args.iter().map(|arg| arg.as_tagged(heap)));
            let staged = scope.stage(&all);
            let result = scope.handle(HostCtx::enter(
                vm,
                runtime,
                heap,
                state,
                target,
                staged,
                Some(new_target),
            )?);
            if result.as_tagged(heap).is_primitive(heap) {
                if derived {
                    // a derived constructor may only return objects
                    bail!(
                        vm,
                        heap,
                        state,
                        VmError::DerivedConstructorReturnedNonObject
                    );
                }
                return Ok(receiver.as_tagged(heap));
            }
            Ok(result.as_tagged(heap))
        })
    }

    pub fn create_construct_receiver_value<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        new_target: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let map = heap.known().object_initial_map.get_handle(heap, &scope);
            let obj = Self::create_from_constructor(
                vm,
                heap,
                state,
                &scope,
                new_target,
                map,
                HandleSlice::EMPTY,
            )?;
            Ok(obj.as_tagged(heap).erase())
        })
    }
}

impl Object {
    /// The builtin tag of a `ToObject`ed receiver.
    /// [ES 20.1.3.6](https://tc39.es/ecma262/#sec-object.prototype.tostring)
    pub fn builtin_tag(heap: &Heap, value: Tagged<'_, Value>) -> Option<&'static str> {
        let Some(object) = value.as_heap_object() else {
            return Some("Object");
        };
        let kind = object.map(heap).kind();
        if kind.is_array() {
            return Some("Array");
        }
        if kind.is_proxy() && Self::is_array(heap, value)? {
            return Some("Array");
        }
        if kind.is_arguments() {
            return Some("Arguments");
        }
        if kind.is_callable() {
            return Some("Function");
        }
        if kind.is_error() {
            return Some("Error");
        }
        if kind.contains(MapKind::PRIMITIVE_WRAPPER) {
            let value = object.slots(heap).at(heap, 0);
            return Some(match WrapperKind::classify(heap, heap.known(), value) {
                Some(WrapperKind::Boolean) => "Boolean",
                Some(WrapperKind::Number) => "Number",
                Some(WrapperKind::String) => "String",
                None => "Object",
            });
        }
        if kind.contains(MapKind::DATE) {
            return Some("Date");
        }
        // TODO: step 13 ("RegExp") once RegExp objects exist.
        Some("Object")
    }
}
