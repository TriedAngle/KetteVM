//! ES 20.1: the Object constructor, statics, and prototype methods.
use crate::JSRuntime;
use vm_core::{
    Args, CallInfo, Compare, Convert, DenseString, Errors, Handle, HandleScope, HandleSlice, Heap,
    HostCtx, NativeIndex, Object, PartialDescriptor, Result, ShouldThrow, Symbol, Tagged,
    ThreadState, VM, Value, VmError, extend_lifetime, proxy::Proxy, raise, raise_runtime, rt_try,
};

crate::register_natives! {
    object_to_string,
    object -> object_constructor,
    object_create,
    object_define_properties,
    object_get_prototype_of,
    object_set_prototype_of,
    object_has_own_property,
    object_is,
    object_property_is_enumerable,
    object_proto_get,
    object_proto_set,
    object_get_own_property_names,
    object_get_own_property_descriptor,
    object_define_property,
    object_prevent_extensions,
    object_is_extensible,
    object_seal,
    object_freeze,
    object_is_frozen,
    object_is_sealed,
}

/// `Object.prototype.toString ( )`
/// [ES 20.1.3.6](https://tc39.es/ecma262/#sec-object.prototype.tostring)
/// The builtin tag from internal slots, overridden by a String `@@toStringTag`.
fn object_to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    // May run from another front end (e.g. Kette printing an object).
    let known = &vm.runtime::<JSRuntime>().known;

    fn tag_string<'a>(heap: &'a mut Heap, scope: &HandleScope<'_>, tag: &str) -> Tagged<'a, Value> {
        DenseString::from_utf8(heap, scope, &format!("[object {tag}]"))
            .as_tagged(heap)
            .erase()
    }

    state.handle_scope(|scope| {
        let receiver = scope.handle(call.receiver.as_tagged(heap));
        let value = receiver.as_tagged(heap);
        if value == vm.known().undefined.get(heap) {
            return tag_string(heap, &scope, "Undefined");
        }
        if value == vm.known().null.get(heap) {
            return tag_string(heap, &scope, "Null");
        }
        let object = if value.is_primitive(heap) {
            match Convert::to_object(vm, heap, state, &scope, receiver) {
                Ok(object) => scope.handle(object.as_tagged(heap).erase()),
                // TODO: BigInt wrappers are unimplemented.
                Err(_) => return tag_string(heap, &scope, "Object"),
            }
        } else {
            receiver
        };
        let Some(default_tag) = Object::builtin_tag(heap, object.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::RevokedProxy);
        };
        let symbol = known.to_string_tag_symbol.get_handle(heap, &scope).erase();
        let tag = scope.handle(rt_try!(
            heap,
            Object::get_property(vm, heap, state, object, object, symbol, false)
        ));
        let tag = tag
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|tag| tag.to_rust_string(heap))
            .unwrap_or_else(|| default_tag.to_string());
        tag_string(heap, &scope, &tag)
    })
}

/// `Object ( value )`
/// [ES 20.1.1.1](https://tc39.es/ecma262/#sec-object-value)
/// Boxes primitives; a `new.target` other than `%Object%` creates an object
/// from its prototype and ignores `value`.
fn object_constructor<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let object_index = ctx.runtime::<JSRuntime>().indices.object;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        if let Some(new_target) = call.new_target
            && !is_native_function(heap, new_target.as_tagged(heap), object_index)
        {
            let map = vm.known().object_initial_map.get_handle(heap, &scope);
            let object = rt_try!(
                heap,
                Object::create_from_constructor(
                    vm,
                    heap,
                    state,
                    &scope,
                    new_target,
                    map,
                    HandleSlice::EMPTY,
                )
            );
            return object.as_tagged(heap).erase();
        }
        let value = args.get_handle(heap, 0);
        if value.as_tagged(heap).is_nullish(heap) {
            return heap
                .new_object(
                    &scope,
                    vm.known().object_initial_map.get_handle(heap, &scope),
                    HandleSlice::EMPTY,
                )
                .erase();
        }
        // TODO: BigInt wrappers are unimplemented.
        match Convert::to_object(vm, heap, state, &scope, value) {
            Ok(object) => object.as_tagged(heap).erase(),
            Err(err) => raise_runtime(heap, err),
        }
    })
}

fn is_native_function(heap: &Heap, value: Tagged<'_, Value>, index: NativeIndex) -> bool {
    value
        .as_heap_object()
        .and_then(|object| object.callable_info(heap))
        .and_then(|info| info.native_index(heap))
        == Some(index)
}

/// `Object.getPrototypeOf ( obj )`
/// [ES 20.1.2.12](https://tc39.es/ecma262/#sec-object.getprototypeof)
/// Coerces `obj` with `ToObject`, then returns its [[Prototype]].
fn object_get_prototype_of<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    match Object::get_prototype(vm, heap, state, value) {
        Ok(proto) => extend_lifetime!(heap, proto),
        Err(err) => raise_runtime(heap, err),
    }
}

/// `get Object.prototype.__proto__`
/// [ES 20.1.3.8.1](https://tc39.es/ecma262/#sec-get-object.prototype.__proto__)
/// Coerces `this` with `ToObject`, then returns its [[Prototype]].
fn object_proto_get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = call.receiver;
    match Object::get_prototype(vm, heap, state, value) {
        Ok(proto) => extend_lifetime!(heap, proto),
        Err(err) => raise_runtime(heap, err),
    }
}

/// `set Object.prototype.__proto__`
/// [ES 20.1.3.8.2](https://tc39.es/ecma262/#sec-set-object.prototype.__proto__)
/// Changes [[Prototype]]; primitives and invalid `proto` values are ignored.
fn object_proto_set<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = call.receiver;
    let proto = args.get_handle(heap, 0);
    // RequireObjectCoercible precedes every other step.
    if receiver.as_tagged(heap).is_nullish(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    // A non-object, non-null proto is ignored (step 2).
    if proto.as_tagged(heap).is_primitive(heap)
        && !proto
            .as_tagged(heap)
            .ptr_eq(vm.known().null.get(heap).erase())
    {
        return vm.known().undefined.get(heap).erase();
    }
    if receiver.as_tagged(heap).is_primitive(heap) {
        return vm.known().undefined.get(heap).erase();
    }
    let status = rt_try!(
        heap,
        Proxy::set_prototype_of(vm, heap, state, receiver, proto)
    );
    if !status {
        raise!(vm, heap, state, VmError::ProxySetPrototypeOfFalse);
    }
    vm.known().undefined.get(heap).erase()
}

/// `Object.create ( proto, properties )`
/// [ES 20.1.2.2](https://tc39.es/ecma262/#sec-object.create)
/// A fresh ordinary object with `proto` as its [[Prototype]].
fn object_create<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let proto = args.get_handle(heap, 0);
        // If Type(proto) is neither Object nor Null, throw a TypeError
        if proto.as_tagged(heap).is_primitive(heap)
            && !proto
                .as_tagged(heap)
                .ptr_eq(vm.known().null.get(heap).erase())
        {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let map = vm.known().object_initial_map.get_handle(heap, &scope);
        let obj = heap
            .new_object(&scope, map, HandleSlice::EMPTY)
            .as_handle(&scope);
        rt_try!(
            heap,
            Object::set_prototype(vm, heap, state, &scope, obj, proto)
        );
        let properties = args.get_handle(heap, 1);
        if !properties
            .as_tagged(heap)
            .ptr_eq(vm.known().undefined.get(heap).erase())
        {
            rt_try!(
                heap,
                define_properties(vm, heap, state, &scope, obj, properties)
            );
        }
        obj.as_tagged(heap).erase()
    })
}

/// `Object.prototype.hasOwnProperty ( value )`
/// [ES 20.1.3.2](https://tc39.es/ecma262/#sec-object.prototype.hasownproperty)
/// `HasOwnProperty` after coercing `this`; the key is coerced first.
fn object_has_own_property<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // ToPropertyKey precedes ToObject (ES 20.1.3.2 steps 1-2)
        let raw_key = args.get_handle(heap, 0);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key).erase();
        let receiver = call.receiver;
        let receiver = rt_try!(heap, Convert::to_object(vm, heap, state, &scope, receiver));
        let found = rt_try!(
            heap,
            Object::get_own_property(vm, heap, state, &scope, receiver.erase(), key)
        )
        .is_some();
        Convert::boolean(heap, found)
    })
}

/// `Object.prototype.propertyIsEnumerable ( value )`
/// [ES 20.1.3.4](https://tc39.es/ecma262/#sec-object.prototype.propertyisenumerable)
/// `[[GetOwnProperty]]` of the coerced receiver; only own properties count.
fn object_property_is_enumerable<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // ToPropertyKey precedes ToObject (ES 20.1.3.4 steps 1-2)
        let raw_key = args.get_handle(heap, 0);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key).erase();
        let receiver = call.receiver;
        let receiver = rt_try!(heap, Convert::to_object(vm, heap, state, &scope, receiver));
        let enumerable = match rt_try!(
            heap,
            Object::get_own_property(vm, heap, state, &scope, receiver.erase(), key)
        ) {
            Some(own) => own.desc.is_enumerable(),
            None => false,
        };
        Convert::boolean(heap, enumerable)
    })
}

/// `Object.getOwnPropertyNames ( obj )`
/// [ES 20.1.2.10](https://tc39.es/ecma262/#sec-object.getownpropertynames)
/// `GetOwnPropertyKeys(obj, string)`: the non-symbol keys of the object,
/// in ordinary property order.
fn object_get_own_property_names<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        // GetOwnPropertyKeys starts with ToObject (ES 20.1.2.11.1)
        let target = rt_try!(heap, Convert::to_object(vm, heap, state, &scope, value));
        let keys = rt_try!(
            heap,
            Object::own_property_keys(vm, heap, state, &scope, target.erase())
        );
        let mut names: Vec<Handle<'_, Value>> = Vec::with_capacity(keys.len());
        for key in keys {
            let value = key.as_tagged(heap);
            if value.get_as::<Symbol>(heap).is_some() {
                continue;
            }
            // Internal element keys are Smis; [[OwnPropertyKeys]] yields the
            // string spelling of every array index.
            let name = if value.is_smi() {
                scope.handle(rt_try!(
                    heap,
                    Convert::to_string(vm, heap, state, &scope, key)
                ))
            } else {
                key
            };
            names.push(name);
        }
        let staged = scope.stage_handles(&names);
        heap.new_array(&scope, staged).erase()
    })
}

/// `Object.getOwnPropertyDescriptor ( obj, key )`
/// [ES 20.1.2.7](https://tc39.es/ecma262/#sec-object.getownpropertydescriptor)
/// `[[GetOwnProperty]]` converted with FromPropertyDescriptor.
fn object_get_own_property_descriptor<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let raw_target = args.get_handle(heap, 0);
        let target = rt_try!(
            heap,
            Convert::to_object(vm, heap, state, &scope, raw_target)
        );
        let raw_key = args.get_handle(heap, 1);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key).erase();
        let Some(own) = rt_try!(
            heap,
            Object::get_own_property(vm, heap, state, &scope, target.erase(), key)
        ) else {
            return vm.known().undefined.get(heap).erase();
        };
        let partial = PartialDescriptor::from(&own.desc);
        rt_try!(heap, Object::descriptor_object(heap, &scope, &partial)).as_tagged(heap)
    })
}

/// `ObjectDefineProperties ( obj, properties )`
/// [ES 20.1.2.3.1](https://tc39.es/ecma262/#sec-objectdefineproperties)
/// Collects every enumerable own descriptor first, then defines them all.
fn define_properties<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: Handle<'_, Object>,
    properties: Handle<'_, Value>,
) -> Result<()> {
    let properties = Convert::to_object(vm, heap, state, scope, properties)?.erase();
    let keys = Object::own_property_keys(vm, heap, state, scope, properties)?;
    let mut descriptors: Vec<(Handle<'s, Value>, PartialDescriptor<'s>)> =
        Vec::with_capacity(keys.len());
    for key in keys {
        let Some(current) = Object::get_own_property(vm, heap, state, scope, properties, key)?
        else {
            continue;
        };
        if !current.desc.is_enumerable() {
            continue;
        }
        let desc_obj = Object::get_property(vm, heap, state, properties, properties, key, false)?;
        let desc_obj = scope.handle(desc_obj);
        let partial = Object::to_property_descriptor(vm, heap, state, scope, desc_obj)?;
        descriptors.push((key, partial));
    }
    let obj = scope.handle(obj.as_tagged(heap).erase());
    for (key, partial) in descriptors {
        Object::define_property_or_throw(vm, heap, state, scope, obj, key, partial)?;
    }
    Ok(())
}

/// `Object.defineProperties ( obj, properties )`
/// [ES 20.1.2.3](https://tc39.es/ecma262/#sec-object.defineproperties)
/// Defines own properties from the enumerable descriptors of `properties`.
fn object_define_properties<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let obj = args.get_handle(heap, 0);
        if obj.as_tagged(heap).is_primitive(heap) {
            raise!(vm, heap, state, VmError::CannotDefineOnPrimitive);
        }
        let obj = scope
            .cast::<Object>(heap, obj.as_tagged(heap))
            .expect("checked to be an object");
        let properties = args.get_handle(heap, 1);
        rt_try!(
            heap,
            define_properties(vm, heap, state, &scope, obj, properties)
        );
        obj.as_tagged(heap).erase()
    })
}

// TODO: implement Reflect.defineProperty (ES 28.1.3.3) on top of
// Object::define_property.
/// `Object.defineProperty ( obj, key, attrs )`
/// [ES 20.1.2.4](https://tc39.es/ecma262/#sec-object.defineproperty)
/// ToPropertyDescriptor + [[DefineOwnProperty]] through the
/// `defineProperty` trap for proxies.
fn object_define_property<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // root the target, key, and descriptor across the conversions below
        let target = scope.handle(args.get(heap, 0));
        let key = scope.handle(args.get(heap, 1));
        let attrs = scope.handle(args.get(heap, 2));
        if rt_try!(
            heap,
            Object::define_property(
                vm,
                heap,
                state,
                &scope,
                target,
                key,
                attrs,
                ShouldThrow::Throw,
            )
        ) {
            target.as_tagged(heap).erase()
        } else {
            Errors::raise(vm, heap, state, VmError::CannotDefineProperty)
        }
    })
}

/// `Object.setPrototypeOf ( obj, proto )`
/// [ES 20.1.2.23](https://tc39.es/ecma262/#sec-object.setprototypeof)
/// Sets the [[Prototype]] of `obj`; primitives are returned unchanged after
/// the `proto` argument is validated.
fn object_set_prototype_of<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let obj = args.get(heap, 0);
        let obj = scope.handle(obj);
        let proto = args.get(heap, 1);
        let proto = scope.handle(proto);
        if obj.as_tagged(heap).is_nullish(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        if proto.as_tagged(heap).is_primitive(heap)
            && !proto
                .as_tagged(heap)
                .ptr_eq(vm.known().null.get(heap).erase())
        {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        if obj.as_tagged(heap).is_primitive(heap) {
            return obj.as_tagged(heap).erase();
        }

        let status = rt_try!(heap, Proxy::set_prototype_of(vm, heap, state, obj, proto));
        if !status {
            raise!(vm, heap, state, VmError::ProxySetPrototypeOfFalse);
        }
        obj.as_tagged(heap).erase()
    })
}

/// `Object.preventExtensions ( obj )`
/// [ES 20.1.2.20](https://tc39.es/ecma262/#sec-object.preventextensions)
/// Prevents adding new own properties; primitives are returned unchanged.
fn object_prevent_extensions<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let target = args.get(heap, 0);
        let target = scope.handle(target);
        // the (possibly proxy) receiver is returned after traps ran user
        // code: keep it rooted across the call
        if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
            return target.as_tagged(heap).erase(); // primitives returned unchanged
        }
        let v = rt_try!(heap, Proxy::prevent_extensions(vm, heap, state, target));
        if !v {
            Errors::raise(vm, heap, state, VmError::NotExtensible)
        } else {
            // re-read through the handle: the trap above ran user
            // code and may have moved the receiver
            target.as_tagged(heap).erase()
        }
    })
}

/// `Object.is ( value1, value2 )`
/// [ES 20.1.2.15](https://tc39.es/ecma262/#sec-object.is)
/// SameValue: `NaN` equals `NaN`, `+0` and `-0` are distinct.
fn object_is<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let value1 = args.get(heap, 0);
    let value2 = args.get(heap, 1);
    Convert::boolean(heap, Compare::same_value(heap, value1, value2))
}

/// `Object.isExtensible ( obj )`
/// [ES 20.1.2.16](https://tc39.es/ecma262/#sec-object.isextensible)
/// Non-objects are not extensible.
fn object_is_extensible<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let obj = args.get_handle(heap, 0);
    if !Proxy::is_js_receiver(heap, obj.as_tagged(heap)) {
        return Convert::boolean(heap, false);
    }
    let extensible = rt_try!(heap, Proxy::object_is_extensible(vm, heap, state, obj));
    Convert::boolean(heap, extensible)
}

/// `SetIntegrityLevel ( obj, level )`
/// [ES 7.3.15](https://tc39.es/ecma262/#sec-setintegritylevel)
/// Prevents extensions, then defines every own property as non-configurable
/// (and non-writable for `frozen`). This is the generic internal-method
/// path, so proxies run `ownKeys` / `getOwnPropertyDescriptor` /
/// `defineProperty` traps.
fn set_integrity_level(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    target: Handle<'_, Value>,
    frozen: bool,
) -> Result<bool> {
    let status = Proxy::prevent_extensions(vm, heap, state, target)?;
    if !status {
        return Ok(false);
    }
    let keys = Object::own_property_keys(vm, heap, state, scope, target)?;
    for key in keys {
        let Some(current) = Object::get_own_property(vm, heap, state, scope, target, key)? else {
            continue;
        };
        let partial = if frozen && current.desc.is_data_descriptor() {
            PartialDescriptor {
                writable: Some(false),
                configurable: Some(false),
                ..PartialDescriptor::default()
            }
        } else {
            PartialDescriptor {
                configurable: Some(false),
                ..PartialDescriptor::default()
            }
        };
        Object::define_property_or_throw(vm, heap, state, scope, target, key, partial)?;
    }
    Ok(true)
}

/// `TestIntegrityLevel ( obj, level )`
/// [ES 7.3.16](https://tc39.es/ecma262/#sec-testintegritylevel)
fn test_integrity_level(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    target: Handle<'_, Value>,
    frozen: bool,
) -> Result<bool> {
    if Proxy::object_is_extensible(vm, heap, state, target)? {
        return Ok(false);
    }
    let keys = Object::own_property_keys(vm, heap, state, scope, target)?;
    for key in keys {
        let Some(current) = Object::get_own_property(vm, heap, state, scope, target, key)? else {
            continue;
        };
        if current.desc.is_configurable() {
            return Ok(false);
        }
        if frozen && current.desc.is_data_descriptor() && current.desc.is_writable() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `Object.seal ( obj )`
/// [ES 20.1.2.22](https://tc39.es/ecma262/#sec-object.seal)
/// Prevents extensions and makes every own property non-configurable.
fn object_seal<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
        return target.as_tagged(heap).erase();
    }
    state.handle_scope(|scope| {
        let ok = rt_try!(
            heap,
            set_integrity_level(vm, heap, state, &scope, target, false)
        );
        if !ok {
            raise!(vm, heap, state, VmError::NotExtensible);
        }
        target.as_tagged(heap).erase()
    })
}

/// `Object.freeze ( obj )`
/// [ES 20.1.2.6](https://tc39.es/ecma262/#sec-object.freeze)
/// Prevents extensions and makes every own property non-configurable and
/// non-writable.
fn object_freeze<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
        return target.as_tagged(heap).erase();
    }
    state.handle_scope(|scope| {
        let ok = rt_try!(
            heap,
            set_integrity_level(vm, heap, state, &scope, target, true)
        );
        if !ok {
            raise!(vm, heap, state, VmError::NotExtensible);
        }
        target.as_tagged(heap).erase()
    })
}

/// `Object.isFrozen ( obj )`
/// [ES 20.1.2.17](https://tc39.es/ecma262/#sec-object.isfrozen)
/// Whether the object is non-extensible with no configurable or writable
/// data properties.
fn object_is_frozen<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
        return Convert::boolean(heap, true);
    }
    state.handle_scope(|scope| {
        let frozen = rt_try!(
            heap,
            test_integrity_level(vm, heap, state, &scope, target, true)
        );
        Convert::boolean(heap, frozen)
    })
}

/// `Object.isSealed ( obj )`
/// [ES 20.1.2.18](https://tc39.es/ecma262/#sec-object.issealed)
/// Whether the object is non-extensible with no configurable properties.
fn object_is_sealed<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
        return Convert::boolean(heap, true);
    }
    state.handle_scope(|scope| {
        let frozen = rt_try!(
            heap,
            test_integrity_level(vm, heap, state, &scope, target, false)
        );
        Convert::boolean(heap, frozen)
    })
}
