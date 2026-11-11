use crate::JSRuntime;
use js_compiler::native::NativeIds;
use vm_core::{
    Args, DenseString, FixedArray, Handle, HandleScope, Heap, HostCtx, Object, Result, Smi, Symbol,
    Tagged, ThreadState, VM, Value, VmError, proxy::Proxy, raise, rt_try,
};

crate::register_natives! {
    NativeIds;
    for_in_enumerate,
    for_in_next,
}

/// Enumerator slots (`for_in_enumerator_map`): [level, the level's key
/// snapshot, the snapshot cursor, the registered keys].
const FOR_IN_LEVEL: usize = 0;
const FOR_IN_KEYS: usize = 1;
const FOR_IN_INDEX: usize = 2;
const FOR_IN_VISITED: usize = 3;

/// for-in head (`ForIn/OfHeadEvaluation`, enumerate)
/// [ES 14.7.5.6](https://tc39.es/ecma262/#sec-for-in-and-for-of-statements-runtime-semantics-evaluation)
/// Returns an enumerator for `subject`, or undefined for null/undefined.
pub fn for_in_enumerate<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let subject = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        if subject.as_tagged(heap).is_nullish(heap) {
            return vm.known().undefined.get(heap).erase();
        }
        let Some(level) = for_in_initial_level(heap, subject.as_tagged(heap)) else {
            return vm.known().undefined.get(heap).erase();
        };
        let level = scope.handle(level);
        let keys = rt_try!(heap, for_in_level_keys(vm, heap, state, &scope, level));
        let staged = scope.stage_handles(&keys);
        let keys = heap.allocate_handle::<FixedArray>(staged, &scope);
        let empty = vm.known().empty_fixed_array.get(heap);
        let map = known.for_in_enumerator_map.get_handle(heap, &scope);
        let words = [
            level.as_tagged(heap).erase(),
            keys.as_tagged(heap).erase(),
            Smi::new(0).into_tagged(),
            empty.erase(),
        ];
        let staged = scope.stage(&words);
        let enumerator = heap.new_object(&scope, map, staged);
        enumerator.erase()
    })
}

/// Level 0: the subject itself (a string keeps its virtual index keys), or
/// the intrinsic prototype for the other primitives.
fn for_in_initial_level<'a>(
    heap: &'a Heap,
    subject: Tagged<'a, Value>,
) -> Option<Tagged<'a, Value>> {
    if DenseString::from_receiver(heap, subject).is_some() {
        return Some(subject);
    }
    if subject.is_primitive(heap) {
        return subject.primitive_prototype(heap);
    }
    Some(subject)
}

/// `[[OwnPropertyKeys]]` of one level as interned string keys: symbols are
/// dropped, index keys stringified, order preserved.
fn for_in_level_keys<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    level: Handle<'_, Value>,
) -> Result<Vec<Handle<'s, Value>>> {
    let own = Object::own_property_keys(vm, heap, state, scope, level)?;
    let mut keys: Vec<Handle<'s, Value>> = Vec::with_capacity(own.len());
    for key in own {
        let key = key.as_tagged(heap);
        if key.get_as::<Symbol>(heap).is_some() {
            continue;
        }
        let interned = match Smi::decode(key.raw()) {
            Some(smi) => vm
                .interner()
                .intern_str(heap, scope, &smi.value().to_string()),
            None => {
                let string = scope.handle(
                    key.get_as::<DenseString>(heap)
                        .expect("an own key is a string or a symbol"),
                );
                vm.interner().intern_value(heap, scope, &string)
            }
        };
        keys.push(interned.erase());
    }
    Ok(keys)
}

/// for-in step (`%ForInIteratorPrototype%.next`)
/// [ES 14.7.5.10.2.1](https://tc39.es/ecma262/#sec-%foriniteratorprototype%.next)
/// Returns the next key, or undefined when the chain is exhausted.
pub fn for_in_next<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let enumerator = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        if enumerator
            .as_tagged(heap)
            .ptr_eq(vm.known().undefined.get(heap))
        {
            return vm.known().undefined.get(heap).erase();
        }
        let Some(enumerator) = scope.cast::<Object>(heap, enumerator.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        loop {
            let keys = enumerator
                .as_tagged(heap)
                .slots(heap)
                .at(heap, FOR_IN_KEYS)
                .get_as::<FixedArray>(heap)
                .expect("for-in keys slot");
            let index = Smi::decode(
                enumerator
                    .as_tagged(heap)
                    .slots(heap)
                    .at(heap, FOR_IN_INDEX)
                    .raw(),
            )
            .expect("for-in cursor slot")
            .value() as usize;

            // snapshot exhausted: advance to the prototype and snapshot it
            if index == keys.len() {
                if !rt_try!(heap, for_in_advance(vm, heap, state, &scope, enumerator)) {
                    return vm.known().undefined.get(heap).erase();
                }
                continue;
            }
            let key = scope.handle(keys.at(heap, index));
            enumerator.as_tagged(heap).slots(heap).set(
                heap,
                FOR_IN_INDEX,
                Smi::new(index as i64 + 1).into_tagged(),
            );
            let level = scope.handle(
                enumerator
                    .as_tagged(heap)
                    .slots(heap)
                    .at(heap, FOR_IN_LEVEL),
            );

            let visited = enumerator
                .as_tagged(heap)
                .slots(heap)
                .at(heap, FOR_IN_VISITED)
                .get_as::<FixedArray>(heap)
                .expect("for-in visited slot");
            let raw = key.as_tagged(heap).raw();
            if visited
                .as_slice()
                .iter()
                .any(|slot| slot.get(heap).raw() == raw)
            {
                continue;
            }

            let Some(own) = rt_try!(
                heap,
                Object::get_own_property(vm, heap, state, &scope, level, key)
            ) else {
                continue;
            };
            // register yielded and shadowing keys, each at most once
            let previous = enumerator
                .as_tagged(heap)
                .slots(heap)
                .at(heap, FOR_IN_VISITED)
                .get_as::<FixedArray>(heap)
                .expect("for-in visited slot");
            let mut registered: Vec<Tagged<'_, Value>> = previous
                .as_slice()
                .iter()
                .map(|slot| slot.get(heap))
                .collect();
            registered.push(key.as_tagged(heap));
            let staged = scope.stage(&registered);
            let table = heap.allocate_handle::<FixedArray>(staged, &scope);
            enumerator.as_tagged(heap).slots(heap).set(
                heap,
                FOR_IN_VISITED,
                table.as_tagged(heap).erase(),
            );
            if own.desc.is_enumerable() {
                return key.as_tagged(heap);
            }
        }
    })
}

/// Advance the enumerator to `[[GetPrototypeOf]]` and snapshot that level;
/// `false` when the chain is exhausted.
fn for_in_advance(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    enumerator: Handle<'_, Object>,
) -> Result<bool> {
    let level = scope.handle(
        enumerator
            .as_tagged(heap)
            .slots(heap)
            .at(heap, FOR_IN_LEVEL),
    );
    let Some(proto) = for_in_next_level(vm, heap, state, level)? else {
        return Ok(false);
    };
    let proto = scope.handle(proto);
    let keys = for_in_level_keys(vm, heap, state, scope, proto)?;
    let staged = scope.stage_handles(&keys);
    let keys = heap.allocate_handle::<FixedArray>(staged, scope);
    let slots = enumerator.as_tagged(heap).slots(heap);
    slots.set(heap, FOR_IN_LEVEL, proto.as_tagged(heap).erase());
    slots.set(heap, FOR_IN_KEYS, keys.as_tagged(heap).erase());
    slots.set(heap, FOR_IN_INDEX, Smi::new(0).into_tagged());
    Ok(true)
}

/// `[[GetPrototypeOf]]` of `level`; `None` ends the walk (null, the internal
/// hole, or a Self-style multi-parent list).
fn for_in_next_level<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    level: Handle<'_, Value>,
) -> Result<Option<Tagged<'a, Value>>> {
    if level.as_tagged(heap).is_primitive(heap) {
        return Ok(level.as_tagged(heap).primitive_prototype(heap));
    }
    if Proxy::is_proxy(heap, level.as_tagged(heap)) {
        let null = heap.known().null.get(heap).raw();
        let proto = Proxy::get_prototype_of(vm, heap, state, level)?;
        return Ok((proto.raw() != null).then_some(proto));
    }
    let Some(object) = level.as_tagged(heap).as_heap_object() else {
        return Ok(None);
    };
    let proto = object.prototype(heap);
    let known = heap.known();
    if proto.ptr_eq(known.the_hole.get(heap)) || proto.ptr_eq(known.null.get(heap)) {
        return Ok(None);
    }
    Ok(proto.get_as::<FixedArray>(heap).is_none().then_some(proto))
}
