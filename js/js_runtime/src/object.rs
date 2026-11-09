//! ES 20.1: the Object constructor, statics, and prototype methods.
use vm_core::Errors;
use vm_core::HostCtx;
use vm_core::Result;
use vm_core::VmError;
use vm_core::raise;

use crate::JSRuntime;
use vm_core::proxy::Proxy;

use vm_core::{
    Args, Compare, Convert, DenseString, Handle, HandleScope, HandleSlice, Heap, MapKind, Object,
    PartialDescriptor, PropertyDescriptor, Prototype, ProxyObject, ShouldThrow, SlotName, Smi,
    Tagged, ThreadState, VM, Value, WellKnown, WrapperKind, descriptor_object,
};
use vm_core::{raise_runtime, rt_try};

/// `"[object <tag>]"`, freshly allocated (not interned: the tag may be user text).
fn tag_string<'a>(heap: &'a mut Heap, scope: &HandleScope<'_>, tag: &str) -> Tagged<'a, Value> {
    DenseString::from_utf8(heap, scope, &format!("[object {tag}]"))
        .as_tagged(heap)
        .erase()
}

/// ES 7.2.2 IsArray, including proxy targets (revoked proxies throw).
pub fn is_array<'a>(heap: &'a Heap, mut value: Tagged<'a, Value>) -> Option<bool> {
    loop {
        let Some(object) = value.as_heap_object() else {
            return Some(false);
        };
        let kind = object.map(heap).kind();
        if kind.is_array() {
            return Some(true);
        }
        if !kind.is_proxy() {
            return Some(false);
        }
        let Some(proxy) = value.get_as::<ProxyObject>(heap) else {
            return Some(false);
        };
        let (target, handler) = proxy.parts(heap);
        if handler == heap.known().null.get(heap) {
            return None;
        }
        value = target;
    }
}

/// Builtin tag for the ToObject'd receiver (ES 20.1.3.6 steps 5-14).
fn builtin_tag(heap: &Heap, receiver: Tagged<'_, Value>) -> Option<&'static str> {
    if is_array(heap, receiver)? {
        return Some("Array");
    }
    let Some(object) = receiver.as_heap_object() else {
        return Some("Object");
    };
    let kind = object.map(heap).kind();
    // [[Call]]: includes callable proxies (a proxy around a function).
    if kind.is_callable() {
        return Some("Function");
    }
    if kind.is_error() {
        return Some("Error");
    }
    if kind.contains(MapKind::PRIMITIVE_WRAPPER) {
        // Wrappers carry the primitive in slots[0]; wrapper prototypes are
        // wrappers too (ES 20.3.3, 20.4.3, 20.5.3). Symbols/BigInts have no
        // builtin-tag step; their prototypes supply @@toStringTag.
        let value = object.slots(heap).at(heap, 0);
        return Some(match WrapperKind::classify(heap, heap.known(), value) {
            Some(WrapperKind::Number) => "Number",
            Some(WrapperKind::String) => "String",
            Some(WrapperKind::Boolean) => "Boolean",
            None => "Object",
        });
    }
    if kind.contains(MapKind::DATE) {
        return Some("Date");
    }
    Some("Object")
}

/// `Object.prototype.toString` (ES 20.1.3.6).
pub fn object_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
            match Convert::to_object(vm, heap, state, &scope, receiver) {
                Ok(object) => scope.handle(object.as_tagged(heap).erase()),
                // TODO: BigInt wrappers are unimplemented.
                Err(_) => return tag_string(heap, &scope, "Object"),
            }
        } else {
            receiver
        };
        let Some(default_tag) = builtin_tag(heap, object.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::RevokedProxy);
        };
        // Let tag be ? Get(O, @@toStringTag).
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

pub fn object_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let is_construct = new_target.is_some();
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
        // TODO: BigInt wrappers are unimplemented, so they fall through.
        if !nullish && let Ok(boxed) = Convert::to_object(vm, heap, state, &scope, arg) {
            return boxed.as_tagged(heap).erase();
        }
        if !is_construct && !nullish {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        // Object() / Object(undefined) / Object(null) / new Object(): a
        // fresh ordinary object.
        heap.new_object(
            &scope,
            vm.known().object_initial_map.get_handle(heap, &scope),
            HandleSlice::EMPTY,
        )
        .erase()
    })
}

pub fn object_get_prototype_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        let result = match Proxy::get_prototype_of(vm, heap, state, receiver) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        };
        result.as_tagged(heap)
    })
}

pub fn object_proto_get<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        let result = match Proxy::get_prototype_of(vm, heap, state, receiver) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        };
        result.as_tagged(heap)
    })
}

pub fn object_proto_set<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        if !proto_ok {
            return vm.known().undefined.get(heap).erase();
        }
        if !receiver_is_object {
            return vm.known().undefined.get(heap).erase();
        }
        if Proxy::is_proxy(heap, receiver.as_tagged(heap)) {
            return if rt_try!(
                heap,
                Proxy::set_prototype_of(vm, heap, state, receiver, proto)
            ) {
                vm.known().undefined.get(heap).erase()
            } else {
                Errors::raise(vm, heap, state, VmError::InvalidOperandType)
            };
        }
        let target = scope
            .cast::<Object>(heap, receiver.as_tagged(heap))
            .expect("receiver checked to be an object");
        rt_try!(
            heap,
            Object::set_prototype(vm, heap, state, &scope, target, proto)
        );
        vm.known().undefined.get(heap).erase()
    })
}

/// `Object.create(O [, Properties])` (ES 20.1.2.2): a fresh extensible
/// ordinary object with `O` as its [[Prototype]] and no own properties.
/// The `Properties` argument is accepted only as `undefined` (property
/// descriptors are not implemented for it yet).
pub fn object_create<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let proto_arg = args.get(heap, 1);
        let proto = scope.handle(proto_arg);
        // If Type(O) is neither Object nor Null, throw a TypeError
        let proto_ok = proto
            .as_tagged(heap)
            .ptr_eq(vm.known().null.get(heap).erase())
            || !proto.as_tagged(heap).is_primitive(heap);
        if !proto_ok {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        if args.len() > 2 {
            let props = args.get(heap, 2);
            let undefined = vm.known().undefined.get(heap).raw();
            if props.raw() != undefined {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        let map = vm.known().object_initial_map.get_handle(heap, &scope);
        let empty: [Tagged<'_, Value>; 0] = [];
        let obj = scope.handle(heap.new_object(&scope, map, scope.stage(&empty)));
        let obj = scope
            .cast::<Object>(heap, obj.as_tagged(heap).erase())
            .expect("fresh object");
        rt_try!(
            heap,
            Object::set_prototype(vm, heap, state, &scope, obj, proto)
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
pub fn own_property_keys<'a>(
    heap: &'a Heap,
    roots: &WellKnown,
    target: Tagged<'a, Value>,
) -> Vec<Tagged<'a, Value>> {
    let mut keys = Vec::new();
    // String exotic own keys: indices then the non-enumerable "length"
    // (ES 10.4.3.5). Wrappers may also carry ordinary own descriptors.
    if let Some(s) = DenseString::from_receiver(heap, target) {
        keys.reserve(s.len() + 1);
        for i in 0..s.len() {
            keys.push(Smi::new(i as i64).into_tagged());
        }
        keys.push(roots.strings.length.get(heap).erase());
    }
    // element indices ascending, then named descriptors in insertion
    // order (ES OrdinaryOwnPropertyKeys)
    if let Some(obj) = target.as_heap_object() {
        for i in obj.element_keys(heap) {
            keys.push(Smi::new(i as i64).into_tagged());
        }
        for d in obj.map(heap).descriptors() {
            keys.push(d.name(heap).erase());
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
}

fn own_property(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    key: Handle<'_, Value>,
) -> Result<OwnProperty> {
    Ok(
        match Object::get_own_property(vm, heap, state, scope, receiver, key)? {
            Some(own) => OwnProperty::Found {
                enumerable: match own.desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
                },
            },
            None => OwnProperty::Missing,
        },
    )
}

/// `Object.prototype.hasOwnProperty(key)` (ES 20.4.3.2, own properties
/// only).
pub fn object_has_own_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        // ToObject(this): null/undefined throw (ES 20.4.3.2 step 1)
        if receiver.as_tagged(heap).is_nullish(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let raw_key = scope.handle(args.get(heap, 1));
        // the key coercion allocates (float/wrapper keys intern or run
        // user code): the receiver must stay rooted across it
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let key_value = scope.handle(key.as_tagged(heap).erase());
        match rt_try!(
            heap,
            own_property(vm, heap, state, &scope, receiver, key_value)
        ) {
            OwnProperty::Found { .. } => Convert::boolean(heap, true),
            OwnProperty::Missing => Convert::boolean(heap, false),
        }
    })
}

/// `Object.prototype.propertyIsEnumerable(key)` (ES 20.4.3.5).
pub fn object_property_is_enumerable<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        // ToObject(this): null/undefined throw (ES 20.4.3.5 step 1)
        if receiver.as_tagged(heap).is_nullish(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let raw_key = scope.handle(args.get(heap, 1));
        // the key coercion allocates (float/wrapper keys intern or run
        // user code): the receiver must stay rooted across it
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let key_value = scope.handle(key.as_tagged(heap).erase());
        match rt_try!(
            heap,
            own_property(vm, heap, state, &scope, receiver, key_value)
        ) {
            OwnProperty::Found { enumerable } => Convert::boolean(heap, enumerable),
            OwnProperty::Missing => Convert::boolean(heap, false),
        }
    })
}

/// `Object.getOwnPropertyNames(O)` (ES 20.1.2.7).
pub fn object_get_own_property_names<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 1);
    // array `length` is a real (accessor) descriptor on the array map, so
    // `own_property_keys` already lists it
    state.handle_scope(|scope| {
        let staged = {
            let names = own_property_keys(heap, vm.known(), target.as_tagged(heap));
            scope.stage(&names)
        };
        heap.new_array(&scope, staged).erase()
    })
}

/// `Object.getOwnPropertyDescriptor(O, P)` (ES 20.1.2.5): [[GetOwnProperty]]
/// converted with FromPropertyDescriptor.
pub fn object_get_own_property_descriptor<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // the key coercion allocates (Float keys intern a string): the
        // target must stay rooted across it
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        let raw_key = args.get(heap, 2);
        let raw_key = scope.handle(raw_key);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let key_value = scope.handle(key.as_tagged(heap).erase());
        let Some(own) = rt_try!(
            heap,
            Object::get_own_property(vm, heap, state, &scope, target, key_value)
        ) else {
            return vm.known().undefined.get(heap).erase();
        };
        let partial = PartialDescriptor::from(&own.desc);
        rt_try!(heap, descriptor_object(heap, &scope, &partial)).as_tagged(heap)
    })
}

// TODO: implement Object.defineProperties (ES 20.1.2.3) and
// Reflect.defineProperty (ES 28.1.3.3) on top of Object::define_property.
/// `Object.defineProperty(O, P, Attributes)` (ES 20.1.2.4):
/// ToPropertyDescriptor + [[DefineOwnProperty]] (through the
/// `defineProperty` trap for proxy receivers, ES 20.2.5.6).
pub fn object_define_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // root the target, key, and descriptor across the conversions below
        let target = scope.handle(args.get(heap, 1));
        let key = scope.handle(args.get(heap, 2));
        let attrs = scope.handle(args.get(heap, 3));
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
            Errors::raise(vm, heap, state, VmError::InvalidOperandType)
        }
    })
}

pub fn object_set_prototype_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        // proto must be Object or Null *before* a primitive O is returned
        // (step 2 precedes step 3)
        if !proto_ok {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        // primitives are returned unchanged (step 3)
        if !target_is_object {
            return target.as_tagged(heap).erase();
        }
        if Proxy::is_proxy(heap, target.as_tagged(heap)) {
            return if rt_try!(
                heap,
                Proxy::set_prototype_of(vm, heap, state, target, proto)
            ) {
                target.as_tagged(heap).erase()
            } else {
                Errors::raise(vm, heap, state, VmError::InvalidOperandType)
            };
        }
        let target_obj = scope
            .cast::<Object>(heap, target.as_tagged(heap))
            .expect("target checked to be an object");
        rt_try!(
            heap,
            Object::set_prototype(vm, heap, state, &scope, target_obj, proto)
        );
        target.as_tagged(heap).erase()
    })
}

/// `Object.preventExtensions(O)` (ES 20.1.2.16): through the
/// `preventExtensions` trap for proxies (ES 20.2.5.3).
pub fn object_prevent_extensions<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let target = args.get(heap, 1);
        let target = scope.handle(target);
        if target.as_tagged(heap).is_nullish(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
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

/// `Object.is(A, B)` (ES 20.1.2.13): SameValue, including NaN and ±0.
pub fn object_is<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm: _, heap, state, ..
    } = ctx;
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
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|_scope| {
        let target = args.get_handle(heap, 1);
        if !Proxy::is_js_receiver(heap, target.as_tagged(heap)) {
            return Convert::boolean(heap, false);
        }
        let v = rt_try!(heap, Proxy::object_is_extensible(vm, heap, state, target));
        Convert::boolean(heap, v)
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
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 1);
    if target.as_tagged(heap).is_nullish(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    // traps run user code before the receiver is returned: keep it rooted
    state.handle_scope(|scope| {
        // Safety: fresh argument word, rooted before any allocation.
        let target_handle = target;
        if !Proxy::is_js_receiver(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        // [[PreventExtensions]] first (traps included)
        let v = rt_try!(
            heap,
            Proxy::prevent_extensions(vm, heap, state, target_handle)
        );
        if !v {
            raise!(vm, heap, state, VmError::NotExtensible);
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
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 1);
    if target.as_tagged(heap).is_nullish(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    // traps run user code before the receiver is returned: keep it rooted
    state.handle_scope(|scope| {
        // Safety: fresh argument word, rooted before any allocation.
        let target_handle = target;
        if !Proxy::is_js_receiver(heap, target_handle.as_tagged(heap)) {
            return target_handle.as_tagged(heap).erase();
        }
        let v = rt_try!(
            heap,
            Proxy::prevent_extensions(vm, heap, state, target_handle)
        );
        if !v {
            raise!(vm, heap, state, VmError::NotExtensible);
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
