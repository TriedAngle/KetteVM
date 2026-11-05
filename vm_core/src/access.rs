use crate::errors::Errors;
use crate::lookup::{Hit, Lookup};
use crate::proxy::{Flow, Proxy};
use crate::transition::{Transition, can_add_element};
use crate::{
    AccessorInfo, AccessorPair, Change, Coercion, Compare, Convert, DETAILS_CONFIGURABLE,
    DETAILS_ENUMERABLE, DenseString, FixedArray, Handle, HandleScope, Heap, HostCtx, Key,
    LanguageMode, NativeIndex, Object, PartialDescriptor, PropertyDescriptor, Prototype,
    SetApplied, SlotFlags, SlotName, Smi, StringOwn, Tagged, ThreadState, VM, Value, VmError,
    raise_runtime,
};

/// Store semantics:
/// - Self-style writes through to an inherited writable slot
/// - JS-style shadows it with a new own property on the receiver.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StoreSemantics {
    WriteThrough,
    Shadow,
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
                    let undefined = heap.known().undefined.get_handle(heap, &scope).erase();
                    let getter = scope.handle(pair.get.get(heap));
                    if getter.as_tagged(heap) == undefined.as_tagged(heap) {
                        return Ok(Coercion::Value(undefined.as_tagged(heap)));
                    }
                    let args = scope.stage(&[receiver.as_tagged(heap).erase()]);
                    let result = scope.handle(HostCtx::enter(
                        vm,
                        state.runtime(vm, heap),
                        heap,
                        state,
                        getter,
                        args,
                        None,
                    )?);
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
                            pairs.element_slot(slot).set(
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

    pub fn define_own_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        desc: PropertyDescriptor<'_>,
    ) -> Result<bool, VmError> {
        if let Ok(Key::Element(i)) = name.as_tagged(heap).erase().classify_key(heap) {
            let current = Self::element_own_descriptor(heap, scope, receiver.as_tagged(heap), i);
            match current {
                Some(current) => {
                    if !element_define_valid(heap, &current, &desc) {
                        return Ok(false);
                    }
                }
                None => {
                    if !can_add_element(heap, receiver.as_tagged(heap), i) {
                        return Ok(false);
                    }
                }
            }
            return Self::define_element(heap, scope, receiver, i, desc);
        }
        let current = receiver
            .as_tagged(heap)
            .map(heap)
            .descriptors()
            .iter()
            .enumerate()
            .find(|(_, d)| d.name(heap).ptr_eq(name.as_tagged(heap)))
            .map(|(index, d)| (index, d.flags(), scope.handle(d.value.get(heap))));
        let Some((index, cur_flags, cur_desc_value)) = current else {
            return Self::add_own_property(heap, scope, receiver, name, desc);
        };

        let Some(action) = ({
            validate_define(
                heap,
                receiver.as_tagged(heap),
                name.as_tagged(heap),
                cur_flags,
                cur_desc_value,
                desc,
            )
        }) else {
            return Ok(false);
        };
        Self::apply_define(heap, scope, receiver, name, index, action);
        Ok(true)
    }

    pub fn delete_own_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        key: Handle<'_, Value>,
    ) -> Result<bool, VmError> {
        let name = 'name: {
            let key = key.as_tagged(heap).classify_key(heap)?;
            match key {
                Key::Element(i) => {
                    let obj = receiver.as_tagged(heap);
                    if let Some(dict) = obj.element_dictionary(heap) {
                        // non-configurable entries reject the delete (ES 10.5.2.10)
                        if let Some(entry) = dict.find(heap, i)
                            && dict.details_at(heap, entry) & DETAILS_CONFIGURABLE == 0
                        {
                            return Ok(false);
                        }
                        dict.delete(heap, i);
                        Prototype::element_mutated(heap, obj);
                        break 'name None;
                    }
                    // indices beyond the visible range were never own
                    // properties (arrays: `length`; others: capacity)
                    if i < obj.max_element_index(heap)
                        && obj.map(heap).kind().is_dense_elements()
                        && let Some(elements) = obj.elements_array(heap)
                        && i < elements.len()
                    {
                        elements.set(heap, i, heap.known().the_hole.get(heap).erase());
                        Object::promote_holey(heap, scope, &receiver);
                        Prototype::element_mutated(heap, receiver.as_tagged(heap));
                    }
                    break 'name None;
                }
                Key::Name(name) => Some(scope.handle(name)),
            }
        };
        let Some(name) = name else {
            return Ok(true);
        };
        Self::delete_named_property(heap, scope, receiver, name)
    }

    fn delete_named_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
    ) -> Result<bool, VmError> {
        let configurable = receiver
            .as_tagged(heap)
            .map(heap)
            .descriptors()
            .iter()
            .find(|d| d.name(heap).ptr_eq(name.as_tagged(heap)))
            .map(|d| d.flags().is_configurable());
        if configurable != Some(true) {
            return Ok(configurable.is_none());
        }
        Transition::remove_property(heap, scope, receiver, name);
        Ok(true)
    }

    fn apply_define(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        index: usize,
        action: DefineAction,
    ) {
        match action {
            DefineAction::Nothing => {}
            DefineAction::WriteDataSlot { value } => {
                // the in-place data write needs no map change
                Transition::write_slot(heap, receiver, index, value);
            }
            DefineAction::Redefine { desc } => {
                Transition::define(heap, scope, receiver, name, desc, Change::Replace { index })
            }
            DefineAction::NativeAccessor { flags } => {
                Transition::replace_native_accessor_flags(
                    heap, scope, receiver, name, index, flags,
                );
            }
        }
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
        let length_key = heap.known().strings.length.get_handle(heap, scope).erase();
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
    let result = scope.handle(HostCtx::enter(
        vm,
        state.runtime(vm, heap),
        heap,
        state,
        setter,
        args,
        None,
    )?);
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
    let result = scope.handle(vm.native(index)(
        HostCtx::new(vm, state.runtime(vm, heap), heap, state),
        None,
        args,
    ));
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
    let result = scope.handle(vm.native(index)(
        HostCtx::new(vm, state.runtime(vm, heap), heap, state),
        None,
        args,
    ));
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

enum DefineAction<'s> {
    /// Valid, nothing to change.
    Nothing,
    /// data→data with unchanged attributes: write the existing slot.
    WriteDataSlot { value: Handle<'s, Value> },
    /// Replace the descriptor with the new one (data or accessor).
    Redefine { desc: PropertyDescriptor<'s> },
    /// Change flags on a native accessor while keeping its accessor info.
    NativeAccessor { flags: SlotFlags },
}

/// ES 3.c of ValidateAndApplyPropertyDescriptor, element edition: a
/// non-configurable existing element only accepts a compatible
/// redefinition (same kind and attributes; same value when
/// non-writable; identical pair halves).
fn element_define_valid(
    heap: &Heap,
    current: &PropertyDescriptor<'_>,
    desc: &PropertyDescriptor<'_>,
) -> bool {
    use PropertyDescriptor as D;
    let configurable = |d: &D| match d {
        D::Data { configurable, .. } | D::Accessor { configurable, .. } => *configurable,
    };
    if configurable(current) {
        return true;
    }
    if configurable(desc) {
        return false;
    }
    match (current, desc) {
        (
            D::Data {
                value: cv,
                writable: cw,
                enumerable: ce,
                ..
            },
            D::Data {
                value: nv,
                writable: nw,
                enumerable: ne,
                ..
            },
        ) => {
            ce == ne
                && (*cw
                    || (!*nw && Compare::same_value(heap, cv.as_tagged(heap), nv.as_tagged(heap))))
        }
        (
            D::Accessor {
                get: cg,
                set: cs,
                enumerable: ce,
                ..
            },
            D::Accessor {
                get: ng,
                set: ns,
                enumerable: ne,
                ..
            },
        ) => {
            ce == ne
                && cg.as_tagged(heap).ptr_eq(ng.as_tagged(heap))
                && cs.as_tagged(heap).ptr_eq(ns.as_tagged(heap))
        }
        _ => false,
    }
}

fn validate_define<'a, 's>(
    heap: &'a Heap,
    receiver: Tagged<'a, Object>,
    name: Tagged<'a, SlotName>,
    cur_flags: SlotFlags,
    cur_desc_value: Handle<'s, Value>,
    desc: PropertyDescriptor<'s>,
) -> Option<DefineAction<'s>> {
    let cur_configurable = cur_flags.is_configurable();

    if cur_flags.is_accessor() {
        if cur_configurable {
            return Some(DefineAction::Redefine { desc });
        }
        if cur_desc_value
            .as_tagged(heap)
            .get_as::<AccessorInfo>(heap)
            .is_some()
        {
            let PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            } = desc
            else {
                return None;
            };
            if configurable || enumerable != cur_flags.is_enumerable() {
                return None;
            }
            let current = receiver.array_length(heap, name)?;
            if !Compare::same_value(heap, value.as_tagged(heap), current) {
                return None;
            }
            if writable == cur_flags.is_writable() {
                return Some(DefineAction::Nothing);
            }
            if !writable && cur_flags.is_writable() {
                return Some(DefineAction::NativeAccessor {
                    flags: cur_flags.without_writable(),
                });
            }
            return None;
        }
        let PropertyDescriptor::Accessor {
            get,
            set,
            enumerable,
            configurable,
        } = desc
        else {
            return None;
        };
        if configurable || enumerable != cur_flags.is_enumerable() {
            return None;
        }
        // Native accessors are handled above; this is a user accessor pair.
        let cur_pair = cur_desc_value
            .as_tagged(heap)
            .get_as::<AccessorPair>(heap)?;
        if !Compare::same_value(heap, get.as_tagged(heap), cur_pair.get.get(heap))
            || !Compare::same_value(heap, set.as_tagged(heap), cur_pair.set.get(heap))
        {
            return None;
        }
        return Some(DefineAction::Nothing);
    }

    if let PropertyDescriptor::Accessor { .. } = desc {
        if !cur_configurable {
            return None;
        }
        return Some(DefineAction::Redefine { desc });
    }

    let PropertyDescriptor::Data {
        value,
        writable,
        enumerable,
        configurable,
    } = desc
    else {
        unreachable!("descriptor kind checked above")
    };
    if !cur_configurable {
        if configurable || enumerable != cur_flags.is_enumerable() {
            return None;
        }
        if !cur_flags.is_writable() {
            if writable {
                return None;
            }

            let offset = cur_desc_value
                .as_tagged(heap)
                .to_i64()
                .expect("data row offset") as usize;
            if !Compare::same_value(
                heap,
                value.as_tagged(heap),
                receiver.slot(heap, offset).get(heap),
            ) {
                return None;
            }
            return Some(DefineAction::Nothing);
        }
    }
    if desc.flags() == cur_flags {
        return Some(DefineAction::WriteDataSlot { value });
    }
    Some(DefineAction::Redefine { desc })
}
