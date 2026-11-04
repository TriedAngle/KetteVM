use core::alloc::Layout;

use crate::{
    AccessorPair, CallableInfoObject, Coercion, ContextObject, Convert, DETAILS_ACCESSOR,
    DETAILS_CONFIGURABLE, DETAILS_DATA, DETAILS_ENUMERABLE, DETAILS_WRITABLE, DenseString,
    EdgeVisitable, ElementsKind, EntryClass, FixedArray, Float, FunctionKind, GcSlot, Handle,
    HandleScope, HandleSlice, Header, Heap, HeapObject, Hint, HostCtx, Key, Map, NativeIndex,
    NumberDictionary, NumberDictionaryInit, ObjectKind, PropertyDescriptor, Prototype,
    SiblingChange, SlotName, Smi, Symbol, Tagged, ThreadState, Transition, VM, Value, Visitor,
    VmError,
};

/// What a set action did, for callers that only need to update caches.
pub enum SetApplied {
    Done,
    Transition,
    CallSetter,
    Rejected,
}

/// The elements slot is typed `FixedArray` but holds a
/// `NumberDictionary` in dictionary mode; the map's elements kind is
/// the authority for interpreting it.
///
/// # Safety
/// The word must be a NumberDictionary; only reachable while the
/// receiver's map says `ElementsKind::Dictionary`.
unsafe fn as_elements_word<'a>(dict: Tagged<'a, NumberDictionary>) -> Tagged<'a, FixedArray> {
    unsafe { Tagged::from_value_unchecked(dict.erase().raw()) }
}

#[repr(C)]
pub struct Object {
    pub header: Header,
    pub slots: GcSlot<FixedArray>,
    pub elements: GcSlot<FixedArray>,
    pub length: GcSlot<Smi>,
}

impl Object {
    pub fn layout_for() -> Layout {
        Layout::new::<Self>()
    }

    pub fn callable_info<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, CallableInfoObject>> {
        let kind = self.map(heap).kind();
        // TODO: this is not clean, we should separate callability from layout
        if !kind.is_callable() || kind.is_bound() {
            return None;
        }
        let info = self.slots(heap).at(heap, 0);
        // the callable map's slot 0 is always its CallableInfoObject
        Some(unsafe { info.cast() })
    }

    pub fn closure_context<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ContextObject>> {
        if !self.map(heap).kind().is_callable() {
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

    #[inline]
    pub fn runtime_index<'a>(&self, heap: &'a Heap) -> Option<usize> {
        if !self.map(heap).kind().is_native() {
            return None;
        }
        let idx = Smi::decode(self.slots(heap).at(heap, 0).raw())?.value();
        usize::try_from(idx).ok()
    }

    /// The NATIVE-kind slot-0 Smi decoded to a call target half: the
    /// value is the callee's registry runtime index.
    #[inline]
    pub fn runtime_call_target<'a>(&self, heap: &'a Heap) -> Option<CallTarget<'a>> {
        if !self.map(heap).kind().is_native() {
            return None;
        }
        let idx = Smi::decode(self.slots(heap).at(heap, 0).raw())?.value();
        Some(CallTarget::Native(NativeIndex(usize::try_from(idx).ok()?)))
    }

    #[inline(always)]
    pub fn is_array<'a>(&self, heap: &'a Heap) -> bool {
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
        if !self.is_array(heap) {
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

    /// The object's elements backing store word.
    #[inline(always)]
    pub fn elements<'a>(&self, heap: &'a Heap) -> Tagged<'a, FixedArray> {
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
        let elements = self.elements(heap);
        elements.is_strong_ptr().then_some(elements)
    }

    /// The sparse-elements backing store, when the receiver is in
    /// dictionary-elements mode.
    #[inline]
    pub fn element_dictionary<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, NumberDictionary>> {
        let kind = self.map(heap).kind();
        if !kind.is_js_receiver() || !kind.is_dictionary_elements() {
            return None;
        }
        let elements = self.elements(heap);
        if !elements.is_strong_ptr() {
            return None;
        }
        elements.erase().get_as::<NumberDictionary>(heap)
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
        if self.is_array(heap) {
            self.length()
        } else {
            self.elements(heap).len()
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
        let elements = self.elements(heap);
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
            if self.is_array(heap) {
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
        if self.is_array(heap) && i >= self.length() {
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
    ) -> Result<(), VmError> {
        let Some(obj) = self.as_heap_object() else {
            return Err(VmError::Type);
        };
        let kind = obj.map(heap).kind();
        if !kind.is_array() || !kind.is_dense_elements() || i >= obj.length() {
            return Err(VmError::OutOfBounds);
        }
        let elements = obj.elements_array(heap).ok_or(VmError::Type)?;
        if i >= elements.len() {
            return Err(VmError::OutOfBounds);
        }
        if kind.is_holey() && elements.at(heap, i) == heap.known().the_hole.get(heap) {
            return Err(VmError::OutOfBounds);
        }
        elements.set(heap, i, value);
        Prototype::element_mutated_kind(heap, obj, kind);
        Ok(())
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
        if kind.is_native() {
            return obj.runtime_call_target(heap);
        }
        // every non-runtime callable object is laid out with
        // `[callable info, context]` in its first two slots
        let slots = obj.slots(heap);
        let info = unsafe { slots.at(heap, 0).cast::<CallableInfoObject>() };
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

    #[inline]
    pub fn bytecode_target(self, heap: &'a Heap) -> Option<CallTarget<'a>> {
        let obj = self.as_heap_object()?;
        let slots = obj.slots(heap);
        let info = unsafe { slots.at(heap, 0).cast::<CallableInfoObject>() };
        let context = unsafe { slots.at(heap, 1).cast::<ContextObject>() };
        let descriptor = info.descriptor.to_smi_unchecked().value() as u64;
        Some(CallTarget::Bytecode {
            target: obj,
            info,
            context,
            kind: FunctionKind::decode(((descriptor >> 32) & 0xf) as i64),
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
    ) -> Result<(), VmError> {
        let new_len = i.checked_add(1).ok_or(VmError::OutOfBounds)?;

        let (grows, logical_len, capacity, dictionary, is_array) = {
            let obj = receiver.as_tagged(heap);
            let dictionary = obj.element_dictionary(heap).is_some();
            let capacity = obj.elements_array(heap).map(|e| e.len()).unwrap_or(0);
            // arrays track a logical length; other receivers end at
            // their elements capacity
            let is_array = obj.is_array(heap);
            let logical = if is_array { obj.length() } else { capacity };
            (i >= capacity, logical, capacity, dictionary, is_array)
        };

        if dictionary {
            return Self::store_dictionary_element(heap, scope, receiver, i, value);
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
            )?;
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
            let elements = obj.elements_array(heap).ok_or(VmError::Type)?;
            elements.set(heap, i, value.as_tagged(heap));
            // a store inside the physical capacity but past an array's
            // logical length still extends the array
            if is_array && i >= obj.length() {
                obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
            }
            Prototype::element_mutated(heap, obj);
        }
        Ok(())
    }

    fn grow_elements(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
        i: usize,
        value: &Handle<'_, Value>,
        new_len: usize,
        logical_len: usize,
        is_array: bool,
    ) -> Result<(), VmError> {
        let obj = receiver.as_tagged(heap);
        let elements = obj.elements_array(heap).ok_or(VmError::Type)?;
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
            return Self::store_dictionary_element(heap, scope, receiver, i, value);
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
            let old = obj.elements_array(heap).ok_or(VmError::Type)?;
            let new = elements.as_tagged(heap);
            for k in 0..keep {
                new.set(heap, k, old.at(heap, k));
            }
            new.set(heap, i, value.as_tagged(heap));
        }
        let obj = receiver.as_tagged(heap);
        obj.elements
            .set(heap, obj.erase(), elements.as_tagged(heap));
        if is_array && new_len > logical_len {
            obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
        }
        Prototype::element_mutated(heap, obj);
        Ok(())
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
    ) -> Result<(), VmError> {
        let new_len = i.checked_add(1).ok_or(VmError::OutOfBounds)?;
        let (dict, length, is_array) = {
            let obj = receiver.as_tagged(heap);
            let dict = obj.element_dictionary(heap).ok_or(VmError::Type)?;
            let is_array = obj.is_array(heap);
            (scope.handle(dict), obj.length(), is_array)
        };
        match dict.as_tagged(heap).classify(heap, i) {
            EntryClass::ReadOnly | EntryClass::Accessor => return Ok(()),
            EntryClass::Data | EntryClass::Absent => {}
        }
        if matches!(dict.as_tagged(heap).classify(heap, i), EntryClass::Absent) {
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
                return Self::store_array_element(heap, scope, receiver, i, value);
            }
        }
        let value_word = value.as_tagged(heap).raw();
        let table = NumberDictionary::insert(
            heap,
            scope,
            &dict,
            i,
            unsafe { Tagged::from_value_unchecked(value_word) },
            DETAILS_DATA,
        );
        let obj = receiver.as_tagged(heap);
        if table.as_tagged(heap) != dict.as_tagged(heap) {
            obj.elements.set(heap, obj.erase(), unsafe {
                as_elements_word(table.as_tagged(heap))
            });
        }
        if is_array && i >= length {
            obj.length.set(heap, obj.erase(), Smi::new(new_len as i64));
        }
        Prototype::element_mutated(heap, obj);
        Ok(())
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
    ) -> Result<bool, VmError> {
        let is_array = receiver.as_tagged(heap).is_array(heap);
        if let PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        } = desc
        {
            Self::store_array_element(heap, scope, &receiver, i, &value)?;
            return Ok(true);
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
                .ok_or(VmError::Type)?,
        );
        let (value_word, details, accessor) = match desc {
            PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } => (
                value.as_tagged(heap).raw(),
                DETAILS_WRITABLE * writable as i64
                    | DETAILS_ENUMERABLE * enumerable as i64
                    | DETAILS_CONFIGURABLE * configurable as i64,
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
                    pair.as_tagged(heap).raw(),
                    DETAILS_ACCESSOR
                        | DETAILS_ENUMERABLE * enumerable as i64
                        | DETAILS_CONFIGURABLE * configurable as i64,
                    true,
                )
            }
        };
        let table = NumberDictionary::insert(
            heap,
            scope,
            &dict,
            i,
            // Safety: fresh word, no allocation since the read.
            unsafe { Tagged::from_value_unchecked(value_word) },
            details,
        );
        if accessor {
            table.as_tagged(heap).require_slow(heap);
        }
        let obj = receiver.as_tagged(heap);
        let host = obj.erase();
        obj.elements.set(
            heap,
            host,
            // Safety: dictionary mode was established above.
            unsafe { Tagged::from_value_unchecked(table.as_tagged(heap).erase().raw()) },
        );
        if is_array && i >= obj.length() {
            obj.length.set(heap, host, Smi::new((i + 1) as i64));
        }
        Prototype::element_mutated(heap, obj);
        Ok(true)
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
    ) -> Result<bool, VmError> {
        if !receiver.as_tagged(heap).is_array(heap) {
            return Err(VmError::Type);
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
                let mut effective = new_len;
                let mut doomed: Vec<usize> = Vec::new();
                dict.as_ref().for_each_entry(heap, |k, _, details| {
                    if k >= new_len {
                        if details & DETAILS_CONFIGURABLE == 0 {
                            effective = effective.max(k + 1);
                        } else {
                            doomed.push(k);
                        }
                    }
                });
                for k in doomed {
                    dict.as_ref().delete(heap, k);
                }
                Prototype::element_mutated(heap, receiver.as_tagged(heap));
                let obj = receiver.as_tagged(heap);
                obj.as_ref()
                    .length
                    .set(heap, obj.erase(), Smi::new(effective as i64));
                return Ok(effective == new_len);
            }
            let obj = receiver.as_tagged(heap);
            obj.as_ref()
                .length
                .set(heap, obj.erase(), Smi::new(new_len as i64));
            return Ok(true);
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
        Ok(true)
    }

    /// Clear [[Writable]] on the array `length` native accessor, keeping its
    /// getter/setter.
    pub fn make_array_length_readonly(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: &Handle<'_, Object>,
    ) {
        let name = scope.handle(heap.known().strings.length.get(heap));
        let map = receiver.as_tagged(heap).map(heap);
        let Some((index, descriptor)) = map
            .descriptors()
            .iter()
            .enumerate()
            .find(|(_, d)| d.name(heap).ptr_eq(name.as_tagged(heap)))
        else {
            return;
        };
        let flags = descriptor.flags().without_writable();
        if flags == descriptor.flags() {
            return;
        }
        Transition::replace_native_accessor_flags(heap, scope, *receiver, name, index, flags);
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
            let word = v.as_tagged(heap).raw();
            dict = NumberDictionary::insert(
                heap,
                scope,
                &dict,
                *k,
                unsafe { Tagged::from_value_unchecked(word) },
                DETAILS_DATA,
            );
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
        obj.elements.set(heap, obj.erase(), unsafe {
            as_elements_word(dict.as_tagged(heap))
        });
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
            .set(heap, obj.erase(), elements.as_tagged(heap));
        obj.header
            .map
            .set(heap, obj.erase(), target.as_tagged(heap));
    }
}

pub struct ObjectInit<'a> {
    pub map: Handle<'a, Map>,
    pub slots: Handle<'a, FixedArray>,
    pub elements: Handle<'a, FixedArray>,
    pub length: usize,
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
        self.elements.set(heap, host, elements);
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
    ) -> Result<Tagged<'a, Object>, VmError> {
        let info_ref = info.as_tagged(heap);
        let kind = info_ref.function_kind();
        let formal_length = info_ref.formal_length();
        let function_name = match info_ref.name(heap) {
            Some(name) => scope.handle(name),
            None => scope.handle(heap.known().strings.empty.get(heap).erase()),
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

        let length_key = scope.handle(heap.known().strings.length.get(heap));
        let name_key = scope.handle(heap.known().strings.name.get(heap));
        if !Object::define_own_property(
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
        )? {
            return Err(VmError::Type);
        }
        if !Object::define_own_property(
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
        )? {
            return Err(VmError::Type);
        }

        if kind.needs_prototype() {
            let proto_map = scope.handle(heap.known().object_initial_map.get(heap));
            let proto = heap
                .new_object(scope, proto_map, HandleSlice::EMPTY)
                .as_handle(scope);
            let constructor = scope.handle(heap.known().strings.constructor.get(heap));
            let prototype = scope.handle(heap.known().strings.prototype.get(heap));
            if !Object::define_own_property(
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
            )? {
                return Err(VmError::Type);
            }
            if !Object::define_own_property(
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
            )? {
                return Err(VmError::Type);
            }
        }

        Ok(function.as_tagged(heap))
    }

    pub fn to_primitive<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        value: Handle<'_, Value>,
        hint: Hint,
    ) -> Result<Coercion<'a>, VmError> {
        if value.as_tagged(heap).is_primitive(heap) {
            return Ok(Coercion::Value(value.as_tagged(heap)));
        }
        let runtime = state.runtime(vm, heap);
        HostCtx::new(vm, runtime, heap, state).handle_scope(|vm, heap, state, scope| {
            // 1. exotic @@toPrimitive (GetMethod)
            let exotic = match Object::get_property(
                vm,
                heap,
                state,
                value,
                value,
                scope.handle(heap.known().to_primitive_symbol.get(heap).erase()),
                false,
            )? {
                Coercion::Threw => return Ok(Coercion::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            if exotic.as_tagged(heap) != heap.known().undefined.get(heap)
                && exotic.as_tagged(heap) != heap.known().null.get(heap)
            {
                if !exotic.as_tagged(heap).is_callable(heap) {
                    // GetMethod: a non-callable, non-nullish method is a TypeError
                    return Err(VmError::Type);
                }
                let s = &heap.known().strings;
                let hint_string = scope.handle(match hint {
                    Hint::Default => s.default.get(heap).erase(),
                    Hint::Number => s.number.get(heap).erase(),
                    Hint::String => s.string.get(heap).erase(),
                });
                let args = scope.stage(&[hint_string.as_tagged(heap)]);
                let result = scope.handle(HostCtx::enter(
                    vm, runtime, &mut *heap, state, exotic, args, None,
                )?);
                if result.as_tagged(heap) == heap.known().exception.get(heap) {
                    return Ok(Coercion::Threw);
                }
                return if result.as_tagged(heap).is_primitive(heap) {
                    Ok(Coercion::Value(result.as_tagged(heap)))
                } else {
                    Err(VmError::Type)
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
                let method = match Object::get_property(vm, heap, state, value, value, name, false)?
                {
                    Coercion::Threw => return Ok(Coercion::Threw),
                    Coercion::Value(v) => scope.handle(v),
                };
                if !method.as_tagged(heap).is_callable(heap) {
                    continue;
                }
                let args = scope.stage(&[value.as_tagged(heap)]);
                let result = scope.handle(HostCtx::enter(
                    vm, runtime, &mut *heap, state, method, args, None,
                )?);
                if result.as_tagged(heap) == heap.known().exception.get(heap) {
                    return Ok(Coercion::Threw);
                }
                if result.as_tagged(heap).is_primitive(heap) {
                    return Ok(Coercion::Value(result.as_tagged(heap)));
                }
                // object result: try the next method name
            }
            Err(VmError::Type)
        })
    }

    pub fn to_numeric(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<Option<f64>, VmError> {
        state.handle_scope(|scope| {
            match Self::to_primitive(vm, heap, state, value, Hint::Number)? {
                Coercion::Threw => Ok(None),
                // root the anchored result to release the heap borrow
                Coercion::Value(v) => {
                    let v = scope.handle(v);
                    Ok(Some(v.as_tagged(heap).to_number(heap)?))
                }
            }
        })
    }

    pub fn to_string<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<Option<Tagged<'a, Value>>, VmError> {
        state.handle_scope(|scope| {
            let primitive = if value.as_tagged(heap).is_primitive(heap) {
                value
            } else {
                match Self::to_primitive(vm, heap, state, value, Hint::String)? {
                    Coercion::Threw => return Ok(None),
                    Coercion::Value(v) => scope.handle(v),
                }
            };
            let s = scope.handle(Convert::to_string(heap, &scope, primitive)?);
            Ok(Some(s.as_tagged(heap)))
        })
    }

    pub fn numeric_op<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        a: Handle<'_, Value>,
        b: Handle<'_, Value>,
        op: fn(f64, f64) -> f64,
    ) -> Result<Option<Tagged<'a, Value>>, VmError> {
        // to_numeric runs user code (valueOf): both operands are rooted by
        // the caller's handles, so the sibling survives the first coercion.
        let Some(a) = Self::to_numeric(vm, heap, state, a)? else {
            return Ok(None);
        };
        let Some(b) = Self::to_numeric(vm, heap, state, b)? else {
            return Ok(None);
        };
        Ok(Some(heap.new_number(op(a, b))))
    }

    pub fn to_property_key<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        v: Handle<'_, Value>,
    ) -> Result<Option<Tagged<'a, SlotName>>, VmError> {
        // canonical indices keep the Smi spelling; Smis are
        // pointer-free, valid at any lifetime, no rooting needed
        if let Ok(Key::Element(i)) = v.as_tagged(heap).classify_key(heap) {
            return Ok(Some(Tagged::from(Smi::new(i as i64))));
        }
        state.handle_scope(|scope| {
            let cond_5 = v.as_tagged(heap).get_as::<Symbol>(heap).is_some();
            if cond_5 {
                return Ok(Some(v.as_tagged(heap).as_name()));
            }
            let is_string = v.as_tagged(heap).get_as::<DenseString>(heap).is_some();
            let primitive: Handle<'_, Value> = if is_string {
                v
            } else {
                match Self::to_primitive(vm, heap, state, v, Hint::String)? {
                    Coercion::Threw => return Ok(None),
                    Coercion::Value(p) => scope.handle(p),
                }
            };
            // Named lookup compares interned strings by pointer:
            // canonicalize exactly once here, then every downstream
            // bits-compare is sound.
            let stringified = Convert::to_string(heap, &scope, primitive)?;
            // Root the word: the `Tagged<'a>` keeps the `&mut Heap` borrow
            // alive, which would conflict with the cast's shared map read.
            let stringified = scope.handle(stringified);
            let interned = match scope.cast::<DenseString>(heap, stringified.as_tagged(heap)) {
                Some(s) => vm.interner().intern_value(heap, &scope, &s),
                // ToString of a symbol primitive throws (ES 6.1.7.1)
                None => return Err(VmError::Type),
            };
            let word = interned.as_tagged(heap);
            // canonical index strings name the same property as their
            // numeric form: canonicalize to the Smi spelling exactly
            // once, so every downstream bits-compare is sound
            if let Ok(Key::Element(i)) = word.erase().classify_key(heap) {
                return Ok(Some(Tagged::from(Smi::new(i as i64))));
            }
            // fresh anchored re-read of the rooted interned word
            Ok(Some(word.into()))
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
    ) -> Result<Option<bool>, VmError> {
        if !callable.as_tagged(heap).is_callable(heap) {
            return Err(VmError::Type);
        }
        // 4. P = Get(C, "prototype") — full [[Get]]; a getter may run user
        // code and move the heap, so only its result has to be rooted here
        state.handle_scope(|scope| {
            let proto = match Object::get_property(
                vm,
                heap,
                state,
                callable,
                callable,
                scope.handle(heap.known().strings.prototype.get(heap).erase()),
                false,
            )? {
                Coercion::Threw => return Ok(None),
                Coercion::Value(v) => scope.handle(v),
            };
            // 5. P must be an object
            if proto.as_tagged(heap).is_primitive(heap) {
                return Err(VmError::Type);
            }
            Ok(Some(Self::has_proto_in_chain(
                heap,
                object.as_tagged(heap),
                proto.as_tagged(heap),
            )))
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
    /// `Ok(None)` means the `prototype` lookup threw.
    pub fn create_from_constructor<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        new_target: Handle<'_, Value>,
        map: Handle<'_, Map>,
        slots: HandleSlice<'_>,
    ) -> Result<Option<Handle<'s, Object>>, VmError> {
        let proto_name = scope.handle(heap.known().strings.prototype.get(heap).erase());
        let proto =
            Object::get_property(vm, heap, state, new_target, new_target, proto_name, false)?;
        let proto = match proto {
            Coercion::Threw => return Ok(None),
            Coercion::Value(v) => scope.handle(v),
        };
        let obj = scope.handle(heap.new_object(scope, map, slots));
        // a non-object `newTarget.prototype` falls back to the map's own one
        if let Some(proto) = scope.cast::<Object>(heap, proto.as_tagged(heap)) {
            Object::set_prototype(heap, scope, obj, proto.erase())?;
        }
        Ok(Some(obj))
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
    ) -> Result<Coercion<'a>, VmError> {
        let runtime = state.runtime(vm, heap);
        let is_bound = target
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|function| function.map(heap).kind().is_bound());
        if is_bound {
            return state.handle_scope(|scope| {
                let value = scope.handle(HostCtx::enter_bound(
                    vm,
                    runtime,
                    heap,
                    state,
                    target,
                    args,
                    Some(new_target),
                )?);
                Ok(
                    if value.as_tagged(heap) == heap.known().exception.get(heap) {
                        Coercion::Threw
                    } else {
                        Coercion::Value(value.as_tagged(heap))
                    },
                )
            });
        }
        state.handle_scope(|scope| {
            let derived = target
                .as_tagged(heap)
                .as_heap_object()
                .filter(|obj| obj.map(heap).kind().is_class_constructor())
                .and_then(|obj| obj.callable_info(heap))
                .is_some_and(|info| info.function_kind().is_derived_class_constructor());
            let receiver = if derived {
                scope.handle(heap.known().the_hole.get(heap).erase())
            } else {
                let Some(receiver) =
                    Self::create_construct_receiver_value(vm, heap, state, new_target)?
                else {
                    return Ok(Coercion::Threw);
                };
                scope.handle(receiver)
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
            if result.as_tagged(heap) == heap.known().exception.get(heap) {
                return Ok(Coercion::Threw);
            }
            if result.as_tagged(heap).is_primitive(heap) {
                if derived {
                    // a derived constructor may only return objects
                    return Err(VmError::Type);
                }
                return Ok(Coercion::Value(receiver.as_tagged(heap)));
            }
            Ok(Coercion::Value(result.as_tagged(heap)))
        })
    }

    pub fn create_construct_receiver_value<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        new_target: Handle<'_, Value>,
    ) -> Result<Option<Tagged<'a, Value>>, VmError> {
        state.handle_scope(|scope| {
            let map = scope.handle(heap.known().object_initial_map.get(heap));
            let obj = Self::create_from_constructor(
                vm,
                heap,
                state,
                &scope,
                new_target,
                map,
                HandleSlice::EMPTY,
            )?;
            Ok(obj.map(|obj| obj.as_tagged(heap).erase()))
        })
    }
}
