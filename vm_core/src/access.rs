use crate::{
    AccessorInfo, Change, DETAILS_CONFIGURABLE, DETAILS_ENUMERABLE, DenseString, FixedArray,
    Handle, HandleScope, Heap, HostCtx, Key, LanguageMode, NativeIndex, Object, PartialDescriptor,
    PropertyDescriptor, Prototype, Result, SetApplied, SlotFlags, SlotName, Smi, StringOwn, Symbol,
    Tagged, ThreadState, Thrown, VM, Value, VmError, bail, lookup::Hit, lookup::Lookup,
    proxy::Proxy, transition::Transition,
};

/// Store semantics:
/// - Self-style writes through to an inherited writable slot
/// - JS-style shadows it with a new own property on the receiver.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StoreSemantics {
    WriteThrough,
    Shadow,
}

/// A rejected define returns false or throws TypeError.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ShouldThrow {
    DontThrow,
    Throw,
}

/// Storage location of an own property, for apply without a second lookup.
pub enum OwnPropertyLocation<'s> {
    MapSlot(usize),
    Element(usize),
    NativeAccessor {
        index: usize,
        info: Handle<'s, AccessorInfo>,
        flags: SlotFlags,
    },
    Parent(usize),
}

pub struct OwnProperty<'s> {
    pub desc: PropertyDescriptor<'s>,
    pub location: Option<OwnPropertyLocation<'s>>,
}

impl ShouldThrow {
    /// Map a rejected define/store to the policy's outcome.
    pub fn reject(self, vm: &VM, heap: &mut Heap, state: &ThreadState) -> Result<bool> {
        match self {
            ShouldThrow::Throw => bail!(vm, heap, state, VmError::CannotRedefineProperty),
            ShouldThrow::DontThrow => Ok(false),
        }
    }
}

/// The ordinary-define variant used when no VM/thread is available; a
/// `Throw` policy cannot occur on that path.
fn reject_ordinary(
    vm: Option<&VM>,
    state: Option<&ThreadState>,
    heap: &mut Heap,
    should_throw: ShouldThrow,
) -> Result<bool> {
    match (vm, state) {
        (Some(vm), Some(state)) => should_throw.reject(vm, heap, state),
        _ => {
            debug_assert_eq!(should_throw, ShouldThrow::DontThrow);
            Ok(false)
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
    ) -> Result<Tagged<'h, Value>> {
        state.handle_scope(|scope| {
            if holder.as_tagged(heap).is_nullish(heap) {
                bail!(vm, heap, state, VmError::CannotReadPropertyOfNullish);
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
                        bail!(vm, heap, state, VmError::NotDefined, &text);
                    }
                    Ok(heap.known().undefined.get(heap).erase())
                }
                Hit::Proxy(proxy) => {
                    let proxy = scope.handle(proxy);
                    Proxy::get(vm, heap, state, proxy, receiver, name)
                }
                Hit::Data { value, .. }
                | Hit::Element { value, .. }
                | Hit::Parent { value, .. } => {
                    let value = scope.handle(value);
                    Ok(value.as_tagged(heap))
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
                    Ok(value.as_tagged(heap))
                }
                Hit::Accessor { pair, .. } => {
                    let undefined = heap.known().undefined.get_handle(heap, &scope).erase();
                    let getter = scope.handle(pair.get.get(heap));
                    if getter.as_tagged(heap) == undefined.as_tagged(heap) {
                        return Ok(undefined.as_tagged(heap));
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
                    Ok(result.as_tagged(heap))
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
    ) -> Result<SetApplied> {
        state.handle_scope(|scope| {
            if receiver.as_tagged(heap).is_nullish(heap) {
                bail!(vm, heap, state, VmError::CannotSetPropertyOfNullish);
            }
            let receiver_is_object = Proxy::is_js_receiver(heap, receiver.as_tagged(heap));
            let hit = {
                let start = scope.handle(start.as_tagged(heap).erase());
                let mut it = Lookup::new(heap, &scope, start, key, false);
                it.next(heap, &scope)
            };
            let outcome = match hit {
                Hit::Proxy(proxy) => {
                    let proxy = scope.handle(proxy);
                    match Proxy::set(vm, heap, state, proxy, key, value, receiver)? {
                        true => Ok(SetApplied::Done),
                        false => Ok(SetApplied::Rejected),
                    }
                }
                Hit::StringOwn { .. } => Ok(SetApplied::Rejected),
                Hit::Accessor { pair, .. } => {
                    let undefined = heap.known().undefined.get(heap).erase();
                    let setter = scope.handle(pair.set.get(heap));
                    if setter.as_tagged(heap) == undefined {
                        Ok(SetApplied::Rejected)
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
                        if !receiver_is_object {
                            Ok(SetApplied::Rejected)
                        } else {
                            define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                        }
                    } else if !flags.is_writable() {
                        Ok(SetApplied::Rejected)
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
                        Ok(SetApplied::Rejected)
                    } else if !receiver_is_object {
                        Ok(SetApplied::Rejected)
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
                            Ok(SetApplied::Done)
                        }
                    }
                }
                Hit::Element {
                    holder,
                    index,
                    flags,
                    ..
                } => {
                    // arguments exotic [[Set]]: write the parameter binding
                    // first when the receiver is the object itself
                    if holder.erase().ptr_eq(receiver.as_tagged(heap)) {
                        holder.arguments_mapped_set(heap, index, value.as_tagged(heap));
                    }
                    if !flags.is_writable() {
                        Ok(SetApplied::Rejected)
                    } else if !receiver_is_object {
                        Ok(SetApplied::Rejected)
                    } else {
                        let inherited = !holder.erase().ptr_eq(receiver.as_tagged(heap));
                        if inherited && semantics == StoreSemantics::Shadow {
                            define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                        } else {
                            let holder = scope.handle(holder);
                            Object::store_array_element(heap, &scope, &holder, index, &value);
                            Ok(SetApplied::Done)
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
                        pairs
                            .element_slot(slot)
                            .set(heap, pairs.erase(), value.as_tagged(heap));
                        Ok(SetApplied::Done)
                    } else if receiver_is_object {
                        define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                    } else {
                        Ok(SetApplied::Rejected)
                    }
                }
                Hit::Miss => {
                    if !receiver_is_object {
                        Ok(SetApplied::Rejected)
                    } else {
                        if contextual
                            && mode.is_strict()
                            && receiver
                                .as_tagged(heap)
                                .ptr_eq(vm.known().global_object.get(heap).erase())
                        {
                            bail!(vm, heap, state, VmError::UninitializedVariable);
                        }
                        define_on_receiver(vm, heap, state, &scope, receiver, key, value)
                    }
                }
            };
            match outcome {
                Ok(SetApplied::Rejected) if mode.is_strict() => {
                    bail!(vm, heap, state, VmError::CannotSetProperty)
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
    ) -> Result<bool> {
        state.handle_scope(|scope| {
            if start.as_tagged(heap).is_nullish(heap) {
                bail!(vm, heap, state, VmError::CannotCheckPropertyOfNullish);
            }
            let hit = Lookup::new(heap, &scope, start, key, false).next(heap, &scope);
            match hit {
                Hit::Proxy(proxy) => {
                    let proxy = scope.handle(proxy);
                    Proxy::has(vm, heap, state, proxy, key)
                }
                Hit::Miss => Ok(false),
                _ => Ok(true),
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
    ) -> Result<bool> {
        state.handle_scope(|scope| {
            if target.as_tagged(heap).is_nullish(heap) {
                bail!(vm, heap, state, VmError::CannotDeletePropertyOfNullish);
            }
            if Proxy::is_proxy(heap, target.as_tagged(heap)) {
                return Proxy::delete(vm, heap, state, target, key);
            }
            // String exotic own properties ("length"/indices) are
            // non-configurable, for primitives and String wrappers alike
            // (ES 10.4.3.1, 10.4.3.5); every other primitive property
            // deletes as absent.
            if let Some(s) = DenseString::from_receiver(heap, target.as_tagged(heap))
                && s.own_key(heap, key.as_tagged(heap).as_name()).is_some()
            {
                return Ok(false);
            }
            if target.as_tagged(heap).is_primitive(heap) {
                return Ok(true);
            }
            let receiver = scope
                .cast::<Object>(heap, target.as_tagged(heap))
                .expect("non-primitive receivers are objects");
            Self::delete_own_property(vm, heap, state, &scope, receiver, key)
        })
    }

    /// `[[GetPrototypeOf]] ( )` after `ToObject`
    /// [ES 10.1.2](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-getprototypeof)
    /// [ES 10.5.1](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-getprototypeof)
    /// Primitive receivers expose their builtin prototype; null/undefined
    /// throw.
    pub fn get_prototype<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let receiver = scope.handle(value.as_tagged(heap));

            let value = receiver.as_tagged(heap);
            if let Some(proto) = value.primitive_prototype(heap) {
                let proto = scope.handle(proto);
                return Ok(proto.as_tagged(heap));
            }
            if value.is_primitive(heap) {
                bail!(vm, heap, state, VmError::CannotConvertNullishToObject);
            }

            let proto = Proxy::get_prototype_of(vm, heap, state, receiver)?;
            Ok(scope.handle(proto).as_tagged(heap))
        })
    }

    /// `[[GetOwnProperty]] ( propertyKey )`
    /// [ES 10.1.5](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-getownproperty-p)
    /// [ES 10.4.3.1](https://tc39.es/ecma262/#sec-string-exotic-objects-getownproperty-p)
    /// [ES 10.5.5](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-getownproperty-p)
    /// Dispatches ordinary objects, String exotic keys, and proxies.
    pub fn get_own_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Result<Option<OwnProperty<'s>>> {
        if obj.as_tagged(heap).is_nullish(heap) {
            bail!(vm, heap, state, VmError::CannotGetPropertyOfNullish);
        }
        if Proxy::is_proxy(heap, obj.as_tagged(heap)) {
            return Proxy::get_own_property(vm, heap, state, scope, obj, key);
        }
        // TODO: dispatch typed-array and module-namespace exotic
        // [[GetOwnProperty]] once those object kinds exist (ES 10.4.5,
        // 10.4.6); arguments values are substituted in `Lookup`.
        // Other primitives have no own properties; String primitives have
        // the virtual code-unit/length properties.
        if obj.as_tagged(heap).is_primitive(heap)
            && DenseString::from_receiver(heap, obj.as_tagged(heap)).is_none()
        {
            return Ok(None);
        }
        Ok(Self::own_property_ordinary(heap, scope, obj, key))
    }

    /// `[[OwnPropertyKeys]] ( )`
    /// [ES 10.1.11](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-ownpropertykeys)
    /// [ES 10.4.3.3](https://tc39.es/ecma262/#sec-string-exotic-objects-ownpropertykeys)
    /// [ES 10.5.11](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-ownpropertykeys)
    /// Integer indices ascending, then string keys, then symbol keys.
    pub fn own_property_keys<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        target: Handle<'_, Value>,
    ) -> Result<Vec<Handle<'s, Value>>> {
        if Proxy::is_proxy(heap, target.as_tagged(heap)) {
            return Proxy::own_keys(vm, heap, state, scope, target);
        }
        let mut keys: Vec<Handle<'s, Value>> = Vec::new();
        if let Some(string) = DenseString::from_receiver(heap, target.as_tagged(heap)) {
            let length = string.len();
            for index in 0..length {
                keys.push(scope.handle(Smi::new(index as i64).into_tagged()));
            }
            let Some(object) = target.as_tagged(heap).as_heap_object() else {
                let length = heap.known().strings.length.get_handle(heap, scope);
                keys.push(scope.handle(length.as_tagged(heap).erase()));
                return Ok(keys);
            };
            for index in object.element_keys(heap) {
                if index >= length {
                    keys.push(scope.handle(Smi::new(index as i64).into_tagged()));
                }
            }
            let length = heap.known().strings.length.get_handle(heap, scope);
            keys.push(scope.handle(length.as_tagged(heap).erase()));
            push_named_own_keys(heap, scope, object, true, &mut keys);
            return Ok(keys);
        }
        if let Some(object) = target.as_tagged(heap).as_heap_object() {
            for index in object.element_keys(heap) {
                keys.push(scope.handle(Smi::new(index as i64).into_tagged()));
            }
            push_named_own_keys(heap, scope, object, false, &mut keys);
        }
        Ok(keys)
    }

    /// Non-proxy `[[GetOwnProperty]]`: descriptor plus storage location.
    pub fn own_property_ordinary<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        receiver: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Option<OwnProperty<'s>> {
        // TODO: ES 10.4.3.1 checks ordinary own properties before
        // StringGetOwnProperty; Lookup reports StringOwn first.
        let hit = Lookup::new(heap, scope, receiver, key, true).next(heap, scope);
        match hit {
            Hit::Data {
                index,
                value,
                flags,
                ..
            } => Some(OwnProperty {
                desc: PropertyDescriptor::Data {
                    value: scope.handle(value),
                    writable: flags.is_writable(),
                    enumerable: flags.is_enumerable(),
                    configurable: flags.is_configurable(),
                },
                location: Some(OwnPropertyLocation::MapSlot(index)),
            }),
            Hit::Accessor {
                index, pair, flags, ..
            } => Some(OwnProperty {
                desc: PropertyDescriptor::Accessor {
                    get: scope.handle(pair.get.get(heap)),
                    set: scope.handle(pair.set.get(heap)),
                    enumerable: flags.is_enumerable(),
                    configurable: flags.is_configurable(),
                },
                location: Some(
                    if matches!(
                        key.as_tagged(heap).classify_key(heap),
                        Some(Key::Element(_))
                    ) {
                        OwnPropertyLocation::Element(index)
                    } else {
                        OwnPropertyLocation::MapSlot(index)
                    },
                ),
            }),
            // Native accessors present as data; only array length exists.
            Hit::NativeAccessor {
                index,
                holder,
                info,
                flags,
                ..
            } => holder
                .array_length(heap, key.as_tagged(heap).as_name())
                .map(|value| OwnProperty {
                    desc: PropertyDescriptor::Data {
                        value: scope.handle(value),
                        writable: flags.is_writable(),
                        enumerable: flags.is_enumerable(),
                        configurable: flags.is_configurable(),
                    },
                    location: Some(OwnPropertyLocation::NativeAccessor {
                        index,
                        info: scope.handle(info),
                        flags,
                    }),
                }),
            Hit::Element {
                index,
                value,
                flags,
                ..
            } => Some(OwnProperty {
                desc: PropertyDescriptor::Data {
                    value: scope.handle(value),
                    writable: flags.is_writable(),
                    enumerable: flags.is_enumerable(),
                    configurable: flags.is_configurable(),
                },
                location: Some(OwnPropertyLocation::Element(index)),
            }),
            Hit::Parent { slot, value, .. } => Some(OwnProperty {
                desc: PropertyDescriptor::Data {
                    value: scope.handle(value),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
                location: Some(OwnPropertyLocation::Parent(slot)),
            }),
            Hit::StringOwn { .. } => {
                DenseString::own_descriptor(heap, scope, receiver, key).map(|desc| OwnProperty {
                    desc,
                    location: None,
                })
            }
            Hit::Proxy(_) | Hit::Miss => None,
        }
    }

    /// `[[DefineOwnProperty]] ( propertyKey, propertyDesc )`
    /// [ES 10.1.6](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-defineownproperty-p-desc)
    /// [ES 10.4.2.1](https://tc39.es/ecma262/#sec-array-exotic-objects-defineownproperty-p-desc)
    /// [ES 10.4.3.2](https://tc39.es/ecma262/#sec-string-exotic-objects-defineownproperty-p-desc)
    /// [ES 10.5.6](https://tc39.es/ecma262/#sec-proxy-object-internal-methods-and-internal-slots-defineownproperty-p-desc)
    /// Dispatches ordinary, array length, String, and proxy defines.
    pub fn define_own_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        name: Handle<'_, Value>,
        partial: PartialDescriptor<'s>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        if Proxy::is_proxy(heap, obj.as_tagged(heap)) {
            return Proxy::define_own_property(
                vm,
                heap,
                state,
                scope,
                obj,
                name,
                partial,
                should_throw,
            );
        }
        if obj.as_tagged(heap).is_primitive(heap) {
            bail!(vm, heap, state, VmError::CannotDefineOnPrimitive);
        }
        if obj
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_arguments())
        {
            return Self::arguments_define_own_property(
                vm,
                heap,
                state,
                scope,
                obj,
                name,
                partial,
                should_throw,
            );
        }
        if obj
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.is_array_exotic(heap))
            && name
                .as_tagged(heap)
                .as_name()
                .erase()
                .ptr_eq(heap.known().strings.length.get(heap))
        {
            let array = scope
                .cast::<Object>(heap, obj.as_tagged(heap))
                .expect("array checked above");
            return Self::array_set_length(vm, heap, state, scope, array, partial, should_throw);
        }
        if let Some(current) = DenseString::own_descriptor(heap, scope, obj, name) {
            let current = PartialDescriptor::from(&current);
            let receiver = scope
                .cast::<Object>(heap, obj.as_tagged(heap))
                .expect("string wrappers are objects");
            let extensible = receiver.as_tagged(heap).is_extendable(heap);
            if Transition::is_compatible_property_descriptor(
                heap,
                extensible,
                &partial,
                Some(&current),
            ) {
                return Ok(true);
            }
            return should_throw.reject(vm, heap, state);
        }
        // TODO: dispatch typed-array and module-namespace exotic
        // [[DefineOwnProperty]] once those object kinds exist (ES 10.4.5,
        // 10.4.6).
        let receiver = scope
            .cast::<Object>(heap, obj.as_tagged(heap))
            .expect("non-proxy target is an object");
        let current = Self::own_property_ordinary(heap, scope, obj, name);
        let name = scope.handle(name.as_tagged(heap).as_name());
        Self::define_own_property_apply(
            Some(vm),
            Some(state),
            heap,
            scope,
            receiver,
            name,
            current,
            &partial,
            should_throw,
        )
    }

    /// `[[DefineOwnProperty]] ( propertyKey, propertyDesc )`
    /// [ES 10.4.4.2](https://tc39.es/ecma262/#sec-arguments-exotic-objects-defineownproperty-p-desc)
    /// Arguments exotic define: an index still in `[[ParameterMap]]` writes
    /// through to the parameter binding and loses its mapping when made an
    /// accessor or non-writable.
    #[allow(clippy::too_many_arguments)]
    fn arguments_define_own_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        name: Handle<'_, Value>,
        partial: PartialDescriptor<'s>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        let index = match name.as_tagged(heap).classify_key(heap) {
            Some(Key::Element(index)) => Some(index),
            _ => None,
        };
        let mapped = index.is_some_and(|index| {
            obj.as_tagged(heap)
                .as_heap_object()
                .is_some_and(|o| o.arguments_mapping(heap, index).is_some())
        });
        let mut effective = partial;
        if mapped
            && partial.is_data_descriptor()
            && partial.value.is_none()
            && partial.writable == Some(false)
            && let Some(value) = obj
                .as_tagged(heap)
                .as_heap_object()
                .and_then(|o| o.arguments_mapped_value(heap, index.expect("mapped index")))
        {
            effective.value = Some(scope.handle(value));
        }
        let current = Self::own_property_ordinary(heap, scope, obj, name);
        let name = scope.handle(name.as_tagged(heap).as_name());
        let receiver = scope
            .cast::<Object>(heap, obj.as_tagged(heap))
            .expect("arguments target is an object");
        let allowed = Self::define_own_property_apply(
            Some(vm),
            Some(state),
            heap,
            scope,
            receiver,
            name,
            current,
            &effective,
            should_throw,
        )?;
        if allowed && let Some(index) = index.filter(|_| mapped) {
            let object = obj
                .as_tagged(heap)
                .as_heap_object()
                .expect("arguments target is an object");
            if partial.is_accessor_descriptor() {
                object.arguments_remove_mapping(heap, index);
            } else {
                if let Some(value) = partial.value {
                    object.arguments_mapped_set(heap, index, value.as_tagged(heap));
                }
                if partial.writable == Some(false) {
                    object.arguments_remove_mapping(heap, index);
                }
            }
        }
        Ok(allowed)
    }

    /// `[[DefineOwnProperty]] ( propertyKey, propertyDesc )`
    /// [ES 10.1.6](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-defineownproperty-p-desc)
    /// [ES 10.1.6.3](https://tc39.es/ecma262/#sec-validateandapplypropertydescriptor)
    /// Ordinary-object define; use [`Self::define_own_property`] for exotics.
    pub fn define_own_property_ordinary(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
        desc: PropertyDescriptor<'_>,
    ) -> Result<bool> {
        let receiver_value = scope.handle(receiver.as_tagged(heap).erase());
        let name_value = scope.handle(name.as_tagged(heap).erase());
        let current = Self::own_property_ordinary(heap, scope, receiver_value, name_value);
        let partial = PartialDescriptor::from(&desc);
        Self::define_own_property_apply(
            None,
            None,
            heap,
            scope,
            receiver,
            name,
            current,
            &partial,
            ShouldThrow::DontThrow,
        )
    }

    /// Ordinary define after `[[GetOwnProperty]]`: validates against `current`,
    /// then adds, writes, redefines, or applies native accessor-backed data
    /// through its setter.
    #[allow(clippy::too_many_arguments)]
    pub fn define_own_property_apply<'s>(
        vm: Option<&VM>,
        state: Option<&ThreadState>,
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        receiver: Handle<'_, Object>,
        name: Handle<'_, SlotName>,
        current: Option<OwnProperty<'s>>,
        partial: &PartialDescriptor<'s>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        let undefined = heap.known().undefined.get_handle(heap, scope).erase();
        let extensible = receiver.as_tagged(heap).is_extendable(heap);
        let current_partial = current
            .as_ref()
            .map(|current| PartialDescriptor::from(&current.desc));
        if !Transition::is_compatible_property_descriptor(
            heap,
            extensible,
            partial,
            current_partial.as_ref(),
        ) {
            return reject_ordinary(vm, state, heap, should_throw);
        }
        let Some(current) = current else {
            let full = partial.complete_against(undefined, None);
            if Self::add_own_property(heap, scope, receiver, name, full) {
                return Ok(true);
            }
            return reject_ordinary(vm, state, heap, should_throw);
        };
        let full = partial.complete_against(undefined, Some(&current.desc));
        match current.location {
            None => Ok(true),
            Some(OwnPropertyLocation::Parent(_)) => {
                if Self::add_own_property(heap, scope, receiver, name, full) {
                    Ok(true)
                } else {
                    reject_ordinary(vm, state, heap, should_throw)
                }
            }
            Some(OwnPropertyLocation::Element(index)) => {
                if Self::define_element(heap, scope, receiver, index, full, Some(&current.desc)) {
                    Ok(true)
                } else {
                    reject_ordinary(vm, state, heap, should_throw)
                }
            }
            Some(OwnPropertyLocation::MapSlot(index)) => {
                let same = match (&current.desc, &full) {
                    (
                        PropertyDescriptor::Accessor {
                            get: cg, set: cs, ..
                        },
                        PropertyDescriptor::Accessor {
                            get: fg, set: fs, ..
                        },
                    ) => {
                        full.flags() == current.desc.flags()
                            && cg.as_tagged(heap).ptr_eq(fg.as_tagged(heap))
                            && cs.as_tagged(heap).ptr_eq(fs.as_tagged(heap))
                    }
                    (PropertyDescriptor::Data { .. }, PropertyDescriptor::Data { .. }) => {
                        full.flags() == current.desc.flags()
                    }
                    _ => false,
                };
                if same {
                    if partial.value.is_some()
                        && let PropertyDescriptor::Data { value, .. } = full
                    {
                        Transition::write_slot(heap, receiver, index, value);
                    }
                } else {
                    Transition::define(
                        heap,
                        scope,
                        receiver,
                        name,
                        full,
                        Change::Replace { index },
                    );
                }
                Ok(true)
            }
            Some(OwnPropertyLocation::NativeAccessor { index, info, flags }) => {
                let PropertyDescriptor::Data {
                    value,
                    writable,
                    enumerable,
                    configurable,
                } = full
                else {
                    unreachable!("native accessors validate as data")
                };
                if partial.value.is_some() {
                    let (Some(vm), Some(state)) = (vm, state) else {
                        return reject_ordinary(vm, state, heap, should_throw);
                    };
                    let holder = scope.handle(receiver.as_tagged(heap).erase());
                    match native_accessor_set(
                        vm,
                        heap,
                        state,
                        scope,
                        info.as_tagged(heap).setter(),
                        holder,
                        value,
                    )? {
                        SetApplied::Rejected => {
                            return should_throw.reject(vm, heap, state);
                        }
                        _ => {}
                    }
                }
                let mut new_flags = SlotFlags::ACCESSOR;
                if writable {
                    new_flags = new_flags.union(SlotFlags::WRITABLE);
                }
                if enumerable {
                    new_flags = new_flags.union(SlotFlags::ENUMERABLE);
                }
                if configurable {
                    new_flags = new_flags.union(SlotFlags::CONFIGURABLE);
                }
                if new_flags != flags {
                    Transition::replace_native_accessor_flags(
                        heap, scope, receiver, name, index, new_flags,
                    );
                }
                Ok(true)
            }
        }
    }

    /// DefineProperty ( obj, key, attrs )
    /// [ES 20.1.2.4](https://tc39.es/ecma262/#sec-object.defineproperty)
    /// Shared `Object.defineProperty` tail: object check, ToPropertyKey,
    /// ToPropertyDescriptor, then [[DefineOwnProperty]].
    pub fn define_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        key: Handle<'_, Value>,
        attrs: Handle<'_, Value>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        if obj.as_tagged(heap).is_primitive(heap) {
            bail!(vm, heap, state, VmError::CannotDefineOnPrimitive);
        }
        let key = Self::to_property_key(vm, heap, state, key)?;
        let key = scope.handle(key);
        let partial = Self::to_property_descriptor(vm, heap, state, scope, attrs)?;
        Self::define_own_property(
            vm,
            heap,
            state,
            scope,
            obj,
            key.erase(),
            partial,
            should_throw,
        )
    }

    /// DefinePropertyOrThrow ( obj, propertyKey, propertyDesc )
    /// [ES 7.3.8](https://tc39.es/ecma262/#sec-definepropertyorthrow)
    /// [[DefineOwnProperty]] with a TypeError when the define is rejected.
    pub fn define_property_or_throw<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        name: Handle<'_, Value>,
        partial: PartialDescriptor<'s>,
    ) -> Result<bool> {
        let defined = Self::define_own_property(
            vm,
            heap,
            state,
            scope,
            obj,
            name,
            partial,
            ShouldThrow::Throw,
        )?;
        if defined {
            Ok(true)
        } else {
            bail!(vm, heap, state, VmError::CannotDefineProperty)
        }
    }

    /// CreateDataPropertyOrThrow ( obj, propertyKey, value )
    /// [ES 7.3.6](https://tc39.es/ecma262/#sec-createdatapropertyorthrow)
    /// Defines `{ value, writable: true, enumerable: true, configurable: true }`.
    pub fn create_data_property_or_throw<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        name: Handle<'_, Value>,
        value: Handle<'s, Value>,
    ) -> Result<bool> {
        let partial = PartialDescriptor::from(&PropertyDescriptor::data(value));
        Self::define_property_or_throw(vm, heap, state, scope, obj, name, partial)
    }

    /// `[[Delete]] ( propertyKey )`
    /// [ES 10.1.10](https://tc39.es/ecma262/#sec-ordinary-object-internal-methods-and-internal-slots-delete-p)
    /// [ES 10.1.10.1](https://tc39.es/ecma262/#sec-ordinarydelete)
    /// Removes a configurable own property and returns true; a
    /// non-configurable key stays and returns false, while an absent key
    /// returns true. Canonical indices are removed from the elements store
    /// (dense hole or dictionary entry). Proxy and String-exotic dispatch
    /// happen in `delete_property`.
    pub fn delete_own_property(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        key: Handle<'_, Value>,
    ) -> Result<bool> {
        let arguments_index = match key.as_tagged(heap).classify_key(heap) {
            Some(Key::Element(index))
                if receiver.as_tagged(heap).map(heap).kind().is_arguments() =>
            {
                Some(index)
            }
            _ => None,
        };
        let mapped = arguments_index.is_some_and(|index| {
            receiver
                .as_tagged(heap)
                .arguments_mapping(heap, index)
                .is_some()
        });
        let result = Self::delete_own_property_ordinary(vm, heap, state, scope, receiver, key)?;
        if result && let Some(index) = arguments_index.filter(|_| mapped) {
            // arguments exotic [[Delete]] also drops the parameter mapping
            receiver
                .as_tagged(heap)
                .arguments_remove_mapping(heap, index);
        }
        Ok(result)
    }

    fn delete_own_property_ordinary(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        key: Handle<'_, Value>,
    ) -> Result<bool> {
        let name = 'name: {
            let Some(key) = key.as_tagged(heap).classify_key(heap) else {
                bail!(vm, heap, state, VmError::InvalidPropertyKey);
            };
            match key {
                Key::Element(i) => {
                    let obj = receiver.as_tagged(heap);
                    if let Some(dict) = obj.element_dictionary(heap) {
                        if let Some(entry) = dict.find(heap, i) {
                            if dict.details_at(heap, entry) & DETAILS_CONFIGURABLE == 0 {
                                return Ok(false);
                            }
                            dict.delete(heap, i);
                            Prototype::element_mutated(heap, obj);
                        }
                        break 'name None;
                    }
                    // indices beyond the visible range were never own
                    // properties (arrays: `length`; others: capacity)
                    if i < obj.max_element_index(heap)
                        && obj.map(heap).kind().is_dense_elements()
                        && let Some(elements) = obj.elements_array(heap)
                        && i < elements.len()
                    {
                        let hole = heap.known().the_hole.get(heap).erase();
                        if elements.at(heap, i) != hole {
                            elements.set(heap, i, hole);
                            Object::promote_holey(heap, scope, &receiver);
                            Prototype::element_mutated(heap, receiver.as_tagged(heap));
                        }
                    }
                    break 'name None;
                }
                Key::Name(name) => Some(scope.handle(name)),
            }
        };
        let Some(name) = name else {
            return Ok(true);
        };
        Ok(Self::delete_named_property(heap, scope, receiver, name))
    }

    fn delete_named_property(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        receiver: Handle<Object>,
        name: Handle<SlotName>,
    ) -> bool {
        let configurable = receiver
            .as_tagged(heap)
            .map(heap)
            .descriptors()
            .iter()
            .find(|d| d.name(heap).ptr_eq(name.as_tagged(heap)))
            .map(|d| d.flags().is_configurable());
        if configurable != Some(true) {
            return configurable.is_none();
        }
        Transition::remove_property(heap, scope, receiver, name);
        true
    }
}

fn push_named_own_keys<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    object: Tagged<'_, Object>,
    skip_length: bool,
    keys: &mut Vec<Handle<'s, Value>>,
) {
    let length = heap.known().strings.length.get(heap);
    for symbols in [false, true] {
        for descriptor in object.map(heap).descriptors() {
            let name = descriptor.name(heap).erase();
            if name.get_as::<Symbol>(heap).is_some() != symbols {
                continue;
            }
            if skip_length && name.ptr_eq(length.erase()) {
                continue;
            }
            if matches!(name.classify_key(heap), Some(Key::Element(_))) {
                continue;
            }
            keys.push(scope.handle(name));
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
) -> Result<SetApplied> {
    let args = scope.stage(&[receiver.as_tagged(heap).erase(), value.as_tagged(heap)]);
    HostCtx::enter(vm, state.runtime(vm, heap), heap, state, setter, args, None)?;
    Ok(SetApplied::CallSetter)
}

fn native_accessor_get<'h>(
    vm: &'h VM,
    heap: &'h mut Heap,
    state: &'h ThreadState,
    scope: &HandleScope<'_>,
    getter: Option<NativeIndex>,
    holder: Handle<'_, Value>,
) -> Result<Tagged<'h, Value>> {
    let Some(index) = getter else {
        return Ok(heap.known().undefined.get(heap).erase());
    };
    let args = scope.stage(&[holder.as_tagged(heap).erase()]).as_args();
    let result = scope.handle(vm.native(index)(
        HostCtx::new(vm, state.runtime(vm, heap), heap, state),
        None,
        args,
    ));
    if result.as_tagged(heap) == heap.known().exception.get(heap) {
        Err(Thrown)
    } else {
        Ok(result.as_tagged(heap))
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
) -> Result<SetApplied> {
    let Some(index) = setter else {
        return Ok(SetApplied::Rejected);
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
        Err(Thrown)
    } else if result.as_tagged(heap) == heap.known().false_object.get(heap).erase() {
        // a native setter rejects the store by returning false (ArraySetLength
        // when a remaining element cannot be deleted); strict code turns this
        // into a TypeError, sloppy code ignores it
        Ok(SetApplied::Rejected)
    } else {
        Ok(SetApplied::CallSetter)
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
) -> Result<SetApplied> {
    let current = Object::get_own_property(vm, heap, state, scope, receiver, key)?;
    let partial = match current {
        Some(d) if !d.desc.is_data_descriptor() || !d.desc.is_writable() => {
            return Ok(SetApplied::Rejected);
        }
        Some(_) => PartialDescriptor::value(value),
        None => PartialDescriptor::new_data(value),
    };
    match Object::define_own_property(
        vm,
        heap,
        state,
        scope,
        receiver,
        key,
        partial,
        ShouldThrow::DontThrow,
    )? {
        true => Ok(SetApplied::Transition),
        false => Ok(SetApplied::Rejected),
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
    let key = key.classify_key(heap)?;
    // String virtual keys win when present; wrappers may also have
    // ordinary own properties.
    if let Some(s) = DenseString::from_receiver(heap, receiver) {
        let virtual_key = match key {
            Key::Element(i) if (i as u64) < s.len() as u64 => Some(true),
            Key::Name(name) => s
                .own_key(heap, name)
                .map(|own| matches!(own, StringOwn::Index(_))),
            _ => None,
        };
        if virtual_key.is_some() {
            return virtual_key;
        }
        if receiver.is_primitive(heap) {
            return None;
        }
    }
    let object = receiver.as_heap_object()?;
    match key {
        Key::Element(i) => {
            if object.is_array_exotic(heap) && i >= object.length() {
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
