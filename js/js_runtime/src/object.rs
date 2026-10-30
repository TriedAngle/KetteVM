//! ES 20.1: the Object constructor, statics, and prototype methods.
use vm_core::Coercion;
use vm_core::HostCtx;
use vm_core::proxy::Flow;
use vm_core::proxy::Proxy;

use vm_core::{
    Args, Compare, Convert, DenseString, Float, Handle, HandleScope, HandleSlice, Heap, MapKind,
    Object, PropertyDescriptor, Prototype, ProxyObject, SlotName, Smi, StringOwn, Symbol, Tagged,
    ThreadState, VM, Value, VmError, WellKnown,
};
use vm_core::{raise_runtime, rt_try};

/// `"[object <tag>]"`, freshly allocated (not interned: the tag may be user text).
fn tag_string<'a>(heap: &'a mut Heap, scope: &HandleScope<'_>, tag: &str) -> Tagged<'a, Value> {
    DenseString::from_utf8(heap, scope, &format!("[object {tag}]"))
        .as_tagged(heap)
        .erase()
}

/// ES 7.2.2 IsArray, including proxy targets (revoked proxies throw).
fn is_array<'a>(heap: &'a Heap, mut value: Tagged<'a, Value>) -> Result<bool, VmError> {
    loop {
        let Some(object) = value.as_heap_object() else {
            return Ok(false);
        };
        let kind = object.map(heap).kind();
        if kind.is_array() {
            return Ok(true);
        }
        if !kind.is_proxy() {
            return Ok(false);
        }
        let Some(proxy) = value.get_as::<ProxyObject>(heap) else {
            return Ok(false);
        };
        let (target, handler) = proxy.parts(heap);
        if handler == heap.known().null.get(heap) {
            return Err(VmError::Type);
        }
        value = target;
    }
}

/// Builtin tag for the ToObject'd receiver (ES 20.1.3.6 steps 5-14).
fn builtin_tag(heap: &Heap, receiver: Tagged<'_, Value>) -> Result<&'static str, VmError> {
    if is_array(heap, receiver)? {
        return Ok("Array");
    }
    let Some(object) = receiver.as_heap_object() else {
        return Ok("Object");
    };
    let kind = object.map(heap).kind();
    // [[Call]]: includes callable proxies (a proxy around a function).
    if kind.is_callable() {
        return Ok("Function");
    }
    if kind.is_error() {
        return Ok("Error");
    }
    if kind.contains(MapKind::PRIMITIVE_WRAPPER) {
        // Wrappers carry the primitive in slots[0]; wrapper prototypes are
        // wrappers too (ES 20.3.3, 20.4.3, 20.5.3).
        let value = object.slots(heap).at(heap, 0);
        let known = heap.known();
        if value.is_smi() || value.get_as::<Float>(heap).is_some() {
            return Ok("Number");
        }
        if value.get_as::<DenseString>(heap).is_some() {
            return Ok("String");
        }
        if value == known.true_object.get(heap) || value == known.false_object.get(heap) {
            return Ok("Boolean");
        }
        // Symbols/BigInts have no builtin-tag step; their prototypes supply
        // @@toStringTag.
    }
    if kind.contains(MapKind::DATE) {
        return Ok("Date");
    }
    Ok("Object")
}

/// `Object.prototype.toString` (ES 20.1.3.6).
pub fn object_to_string<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        let value = receiver.as_tagged(heap);
        if value == vm.known().undefined.get(heap) {
            return tag_string(heap, &scope, "Undefined");
        }
        if value == vm.known().null.get(heap) {
            return tag_string(heap, &scope, "Null");
        }
        // ToObject(this): box primitives so the @@toStringTag lookup (and
        // any getter's receiver) sees the wrapper, as the spec requires.
        let object = if value.is_primitive(heap) {
            match box_primitive(vm, heap, &scope, receiver) {
                Some(object) => scope.handle(object.as_tagged(heap).erase()),
                None => return tag_string(heap, &scope, "Object"),
            }
        } else {
            receiver
        };
        let default_tag = match builtin_tag(heap, object.as_tagged(heap)) {
            Ok(tag) => tag,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        // Let tag be ? Get(O, @@toStringTag).
        let symbol = scope.handle(vm.known().to_string_tag_symbol.get(heap).erase());
        let tag = match rt_try!(
            vm,
            heap,
            state,
            Object::get_property(vm, heap, state, object, object, symbol, false)
        ) {
            Coercion::Threw => return vm.known().exception.get(heap).erase(),
            Coercion::Value(tag) => scope.handle(tag),
        };
        let tag = tag
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|tag| tag.to_rust_string(heap))
            .unwrap_or_else(|| default_tag.to_string());
        tag_string(heap, &scope, &tag)
    })
}

pub fn object_constructor<'a>(
    nctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let is_construct = new_target.is_some();
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let arg = args.get_handle(heap, 1);
    if !arg.as_tagged(heap).is_primitive(heap) {
        // Object(obj) / new Object(obj) return the object unchanged
        // (ES 20.1.1.1 step 3).
        return arg.as_tagged(heap);
    }
    let nullish = arg.as_tagged(heap).is_nullish(heap);
    state.handle_scope(|scope| {
        // String/Number/Boolean/Symbol primitives become their wrapper
        // objects (ES 20.1.1.1 step 3).
        if !nullish && let Some(boxed) = box_primitive(vm, heap, &scope, arg) {
            return boxed.as_tagged(heap).erase();
        }
        if !is_construct && !nullish {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // Object() / Object(undefined) / Object(null) / new Object(): a
        // fresh ordinary object.
        heap.new_object(
            &scope,
            scope.handle(vm.known().object_initial_map.get(heap)),
            HandleSlice::EMPTY,
        )
        .erase()
    })
}

/// ToObject for the primitive wrapper kinds this VM has maps for.
fn box_primitive<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    arg: Handle<'_, Value>,
) -> Option<Handle<'s, Object>> {
    let known = vm.known();
    let map = {
        let value = arg.as_tagged(heap);
        if value.get_as::<DenseString>(heap).is_some() {
            known.string_wrapper_map.get(heap)
        } else if value.is_smi() || value.get_as::<Float>(heap).is_some() {
            known.number_wrapper_map.get(heap)
        } else if value == known.true_object.get(heap) || value == known.false_object.get(heap) {
            known.boolean_wrapper_map.get(heap)
        } else if value.get_as::<Symbol>(heap).is_some() {
            known.symbol_wrapper_map.get(heap)
        } else {
            return None;
        }
    };
    let map = scope.handle(map);
    let value = scope.handle(arg.as_tagged(heap));
    let object = heap.new_object(scope, map, scope.stage(&[value.as_tagged(heap)]));
    Some(scope.handle(object))
}

pub fn object_get_prototype_of<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 1));
        {
            let value = receiver.as_tagged(heap);
            if let Some(proto) = value.primitive_prototype(heap) {
                let proto = scope.handle(proto);
                return proto.as_tagged(heap);
            }
            if value.is_primitive(heap) {
                // null / undefined (and symbols, which have no exposed
                // prototype in this VM): ToObject throws
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }
        let exception = scope.handle(vm.known().exception.get(heap).erase());
        let result = match Proxy::get_prototype_of(vm, heap, state, receiver) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => exception,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        result.as_tagged(heap)
    })
}

pub fn object_proto_get<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        {
            let value = receiver.as_tagged(heap);
            if let Some(proto) = value.primitive_prototype(heap) {
                let proto = scope.handle(proto);
                return proto.as_tagged(heap);
            }
            if value.is_primitive(heap) {
                // null / undefined (and symbols, which have no exposed
                // prototype in this VM): ToObject throws
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }
        let exception = scope.handle(vm.known().exception.get(heap).erase());
        let result = match Proxy::get_prototype_of(vm, heap, state, receiver) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => exception,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        result.as_tagged(heap)
    })
}

pub fn object_proto_set<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        let proto = scope.handle(args.get(heap, 1));
        let nullish = receiver.as_tagged(heap).is_nullish(heap);
        let receiver_is_object = !receiver.as_tagged(heap).is_primitive(heap);
        let proto_ok = proto
            .as_tagged(heap)
            .ptr_eq(vm.known().null.get(heap).erase())
            || !proto.as_tagged(heap).is_primitive(heap);
        if nullish {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        if !proto_ok {
            return vm.known().undefined.get(heap).erase();
        }
        if !receiver_is_object {
            return vm.known().undefined.get(heap).erase();
        }
        if Proxy::is_proxy(heap, receiver.as_tagged(heap)) {
            return match rt_try!(
                vm,
                heap,
                state,
                Proxy::set_prototype_of(vm, heap, state, receiver, proto)
            ) {
                Flow::Threw => vm.known().exception.get(heap).erase(),
                Flow::Value(false) => raise_runtime(vm, heap, state, VmError::Type),
                Flow::Value(true) => vm.known().undefined.get(heap).erase(),
            };
        }
        let target = scope
            .cast::<Object>(heap, receiver.as_tagged(heap))
            .expect("receiver checked to be an object");
        rt_try!(
            vm,
            heap,
            state,
            Object::set_prototype(heap, &scope, target, proto)
        );
        vm.known().undefined.get(heap).erase()
    })
}

/// `Object.create(O [, Properties])` (ES 20.1.2.2): a fresh extensible
/// ordinary object with `O` as its [[Prototype]] and no own properties.
/// The `Properties` argument is accepted only as `undefined` (property
/// descriptors are not implemented for it yet).
pub fn object_create<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let proto_arg = args.get(heap, 1);
        let proto = scope.handle(proto_arg);
        // If Type(O) is neither Object nor Null, throw a TypeError
        let proto_ok = proto
            .as_tagged(heap)
            .ptr_eq(vm.known().null.get(heap).erase())
            || !proto.as_tagged(heap).is_primitive(heap);
        if !proto_ok {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        if args.len() > 2 {
            let props = args.get(heap, 2);
            let undefined = vm.known().undefined.get(heap).raw();
            if props.raw() != undefined {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }
        let map = scope.handle(vm.known().object_initial_map.get(heap));
        let empty: [Tagged<'_, Value>; 0] = [];
        let obj = scope.handle(heap.new_object(&scope, map, scope.stage(&empty)));
        let obj = scope
            .cast::<Object>(heap, obj.as_tagged(heap).erase())
            .expect("fresh object");
        rt_try!(
            vm,
            heap,
            state,
            Object::set_prototype(heap, &scope, obj, proto)
        );
        obj.as_tagged(heap).erase()
    })
}

/// `Object.setPrototypeOf(O, proto)` (ES 20.1.2.20): primitives return O
/// unchanged (after RequireObjectCoercible); proto must be an object or
/// null; the underlying [[SetPrototypeOf]] may reject (non-extensible
/// receiver, prototype cycles) with a TypeError.
/// Own enumerable-property keys in specification order: integer indices
/// ascending, then string keys in insertion order (ES 8.6.2, the
/// descriptors array is insertion-ordered).
pub fn own_property_keys(heap: &Heap, roots: &WellKnown, target: Tagged<'_, Value>) -> Vec<Value> {
    let mut keys = Vec::new();
    // String exotic own keys: indices then the non-enumerable "length"
    // (ES 10.4.3.5). Wrappers may also carry ordinary own descriptors.
    if let Some(s) = DenseString::from_receiver(heap, target) {
        keys.reserve(s.len() + 1);
        for i in 0..s.len() {
            keys.push(Smi::new(i as i64).encode());
        }
        keys.push(roots.strings.length.raw());
    }
    // element indices ascending, then named descriptors in insertion
    // order (ES OrdinaryOwnPropertyKeys)
    if let Some(obj) = target.as_heap_object() {
        for i in obj.element_keys(heap) {
            keys.push(Smi::new(i as i64).encode());
        }
        for d in obj.map(heap).descriptors() {
            keys.push(d.name(heap).raw());
        }
    }
    keys
}

/// One own-property lookup over every receiver shape (string exotic,
/// proxy trap, ordinary object). `Threw` means user code threw and the
/// exception sentinel is pending.
enum OwnProperty {
    Found { enumerable: bool },
    Missing,
    Threw,
}

fn own_property(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    key: Handle<'_, SlotName>,
    key_value: Handle<'_, Value>,
) -> Result<OwnProperty, VmError> {
    if let Some(s) = DenseString::from_receiver(heap, receiver.as_tagged(heap))
        && let Some(own) = s.as_ref().own_key(heap, key.as_tagged(heap))
    {
        return Ok(match own {
            StringOwn::Index(_) => OwnProperty::Found { enumerable: true },
            StringOwn::Length(_) => OwnProperty::Found { enumerable: false },
        });
    }
    if Proxy::is_proxy(heap, receiver.as_tagged(heap)) {
        return Ok(
            match Proxy::own_descriptor(vm, heap, state, scope, receiver, key_value)? {
                Flow::Threw => OwnProperty::Threw,
                Flow::Value(Some(desc)) => OwnProperty::Found {
                    enumerable: desc.enumerable == Some(true),
                },
                Flow::Value(None) => OwnProperty::Missing,
            },
        );
    }
    Ok(
        match Object::ordinary_own_descriptor(
            heap,
            scope,
            receiver.as_tagged(heap),
            key_value.as_tagged(heap),
        ) {
            Some(PropertyDescriptor::Data { enumerable, .. })
            | Some(PropertyDescriptor::Accessor { enumerable, .. }) => {
                OwnProperty::Found { enumerable }
            }
            None => OwnProperty::Missing,
        },
    )
}

/// `Object.prototype.hasOwnProperty(key)` (ES 20.4.3.2, own properties
/// only).
pub fn object_has_own_property<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        // ToObject(this): null/undefined throw (ES 20.4.3.2 step 1)
        if receiver.as_tagged(heap).is_nullish(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let raw_key = scope.handle(args.get(heap, 1));
        // the key coercion allocates (float/wrapper keys intern or run
        // user code): the receiver must stay rooted across it
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let key_value = scope.handle(key.as_tagged(heap).erase());
        match own_property(vm, heap, state, &scope, receiver, key, key_value) {
            Ok(OwnProperty::Found { .. }) => Convert::boolean(heap, true),
            Ok(OwnProperty::Missing) => Convert::boolean(heap, false),
            Ok(OwnProperty::Threw) => vm.known().exception.get(heap).erase(),
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    })
}

/// `Object.prototype.propertyIsEnumerable(key)` (ES 20.4.3.5).
pub fn object_property_is_enumerable<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        // ToObject(this): null/undefined throw (ES 20.4.3.5 step 1)
        if receiver.as_tagged(heap).is_nullish(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let raw_key = scope.handle(args.get(heap, 1));
        // the key coercion allocates (float/wrapper keys intern or run
        // user code): the receiver must stay rooted across it
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let key_value = scope.handle(key.as_tagged(heap).erase());
        match own_property(vm, heap, state, &scope, receiver, key, key_value) {
            Ok(OwnProperty::Found { enumerable }) => Convert::boolean(heap, enumerable),
            Ok(OwnProperty::Missing) => Convert::boolean(heap, false),
            Ok(OwnProperty::Threw) => vm.known().exception.get(heap).erase(),
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    })
}

/// `Object.getOwnPropertyNames(O)` (ES 20.1.2.7).
pub fn object_get_own_property_names<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let target = args.get(heap, 1);
    let target = target;
    // array `length` is a real (accessor) descriptor on the array map, so
    // `own_property_keys` already lists it
    let names: Vec<Value> = own_property_keys(heap, vm.known(), target);
    state.handle_scope(|scope| {
        let staged = scope.stage(
            &names
                .iter()
                .map(|v| unsafe { Tagged::<Value>::from_value_unchecked(*v) })
                .collect::<Vec<_>>(),
        );
        heap.new_array(&scope, staged).erase()
    })
}

/// Build a plain `{ key: value, ... }` object from static field names.
pub fn plain_object<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    fields: &[(&'static str, Handle<'_, Value>)],
) -> Result<Tagged<'a, Value>, VmError> {
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().object_initial_map.get(heap));
        let obj = heap
            .new_object(&scope, map, HandleSlice::EMPTY)
            .as_handle(&scope);
        for (name, value) in fields {
            let name = vm.interner().intern_str(heap, &scope, name);
            let name = scope.handle(name.as_tagged(heap));
            Object::define_own_property(heap, &scope, obj, name, PropertyDescriptor::data(*value))?;
        }
        Ok(obj.as_tagged(heap).erase())
    })
}

/// `Object.getOwnPropertyDescriptor(O, P)` (ES 20.1.2.5): the shared
/// raw descriptor reader (`ordinary_own_descriptor`) converted
/// to a descriptor object via FromPropertyDescriptor semantics.
pub fn object_get_own_property_descriptor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        // the key coercion allocates (Float keys intern a string): the
        // target must stay rooted across it
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        let raw_key = args.get(heap, 2);
        let raw_key = scope.handle(raw_key);
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        // Safety: both are rooted in `scope`; the values are re-anchored so
        // the &mut heap borrow below does not overlap the target/key reads.
        let target_v = unsafe { Tagged::<Value>::from_value_unchecked(target.raw()) };
        let key_v = unsafe { Tagged::<Value>::from_value_unchecked(key.raw()) };
        // String exotic own keys (`length`, in-range indices) take
        // precedence; a String wrapper may also carry ordinary own
        // descriptors, so fall through when the key is neither.
        let desc = DenseString::own_descriptor(heap, &scope, target_v, key_v)
            .or_else(|| Object::ordinary_own_descriptor(heap, &scope, target_v, key_v));
        // root the oddball singletons once for the descriptor fields
        let true_v = scope.handle(vm.known().true_object.get(heap).erase());
        let false_v = scope.handle(vm.known().false_object.get(heap).erase());
        let bool_ = |b| if b { true_v } else { false_v };
        match desc {
            Some(PropertyDescriptor::Data {
                value,
                writable,
                enumerable,
                configurable,
            }) => match plain_object(
                vm,
                heap,
                state,
                &[
                    ("value", value),
                    ("writable", bool_(writable)),
                    ("enumerable", bool_(enumerable)),
                    ("configurable", bool_(configurable)),
                ],
            )
            .map(|v| v.raw())
            {
                Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
                Err(err) => return raise_runtime(vm, heap, state, err),
            },
            Some(PropertyDescriptor::Accessor {
                get,
                set,
                enumerable,
                configurable,
            }) => match plain_object(
                vm,
                heap,
                state,
                &[
                    ("get", get),
                    ("set", set),
                    ("enumerable", bool_(enumerable)),
                    ("configurable", bool_(configurable)),
                ],
            )
            .map(|v| v.raw())
            {
                Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
                Err(err) => return raise_runtime(vm, heap, state, err),
            },
            None => vm.known().undefined.get(heap).erase(),
        }
    })
}

/// `Object.defineProperty(O, P, Attributes)` (ES 20.1.2.4):
/// ToPropertyDescriptor + [[DefineOwnProperty]] (through the
/// `defineProperty` trap for proxy receivers, ES 20.2.5.6).
pub fn object_define_property<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        // root the target, key, and descriptor: the key coercion and the
        // descriptor conversion below allocate (user getters run), which
        // would leave raw copies stale
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        let attrs = args.get(heap, 3);
        let attrs = scope.handle(attrs);
        let raw_key = args.get(heap, 2);
        let raw_key = scope.handle(raw_key);
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        // shared ToPropertyDescriptor; proxies and ordinary targets both
        // complete/validate inside define_own_property
        let partial = match rt_try!(
            vm,
            heap,
            state,
            Object::to_property_descriptor(vm, heap, state, &scope, attrs)
        ) {
            Some(partial) => partial,
            None => return vm.known().exception.get(heap).erase(),
        };
        match rt_try!(
            vm,
            heap,
            state,
            Proxy::define_own_property(vm, heap, state, &scope, target, key.erase(), partial)
        ) {
            Flow::Threw => vm.known().exception.get(heap).erase(),
            Flow::Value(false) => raise_runtime(vm, heap, state, VmError::Type),
            Flow::Value(true) => target.as_tagged(heap).erase(),
        }
    })
}

pub fn object_set_prototype_of<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        let proto = args.get(heap, 2);
        let proto = scope.handle(proto);
        let nullish = target.as_tagged(heap).is_nullish(heap);
        let target_is_object = !target.as_tagged(heap).is_primitive(heap);
        let proto_ok = proto
            .as_tagged(heap)
            .ptr_eq(vm.known().null.get(heap).erase())
            || !proto.as_tagged(heap).is_primitive(heap);
        // RequireObjectCoercible(O) (ES 20.1.2.20 step 1)
        if nullish {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // proto must be Object or Null *before* a primitive O is returned
        // (step 2 precedes step 3)
        if !proto_ok {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // primitives are returned unchanged (step 3)
        if !target_is_object {
            return target.as_tagged(heap).erase();
        }
        if Proxy::is_proxy(heap, target.as_tagged(heap)) {
            return match rt_try!(
                vm,
                heap,
                state,
                Proxy::set_prototype_of(vm, heap, state, target, proto)
            ) {
                Flow::Threw => vm.known().exception.get(heap).erase(),
                Flow::Value(false) => raise_runtime(vm, heap, state, VmError::Type),
                Flow::Value(true) => target.as_tagged(heap).erase(),
            };
        }
        let target_obj = scope
            .cast::<Object>(heap, target.as_tagged(heap))
            .expect("target checked to be an object");
        rt_try!(
            vm,
            heap,
            state,
            Object::set_prototype(heap, &scope, target_obj, proto)
        );
        target.as_tagged(heap).erase()
    })
}

/// `Object.preventExtensions(O)` (ES 20.1.2.16): through the
/// `preventExtensions` trap for proxies (ES 20.2.5.3).
pub fn object_prevent_extensions<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        if target.as_tagged(heap).is_nullish(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // the (possibly proxy) receiver is returned after traps ran user
        // code: keep it rooted across the call
        if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
            return target.as_tagged(heap).erase(); // primitives returned unchanged
        }
        let raw = match rt_try!(
            vm,
            heap,
            state,
            Proxy::prevent_extensions(vm, heap, state, target)
        ) {
            Coercion::Threw => None,
            Coercion::Value(v) => Some(v.raw()),
        };
        let Some(raw) = raw else {
            return vm.known().exception.get(heap).erase();
        };
        let v = scope.handle(unsafe { Tagged::<Value>::from_value_unchecked(raw) });
        let cond_39 = v.as_tagged(heap).is_truthy(heap);
        if !cond_39 {
            raise_runtime(vm, heap, state, VmError::NotExtensible)
        } else {
            // re-read through the handle: the trap above ran user
            // code and may have moved the receiver
            target.as_tagged(heap).erase()
        }
    })
}

/// `Object.is(A, B)` (ES 20.1.2.13): SameValue, including NaN and ±0.
pub fn object_is<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { heap, state, .. } = nctx;
    state.handle_scope(|scope| {
        let a = args.get_handle(heap, 1);
        let b = args.get_handle(heap, 2);
        let a = scope.handle(a.as_tagged(heap).erase());
        let b = scope.handle(b.as_tagged(heap).erase());
        Convert::boolean(
            heap,
            Compare::same_value(heap, a.as_tagged(heap), b.as_tagged(heap)),
        )
    })
}

/// `Object.isExtensible(O)` (ES 20.1.2.14): primitives are `false`;
/// proxies run the `isExtensible` trap with its must-match invariant.
pub fn object_is_extensible<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let target = args.get_handle(heap, 1);
        if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
            return Convert::boolean(heap, false);
        }
        // Safety: rooted argument word, consumed by the call.
        let target = unsafe { Tagged::<Value>::from_value_unchecked(target.raw()) };
        let extensible = match rt_try!(
            vm,
            heap,
            state,
            Proxy::object_is_extensible(vm, heap, state, target)
        ) {
            Coercion::Threw => None,
            Coercion::Value(v) => Some(scope.handle(v)),
        };
        let Some(v) = extensible else {
            return vm.known().exception.get(heap).erase();
        };
        v.as_tagged(heap).erase()
    })
}

/// SetIntegrityLevel (ES 7.3.15/16) for ordinary objects: clone the map
/// with `configurable` (and for freeze `writable`) cleared on every
/// descriptor and EXTENDABLE dropped. Dense array elements keep their
/// intrinsic attributes (TODO: element sealing with the elements
/// machinery).
pub fn set_integrity_flags(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    obj: Handle<'_, Object>,
    freeze: bool,
) {
    use vm_core::{Map, MapInit, MapKind, SlotFlags};
    let (kind, descriptor_count, already) = {
        let map = obj.as_tagged(heap).map(heap);
        (
            map.kind(),
            map.descriptors().len(),
            !map.kind().is_extendable()
                && map.descriptors().iter().all(|d| {
                    let flags = d.flags();
                    !flags.is_configurable()
                        && (!freeze || flags.is_accessor() || !flags.is_writable())
                }),
        )
    };
    if already {
        return;
    }
    // clone the map with `configurable` (and for freeze `writable`)
    // cleared on every descriptor and EXTENDABLE dropped: names are
    // read under the enter-heap anchor so the descriptor rows stay
    // anchored across the (allocating) map build
    heap.allocate_token_enter_heap(Map::layout_for(descriptor_count), |token, heap| {
        let obj_ref = obj.as_tagged(heap);
        let map = obj_ref.map(heap);
        // Safety: fresh map-slot word, rooted below before the allocation.
        let prototype = scope.handle(map.prototype.get(heap));
        let descriptors: Vec<(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>)> = map
            .descriptors()
            .iter()
            .map(|d| {
                let mut flags = d.flags();
                flags = SlotFlags::new(flags.bits() & !SlotFlags::CONFIGURABLE.bits());
                if freeze && !flags.is_accessor() {
                    flags = SlotFlags::new(flags.bits() & !SlotFlags::WRITABLE.bits());
                }
                (
                    scope.handle(d.name(heap)),
                    flags,
                    scope.handle(d.value.get(heap)),
                )
            })
            .collect();
        let new_map = token.allocate::<Map>(MapInit {
            kind: MapKind::new(kind.bits() & !MapKind::EXTENDABLE.bits()),
            value_slot_count: map.value_slot_count(),
            descriptors: &descriptors,
            prototype,
        });
        Prototype::shape_changed(heap, obj_ref.map(heap));
        obj_ref
            .header
            .map
            .set(heap, obj.as_tagged(heap).erase(), new_map);
    });
}

/// `Object.seal(O)` (ES 20.1.2.17).
pub fn object_seal<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let target = args.get_handle(heap, 1);
    if target.as_tagged(heap).is_nullish(heap) {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    // traps run user code before the receiver is returned: keep it rooted
    state.handle_scope(|scope| {
        // Safety: fresh argument word, rooted before any allocation.
        let target_handle = target;
        if !Proxy::is_js_receiver(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        // [[PreventExtensions]] first (traps included)
        let raw = match rt_try!(
            vm,
            heap,
            state,
            Proxy::prevent_extensions(vm, heap, state, target_handle)
        ) {
            Coercion::Threw => None,
            Coercion::Value(v) => Some(v.raw()),
        };
        let Some(raw) = raw else {
            return vm.known().exception.get(heap).erase();
        };
        let v = scope.handle(unsafe { Tagged::<Value>::from_value_unchecked(raw) });
        let cond_42 = v.as_tagged(heap).is_truthy(heap);
        if !cond_42 {
            return raise_runtime(vm, heap, state, VmError::NotExtensible);
        }
        // TODO: per-key [[DefineOwnProperty]] through the defineProperty
        // trap once ownKeys lands (proxy targets); ordinary targets:
        // re-read through the handle: the trap may have moved the receiver
        if Proxy::is_proxy(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        let obj = scope
            .cast::<Object>(heap, target_handle.as_tagged(heap))
            .expect("checked above");
        set_integrity_flags(heap, &scope, obj, false);
        // re-read through the handle: traps may have moved the receiver
        target_handle.as_tagged(heap).erase()
    })
}

/// `Object.freeze(O)` (ES 20.1.2.9).
pub fn object_freeze<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let target = args.get_handle(heap, 1);
    if target.as_tagged(heap).is_nullish(heap) {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    // traps run user code before the receiver is returned: keep it rooted
    state.handle_scope(|scope| {
        // Safety: fresh argument word, rooted before any allocation.
        let target_handle = target;
        if !Proxy::is_js_receiver(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        let raw = match rt_try!(
            vm,
            heap,
            state,
            Proxy::prevent_extensions(vm, heap, state, target_handle)
        ) {
            Coercion::Threw => None,
            Coercion::Value(v) => Some(v.raw()),
        };
        let Some(raw) = raw else {
            return vm.known().exception.get(heap).erase();
        };
        let v = scope.handle(unsafe { Tagged::<Value>::from_value_unchecked(raw) });
        let cond_45 = v.as_tagged(heap).is_truthy(heap);
        if !cond_45 {
            return raise_runtime(vm, heap, state, VmError::NotExtensible);
        }
        // re-read through the handle: the trap may have moved the receiver
        if Proxy::is_proxy(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        let obj = scope
            .cast::<Object>(heap, target_handle.as_tagged(heap))
            .expect("checked above");
        set_integrity_flags(heap, &scope, obj, true);
        // re-read through the handle: traps may have moved the receiver
        target_handle.as_tagged(heap).erase()
    })
}
