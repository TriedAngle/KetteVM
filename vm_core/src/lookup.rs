use crate::errors::Errors;
use crate::proxy::{Flow, Proxy};
use crate::{
    AccessorInfo, AccessorPair, Coercion, Convert, DETAILS_ACCESSOR, DETAILS_CONFIGURABLE,
    DETAILS_ENUMERABLE, DETAILS_WRITABLE, DenseString, FixedArray, Handle, HandleScope, Heap,
    HostCtx, Key, LanguageMode, NativeIndex, Object, PartialDescriptor, PropertyDescriptor,
    SetApplied, SlotFlags, SlotName, Smi, StoreSemantics, StringOwn, Tagged, ThreadState, VM,
    Value, VmError, raise_runtime,
};

/// One resolved position of the walk: where the property lives and
/// what it holds. `Miss` is terminal (chain exhausted).
pub enum Hit<'h> {
    /// Chain (or own lookup) exhausted.
    Miss,
    /// Named data property in a map descriptor.
    Data {
        holder: Tagged<'h, Object>,
        offset: usize,
        value: Tagged<'h, Value>,
        flags: SlotFlags,
    },
    Accessor {
        holder: Tagged<'h, Object>,
        pair: Tagged<'h, AccessorPair>,
        flags: SlotFlags,
    },
    NativeAccessor {
        holder: Tagged<'h, Object>,
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
            Ok(Key::Element(i)) => Some(i),
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

        for d in o.map(heap).descriptors() {
            if !d.name(heap).ptr_eq(name) {
                continue;
            }
            if d.flags().is_accessor() {
                let value = d.value.get(heap);
                if let Some(pair) = value.get_as::<AccessorPair>(heap) {
                    return Some(Hit::Accessor {
                        holder: o,
                        pair,
                        flags: d.flags(),
                    });
                }
                return Some(Hit::NativeAccessor {
                    holder: o,
                    info: value
                        .get_as::<AccessorInfo>(heap)
                        .expect("accessor descriptor holds a pair or accessor info"),
                    flags: d.flags(),
                });
            }
            let offset = d.offset();
            return Some(Hit::Data {
                holder: o,
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

impl Object {
    /// `[[Get]] ( propertyKey, receiver )`
    /// [ES 10.1.8](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-get-p-receiver)
    /// [ES 10.5.8](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-get-p-receiver)
    /// Reads a property, running getters and proxy traps. `throws_on_miss`
    /// raises a ReferenceError for contextual loads.
    pub fn get_property<'h>(
        vm: &'h VM,
        heap: &'h mut Heap,
        state: &'h ThreadState,
        holder: Handle<'_, Value>,
        receiver: Handle<'_, Value>,
        name: Handle<'_, Value>,
        throws_on_miss: bool,
    ) -> Result<Coercion<'h>, VmError> {
        state.handle_scope(|scope| {
            if holder.as_tagged(heap).is_nullish(heap) {
                return Err(VmError::Type);
            }
            let start = scope.handle(holder.as_tagged(heap).erase());
            let mut it = Lookup::new(heap, &scope, start, name, false);
            let hit = it.next(heap, &scope);
            match hit {
                Hit::Miss => {
                    if throws_on_miss {
                        let text = name
                            .as_tagged(heap)
                            .erase()
                            .get_as::<DenseString>(heap)
                            .map(|s| s.to_rust_string(heap))
                            .unwrap_or_default();
                        let ex = Errors::not_defined(vm, heap, state, &text)
                            .expect("error materialization must not fail");
                        state.set_pending_exception(ex);
                        return Ok(Coercion::Threw);
                    }
                    Ok(Coercion::Value(heap.known().undefined.get(heap).erase()))
                }
                Hit::Proxy(proxy) => {
                    let proxy = scope.handle(proxy);
                    Proxy::get(vm, heap, state, proxy, receiver, name)
                }
                Hit::Data { value, .. }
                | Hit::Element { value, .. }
                | Hit::Parent { value, .. } => {
                    let value = scope.handle(value);
                    Ok(Coercion::Value(value.as_tagged(heap)))
                }
                Hit::StringOwn { holder, own } => {
                    let value = match own {
                        StringOwn::Length(len) => scope.handle(Smi::new(len as i64).into_tagged()),
                        StringOwn::Index(i) => {
                            let s = DenseString::from_receiver(heap, holder)
                                .expect("string own key from a string receiver");
                            let unit = s.code_unit(heap, i);
                            let unit = DenseString::from_units(heap, &scope, &[unit]);
                            scope.handle(unit.as_tagged(heap).erase())
                        }
                    };
                    Ok(Coercion::Value(value.as_tagged(heap)))
                }
                Hit::Accessor { pair, .. } => {
                    let undefined = scope.handle(heap.known().undefined.get(heap).erase());
                    let getter = scope.handle(pair.get.get(heap));
                    if getter.as_tagged(heap) == undefined.as_tagged(heap) {
                        return Ok(Coercion::Value(undefined.as_tagged(heap)));
                    }
                    let args = scope.stage(&[receiver.as_tagged(heap).erase()]);
                    let result = scope.handle(HostCtx::enter(vm, heap, state, getter, args, None)?);
                    if result.as_tagged(heap) == heap.known().exception.get(heap) {
                        Ok(Coercion::Threw)
                    } else {
                        Ok(Coercion::Value(result.as_tagged(heap)))
                    }
                }
                Hit::NativeAccessor { holder, info, .. } => {
                    let getter = info.getter();
                    let holder = scope.handle(holder.erase());
                    native_accessor_get(vm, heap, state, &scope, getter, holder)
                }
            }
        })
    }

    /// Data value behind a hit, never invoking getters or traps.
    pub fn get_data_property<'a>(
        heap: &'a Heap,
        scope: &HandleScope<'_>,
        start: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Tagged<'a, Value> {
        let hit = Lookup::new(heap, scope, start, key, false).next(heap, scope);
        match hit {
            Hit::Data { value, .. } | Hit::Element { value, .. } | Hit::Parent { value, .. } => {
                value
            }
            Hit::NativeAccessor { holder, .. } => holder
                .array_length(heap, key.as_tagged(heap).as_name())
                .unwrap_or_else(|| heap.known().undefined.get(heap).erase()),
            Hit::Accessor { .. } | Hit::Proxy(_) | Hit::StringOwn { .. } | Hit::Miss => {
                heap.known().undefined.get(heap).erase()
            }
        }
    }

    /// `[[Set]] ( propertyKey, value, receiver )`
    /// [ES 10.1.9](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-set-p-v-receiver)
    /// [ES 10.5.9](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-set-p-v-receiver)
    /// [ES 7.3.4](https://tc39.es/ecma262/#sec-set-o-p-v-throw)
    /// Writes a property, running setters and proxy traps. Strict rejection
    /// raises as `Set(..., true)`.
    /// `Shadow` defines inherited hits on the receiver; `WriteThrough` writes
    /// the inherited slot. `contextual` marks identifier stores, so a strict
    /// global miss raises ReferenceError.
    #[allow(clippy::too_many_arguments)]
    pub fn set_property(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        start: Handle<'_, Value>,
        receiver: Handle<'_, Value>,
        key: Handle<'_, Value>,
        value: Handle<'_, Value>,
        semantics: StoreSemantics,
        contextual: bool,
        mode: LanguageMode,
    ) -> Result<Flow<SetApplied>, VmError> {
        state.handle_scope(|scope| {
            if receiver.as_tagged(heap).is_nullish(heap) {
                return Err(VmError::Type);
            }
            let outcome = if !Proxy::is_js_receiver(heap, receiver.as_tagged(heap)) {
                // Primitive receivers are not property-store targets
                Ok(Flow::Value(SetApplied::Rejected))
            } else {
                let hit = {
                    let start = scope.handle(start.as_tagged(heap).erase());
                    let mut it = Lookup::new(heap, &scope, start, key, false);
                    it.next(heap, &scope)
                };
                match hit {
                    Hit::Proxy(proxy) => {
                        let proxy = scope.handle(proxy);
                        match Proxy::set(vm, heap, state, proxy, key, value, receiver)? {
                            Coercion::Threw => Ok(Flow::Threw),
                            Coercion::Value(v) => {
                                let v = scope.handle(v);
                                if v.as_tagged(heap).is_truthy(heap) {
                                    Ok(Flow::Value(SetApplied::Done))
                                } else {
                                    Ok(Flow::Value(SetApplied::Rejected))
                                }
                            }
                        }
                    }
                    Hit::StringOwn { .. } => Ok(Flow::Value(SetApplied::Rejected)),
                    Hit::Accessor { pair, .. } => {
                        let undefined = heap.known().undefined.get(heap).erase();
                        let setter = scope.handle(pair.set.get(heap));
                        if setter.as_tagged(heap) == undefined {
                            Ok(Flow::Value(SetApplied::Rejected))
                        } else {
                            call_setter(vm, heap, state, &scope, receiver, value, setter)
                        }
                    }
                    Hit::NativeAccessor {
                        holder,
                        info,
                        flags,
                        ..
                    } => {
                        if !holder.erase().ptr_eq(receiver.as_tagged(heap)) {
                            // Native accessors present as data properties:
                            // an inherited write defines on the receiver.
                            define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                        } else if !flags.is_writable() {
                            Ok(Flow::Value(SetApplied::Rejected))
                        } else {
                            let setter = info.setter();
                            native_accessor_set(vm, heap, state, &scope, setter, receiver, value)
                        }
                    }
                    Hit::Data {
                        holder,
                        offset,
                        flags,
                        ..
                    } => {
                        if !flags.is_writable() {
                            Ok(Flow::Value(SetApplied::Rejected))
                        } else {
                            let inherited = !holder.erase().ptr_eq(receiver.as_tagged(heap));
                            if inherited && semantics == StoreSemantics::Shadow {
                                define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                            } else {
                                let holder = scope.handle(holder);
                                let host = holder.as_tagged(heap).erase();
                                holder.as_tagged(heap).slot(heap, offset).set(
                                    heap,
                                    host,
                                    value.as_tagged(heap),
                                );
                                Ok(Flow::Value(SetApplied::Done))
                            }
                        }
                    }
                    Hit::Element {
                        holder,
                        index,
                        flags,
                        ..
                    } => {
                        if !flags.is_writable() {
                            Ok(Flow::Value(SetApplied::Rejected))
                        } else {
                            let inherited = !holder.erase().ptr_eq(receiver.as_tagged(heap));
                            if inherited && semantics == StoreSemantics::Shadow {
                                define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                            } else {
                                let holder = scope.handle(holder);
                                Object::store_array_element(heap, &scope, &holder, index, &value)?;
                                Ok(Flow::Value(SetApplied::Done))
                            }
                        }
                    }
                    Hit::Parent { holder, slot, .. } => {
                        if holder.erase().ptr_eq(receiver.as_tagged(heap)) {
                            let holder = scope.handle(holder);
                            let pairs = holder
                                .as_tagged(heap)
                                .map(heap)
                                .prototype
                                .get(heap)
                                .get_as::<FixedArray>(heap)
                                .expect("parent state implies a parent list");
                            pairs.as_ref().element_slot(slot).set(
                                heap,
                                pairs.erase(),
                                value.as_tagged(heap),
                            );
                            Ok(Flow::Value(SetApplied::Done))
                        } else {
                            define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                        }
                    }
                    Hit::Miss => {
                        if contextual
                            && mode.is_strict()
                            && receiver
                                .as_tagged(heap)
                                .ptr_eq(vm.known().global_object.get(heap).erase())
                        {
                            raise_runtime(vm, heap, state, VmError::Reference);
                            return Ok(Flow::Threw);
                        }
                        define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                    }
                }
            };
            match outcome {
                Ok(Flow::Value(SetApplied::Rejected)) if mode.is_strict() => {
                    raise_runtime(vm, heap, state, VmError::Type);
                    Ok(Flow::Threw)
                }
                other => other,
            }
        })
    }

    /// `[[HasProperty]] ( propertyKey )`
    /// [ES 10.1.7](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-hasproperty-p)
    /// [ES 10.5.7](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-hasproperty-p)
    /// Reports property existence; proxy traps run, accessors never do.
    pub fn has_property<'h>(
        vm: &VM,
        heap: &'h mut Heap,
        state: &ThreadState,
        start: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Result<Coercion<'h>, VmError> {
        state.handle_scope(|scope| {
            if start.as_tagged(heap).is_nullish(heap) {
                return Err(VmError::Type);
            }
            let hit = Lookup::new(heap, &scope, start, key, false).next(heap, &scope);
            match hit {
                Hit::Proxy(proxy) => {
                    let proxy = scope.handle(proxy);
                    Proxy::has(vm, heap, state, proxy, key)
                }
                Hit::Miss => Ok(Coercion::Value(Convert::boolean(heap, false))),
                _ => Ok(Coercion::Value(Convert::boolean(heap, true))),
            }
        })
    }

    /// LengthOfArrayLike (ES 7.3.18) with fast paths for arrays and strings.
    pub fn length_of_array_like<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        scope: &HandleScope<'_>,
        obj: Handle<'_, Value>,
    ) -> Result<Option<u64>, VmError> {
        if let Some(o) = obj.as_tagged(heap).as_heap_object()
            && o.as_ref().is_array(heap)
        {
            return Ok(Some(o.as_ref().length() as u64));
        }
        if let Some(s) = DenseString::from_receiver(heap, obj.as_tagged(heap)) {
            return Ok(Some(s.len() as u64));
        }
        // TODO: typed-array fast path once [[TypedArrayName]] exists.
        let length_key = scope.handle(vm.known().strings.length.get(heap).erase());
        let length_value = match Self::get_property(vm, heap, state, obj, obj, length_key, false) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => return Ok(None),
            Err(err) => return Err(err),
        };
        let length = match Self::to_numeric(vm, heap, state, length_value) {
            // TODO: handle the 2^53 max length/key.
            Ok(Some(n)) => n as u64,
            Ok(None) => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(length))
    }

    /// `[[Delete]] ( propertyKey )`
    /// [ES 10.1.10](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-delete-p)
    /// [ES 10.5.10](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-delete-p)
    /// Deletes a property; proxy traps run and non-configurable keys are refused.
    pub fn delete_property(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        target: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Result<Flow<bool>, VmError> {
        state.handle_scope(|scope| {
            if target.as_tagged(heap).is_nullish(heap) {
                return Err(VmError::Type);
            }
            if Proxy::is_proxy(heap, target.as_tagged(heap)) {
                return match Proxy::delete(vm, heap, state, target, key)? {
                    Coercion::Threw => Ok(Flow::Threw),
                    Coercion::Value(v) => {
                        let v = scope.handle(v);
                        Ok(Flow::Value(v.as_tagged(heap).is_truthy(heap)))
                    }
                };
            }
            // String exotic own properties ("length"/indices) are
            // non-configurable, for primitives and String wrappers alike
            // (ES 10.4.3.4); every other primitive property deletes as absent.
            if let Some(s) = DenseString::from_receiver(heap, target.as_tagged(heap))
                && s.own_key(heap, key.as_tagged(heap).as_name()).is_some()
            {
                return Ok(Flow::Value(false));
            }
            if target.as_tagged(heap).is_primitive(heap) {
                return Ok(Flow::Value(true));
            }
            let receiver = scope
                .cast::<Object>(heap, target.as_tagged(heap))
                .expect("non-primitive receivers are objects");
            Ok(Flow::Value(Self::delete_own_property(
                heap, &scope, receiver, key,
            )?))
        })
    }

    /// `[[GetOwnProperty]]` as a raw descriptor: side-effect free.
    pub fn ordinary_own_descriptor<'s>(
        heap: &Heap,
        scope: &'s HandleScope<'_>,
        receiver: Tagged<'_, Value>,
        key: Tagged<'_, Value>,
    ) -> Option<PropertyDescriptor<'s>> {
        let hit = Lookup::new(heap, scope, scope.handle(receiver), scope.handle(key), true)
            .next(heap, scope);
        match hit {
            Hit::Data { value, flags, .. } => Some(PropertyDescriptor::Data {
                value: scope.handle(value),
                writable: flags.is_writable(),
                enumerable: flags.is_enumerable(),
                configurable: flags.is_configurable(),
            }),
            Hit::Accessor { pair, flags, .. } => Some(PropertyDescriptor::Accessor {
                get: scope.handle(pair.get.get(heap)),
                set: scope.handle(pair.set.get(heap)),
                enumerable: flags.is_enumerable(),
                configurable: flags.is_configurable(),
            }),
            // Native accessors over an internal slot present as data.
            // Only the array-length field accessor exists today.
            Hit::NativeAccessor { holder, flags, .. } => holder
                .array_length(heap, key.as_name())
                .map(|value| PropertyDescriptor::Data {
                    value: scope.handle(value),
                    writable: flags.is_writable(),
                    enumerable: flags.is_enumerable(),
                    configurable: flags.is_configurable(),
                }),
            Hit::Element { value, flags, .. } => Some(PropertyDescriptor::Data {
                value: scope.handle(value),
                writable: flags.is_writable(),
                enumerable: flags.is_enumerable(),
                configurable: flags.is_configurable(),
            }),
            Hit::Parent { value, .. } => Some(PropertyDescriptor::Data {
                value: scope.handle(value),
                writable: false,
                enumerable: false,
                configurable: false,
            }),
            Hit::StringOwn { .. } | Hit::Proxy(_) | Hit::Miss => None,
        }
    }
}

fn call_setter(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    value: Handle<'_, Value>,
    setter: Handle<'_, Value>,
) -> Result<Flow<SetApplied>, VmError> {
    let args = scope.stage(&[receiver.as_tagged(heap).erase(), value.as_tagged(heap)]);
    let result = scope.handle(HostCtx::enter(vm, heap, state, setter, args, None)?);
    if result.as_tagged(heap) == heap.known().exception.get(heap) {
        Ok(Flow::Threw)
    } else {
        Ok(Flow::Value(SetApplied::CallSetter))
    }
}

fn native_accessor_get<'h>(
    vm: &'h VM,
    heap: &'h mut Heap,
    state: &'h ThreadState,
    scope: &HandleScope<'_>,
    getter: Option<NativeIndex>,
    holder: Handle<'_, Value>,
) -> Result<Coercion<'h>, VmError> {
    let Some(index) = getter else {
        return Ok(Coercion::Value(heap.known().undefined.get(heap).erase()));
    };
    let args = scope.stage(&[holder.as_tagged(heap).erase()]).as_args();
    let result = scope.handle(vm.runtime(index)(HostCtx::new(vm, heap, state), None, args));
    if result.as_tagged(heap) == heap.known().exception.get(heap) {
        Ok(Coercion::Threw)
    } else {
        Ok(Coercion::Value(result.as_tagged(heap)))
    }
}

fn native_accessor_set<'h>(
    vm: &'h VM,
    heap: &'h mut Heap,
    state: &'h ThreadState,
    scope: &HandleScope<'_>,
    setter: Option<NativeIndex>,
    holder: Handle<'_, Value>,
    value: Handle<'_, Value>,
) -> Result<Flow<SetApplied>, VmError> {
    let Some(index) = setter else {
        return Ok(Flow::Value(SetApplied::Rejected));
    };
    let args = scope
        .stage(&[holder.as_tagged(heap).erase(), value.as_tagged(heap)])
        .as_args();
    let result = scope.handle(vm.runtime(index)(HostCtx::new(vm, heap, state), None, args));
    if result.as_tagged(heap) == heap.known().exception.get(heap) {
        Ok(Flow::Threw)
    } else if result.as_tagged(heap) == heap.known().false_object.get(heap).erase() {
        // a native setter rejects the store by returning false (ArraySetLength
        // when a remaining element cannot be deleted); strict code turns this
        // into a TypeError, sloppy code ignores it
        Ok(Flow::Value(SetApplied::Rejected))
    } else {
        Ok(Flow::Value(SetApplied::CallSetter))
    }
}

/// The define-on-receiver tail of a [[Set]] (missing or shadowed hits);
/// the probe and the define may run proxy traps.
fn define_on_receiver(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    key: Handle<'_, Value>,
    value: Handle<'_, Value>,
) -> Result<Flow<SetApplied>, VmError> {
    let current = Proxy::own_descriptor(vm, heap, state, scope, receiver, key)?;
    let partial = match current {
        Flow::Threw => return Ok(Flow::Threw),
        Flow::Value(Some(d))
            if d.is_accessor_descriptor()
                || (d.is_data_descriptor() && d.writable == Some(false)) =>
        {
            return Ok(Flow::Value(SetApplied::Rejected));
        }
        Flow::Value(Some(_)) => PartialDescriptor::value(value),
        Flow::Value(None) => PartialDescriptor::new_data(value),
    };
    match Proxy::define_own_property(vm, heap, state, scope, receiver, key, partial)? {
        Flow::Threw => Ok(Flow::Threw),
        Flow::Value(true) => Ok(Flow::Value(SetApplied::Transition)),
        Flow::Value(false) => Ok(Flow::Value(SetApplied::Rejected)),
    }
}

/// `[[GetOwnProperty]]`'s [[Enumerable]] for an own key of an ordinary
/// object or string exotic: `None` when the property is absent. Proxy traps
/// are the caller's business.
pub fn own_property_enumerable(
    heap: &Heap,
    receiver: Tagged<'_, Value>,
    key: Tagged<'_, Value>,
) -> Option<bool> {
    let key = key.classify_key(heap).ok()?;
    // string exotic: indices are enumerable own properties, "length" is not
    if let Some(s) = DenseString::from_receiver(heap, receiver) {
        return match key {
            Key::Element(i) => Some((i as u64) < s.len() as u64),
            Key::Name(name) => s
                .own_key(heap, name)
                .map(|own| matches!(own, StringOwn::Index(_))),
        };
    }
    let object = receiver.as_heap_object()?;
    match key {
        Key::Element(i) => {
            if i >= object.max_element_index(heap) {
                return None;
            }
            // dense entries are created writable, enumerable, configurable
            if object.element_value_dense(heap, i).is_some() {
                return Some(true);
            }
            let entry = object.element_dictionary(heap)?.find(heap, i)?;
            let details = object.element_dictionary(heap)?.details_at(heap, entry);
            Some(details & DETAILS_ENUMERABLE != 0)
        }
        Key::Name(name) => object
            .map(heap)
            .descriptors()
            .iter()
            .find(|d| d.name(heap).ptr_eq(name))
            .map(|d| d.flags().is_enumerable()),
    }
}
