//! for-in enumeration (ES 14.7.5): a hidden enumerator object that walks
//! the prototype chain lazily, one own-key snapshot per level.

use crate::JSRuntime;
use vm_core::lookup::own_property_enumerable;
use vm_core::{
    Args, DenseString, FixedArray, Float, Handle, HandleScope, Heap, HostCtx, Object, Smi, Symbol,
    Tagged, VM, Value, VmError, WellKnown, raise_runtime, rt_try,
};

/// Enumerator slot layout (a hidden object of `for_in_enumerator_map`):
/// [0] the current prototype-chain level (an object, or a string
///     primitive for level 0 of string subjects),
/// [1] the level's own-string-key snapshot (a FixedArray of interned
///     strings, taken when the level is reached),
/// [2] the snapshot cursor (Smi),
/// [3] keys already registered (a FixedArray; yielded keys and
///     non-enumerable shadowing keys both enter it, ES 14.7.5.9).
const FOR_IN_LEVEL: usize = 0;
const FOR_IN_KEYS: usize = 1;
const FOR_IN_INDEX: usize = 2;
const FOR_IN_VISITED: usize = 3;

/// for-in head (ES 14.7.5.6 ForIn/OfHeadEvaluation, enumerate):
/// (subject) -> enumerator | undefined. null/undefined subjects run
/// zero iterations; objects and strings snapshot level 0 of the lazy
/// chain walk. Other primitives' prototypes are not walked yet (their
/// own properties are none, so they enumerate empty).
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
        let subject = subject;
        if subject.as_tagged(heap).ptr_eq(vm.known().null.get(heap))
            || subject
                .as_tagged(heap)
                .ptr_eq(vm.known().undefined.get(heap))
        {
            return vm.known().undefined.get(heap).erase();
        }
        let Some(level) =
            for_in_initial_level(heap, &scope, vm.known(), known, subject.as_tagged(heap))
        else {
            return vm.known().undefined.get(heap).erase();
        };
        // the level must survive the key collection and FixedArray
        // allocation below (both allocate)
        let level = scope.handle(level);
        let keys = rt_try!(vm, heap, state, for_in_level_keys(vm, heap, &scope, level));
        let staged = scope.stage_handles(&keys);
        let keys = heap.allocate_handle::<FixedArray>(staged, &scope);
        let empty = vm.known().empty_fixed_array.get(heap);
        let map = scope.handle(known.for_in_enumerator_map.get(heap));
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

/// Level 0 of the chain for a subject: objects are their own level 0;
/// string primitives enumerate their indices (a fresh wrapper would be
/// unobservable otherwise). Other primitives have no own properties —
/// their level 0 is the constructor's prototype, so additions to
/// `Number.prototype` etc. are observable (ES 14.7.5.9: the walk starts
/// at ToObject(subject)). `None` when the prototype is unreachable.
fn for_in_initial_level<'a, 's>(
    heap: &'a Heap,
    scope: &'s HandleScope<'_>,
    roots: &WellKnown,
    known: &crate::known::JsKnown,
    subject: Tagged<'a, Value>,
) -> Option<Tagged<'a, Value>> {
    if DenseString::from_receiver(heap, subject).is_some() {
        return Some(subject);
    }
    if !subject.is_primitive(heap) {
        return Some(subject);
    }
    let ctor_name = if Smi::decode(subject.raw()).is_some() {
        "Number"
    } else if subject.get_as::<Float>(heap).is_some() {
        "Number"
    } else if subject == roots.true_object.get(heap) || subject == roots.false_object.get(heap) {
        "Boolean"
    } else if subject.get_as::<Symbol>(heap).is_some() {
        "Symbol"
    } else {
        return None;
    };
    let global = roots.global_object.get(heap);
    let strings = &known.strings;
    let ctor_handle = match ctor_name {
        "Number" => strings.number_ctor.get(heap),
        "Boolean" => strings.boolean_ctor.get(heap),
        _ => strings.symbol_ctor.get(heap),
    };
    let global = scope.handle(global.erase());
    let ctor_key = scope.handle(ctor_handle.erase());
    let ctor = Object::get_data_property(heap, scope, global, ctor_key);
    if !ctor.is_strong_ptr() {
        return None;
    }
    let ctor = scope.handle(ctor);
    let proto_key = scope.handle(roots.strings.prototype.get(heap).erase());
    let proto = Object::get_data_property(heap, scope, ctor, proto_key);
    if proto.is_strong_ptr() {
        Some(proto)
    } else {
        None
    }
}

/// The own string keys of a level in [[OwnPropertyKeys]] order (ES
/// 10.1.11: array indices ascending, then strings in insertion order;
/// symbols excluded). Index keys are interned to their canonical string
/// form, the value a for-in binding receives. Enumerability is NOT
/// filtered here: EnumerateObjectProperties checks it lazily per key,
/// and non-enumerable own keys must still register as visited.
fn for_in_level_keys<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    level: Handle<'_, Value>,
) -> Result<Vec<Handle<'s, Value>>, VmError> {
    // raw pass: Smi index keys (to be interned) and ready name keys
    let (mut indices, names) = 'keys: {
        let level = level.as_tagged(heap);
        let mut indices: Vec<i64> = Vec::new();
        let mut names: Vec<Handle<'s, Value>> = Vec::new();
        // string exotic: indices first; wrappers may also carry ordinary
        // descriptors, so this does not break out
        if let Some(s) = DenseString::from_receiver(heap, level) {
            indices.extend(0..s.len() as i64);
        }
        let Some(obj) = level.as_heap_object() else {
            break 'keys (indices, names);
        };
        let obj = obj;
        // elements ascending (dense hits in index order; dictionary
        // entries collected and sorted) — every receiver, not just arrays
        indices.extend(obj.element_keys(heap).iter().map(|i| *i as i64));

        for d in obj.map(heap).descriptors() {
            let name = d.name(heap);
            // exclusive homes: descriptors never hold indices; symbols
            // are never yielded, everything else keeps insertion order
            if name.erase().get_as::<Symbol>(heap).is_some() {
                continue;
            }
            names.push(scope.handle(name.erase()));
        }
        (indices, names)
    };

    indices.sort_unstable();
    indices.dedup();
    // root every key before the next allocates: interning index keys
    // promotes earlier results, and raw copies would dangle
    let mut keys: Vec<Handle<'s, Value>> = Vec::with_capacity(indices.len() + names.len());
    for i in indices {
        let s = vm.interner().intern_str(heap, scope, &i.to_string());
        keys.push(s.erase());
    }
    keys.extend(names);
    Ok(keys)
}

/// for-in iteration step (ES 14.7.5.9 EnumerateObjectProperties):
/// (enumerator) -> next key string | undefined. Per candidate key, the
/// own descriptor is checked lazily against the key's own level —
/// deleted-since-snapshot keys are skipped unvisited; keys shadowed by
/// an earlier level (yielded or non-enumerable) are skipped; enumerable
/// survivors are yielded at most once. When a level's snapshot runs
/// dry, the walk advances to the live prototype and snapshots it.
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
            // nullish subject: the head produced no enumerator
            return vm.known().undefined.get(heap).erase();
        }
        loop {
            // one candidate per turn: the cursor advances before the
            // key is examined, so skipped keys are never revisited
            let candidate = 'candidate: {
                let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                let slots = obj.slots(heap);
                let keys = rt_try!(
                    vm,
                    heap,
                    state,
                    slots
                        .at(heap, FOR_IN_KEYS)
                        .get_as::<FixedArray>(heap)
                        .ok_or(VmError::Type)
                );
                let index = rt_try!(
                    vm,
                    heap,
                    state,
                    Smi::decode(slots.at(heap, FOR_IN_INDEX).raw()).ok_or(VmError::Type)
                )
                .value() as usize;
                let Some(key) = (index < keys.len()).then(|| keys.at(heap, index)) else {
                    break 'candidate None;
                };
                slots.set(heap, FOR_IN_INDEX, Smi::new(index as i64 + 1).into_tagged());
                Some(scope.handle(key))
            };
            let Some(key) = candidate else {
                // snapshot exhausted: advance to the live prototype
                let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                let level = scope.handle(obj.slots(heap).at(heap, FOR_IN_LEVEL));
                let Some(proto) = rt_try!(
                    vm,
                    heap,
                    state,
                    for_in_next_level(heap, vm.known(), &scope, level)
                ) else {
                    return vm.known().undefined.get(heap).erase();
                };
                let keys = rt_try!(vm, heap, state, for_in_level_keys(vm, heap, &scope, proto));
                let staged = scope.stage_handles(&keys);
                let keys = heap.allocate_handle::<FixedArray>(staged, &scope);
                let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                let slots = obj.slots(heap);
                slots.set(heap, FOR_IN_LEVEL, proto.as_tagged(heap).erase());
                slots.set(heap, FOR_IN_KEYS, keys.as_tagged(heap).erase());
                slots.set(heap, FOR_IN_INDEX, Smi::new(0).into_tagged());
                continue;
            };
            // lazy [[GetOwnProperty]] on the key's own level: a key
            // deleted since the snapshot is skipped without registering
            let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let level = obj.slots(heap).at(heap, FOR_IN_LEVEL);
            let own = own_property_enumerable(heap, level, key.as_tagged(heap));
            let Some(enumerable) = own else {
                continue;
            };
            // already registered (yielded earlier, or shadowing
            // non-enumerable on a closer level): skip
            let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let visited = rt_try!(
                vm,
                heap,
                state,
                obj.slots
                    .get(heap)
                    .at(heap, FOR_IN_VISITED)
                    .get_as::<FixedArray>(heap)
                    .ok_or(VmError::Type)
            );
            let key_word = key.as_tagged(heap);
            let seen = visited
                .as_slice()
                .iter()
                .any(|s| s.get(heap).ptr_eq(key_word));
            if seen {
                continue;
            }
            // register the key — yielded or shadowing, both at most once
            {
                let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                let visited = rt_try!(
                    vm,
                    heap,
                    state,
                    obj.slots
                        .get(heap)
                        .at(heap, FOR_IN_VISITED)
                        .get_as::<FixedArray>(heap)
                        .ok_or(VmError::Type)
                );
                let mut visited: Vec<Tagged<'_, Value>> =
                    visited.as_slice().iter().map(|s| s.get(heap)).collect();
                visited.push(key.as_tagged(heap));
                let staged = scope.stage(&visited);
                let visited = heap.allocate_handle::<FixedArray>(staged, &scope);
                let Some(obj) = enumerator.as_tagged(heap).as_heap_object() else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                obj.slots(heap)
                    .set(heap, FOR_IN_VISITED, visited.as_tagged(heap).erase());
            }
            if !enumerable {
                continue;
            }
            return key.as_tagged(heap);
        }
    })
}

/// The next level of the prototype chain: an object's live [[Prototype]]
/// (read at advance time, so mutations between iterations are visible),
/// or `String.prototype` for a string primitive level. Multi-parent
/// (Self-style) and null prototypes end the walk.
fn for_in_next_level<'s>(
    heap: &Heap,
    roots: &WellKnown,
    scope: &'s HandleScope<'_>,
    level: Handle<'_, Value>,
) -> Result<Option<Handle<'s, Value>>, VmError> {
    let level = level.as_tagged(heap);
    if level.get_as::<DenseString>(heap).is_some() {
        // String.prototype via the global object (both plain data
        // lookups; no user code can run)
        let global = scope.handle(roots.global_object.get(heap).erase());
        let ctor_key = scope.handle(roots.strings.string.get(heap).erase());
        let string_ctor = Object::get_data_property(heap, scope, global, ctor_key);
        if !string_ctor.is_strong_ptr() {
            return Ok(None);
        }
        let string_ctor = scope.handle(string_ctor);
        let proto_key = scope.handle(roots.strings.prototype.get(heap).erase());
        let proto = Object::get_data_property(heap, scope, string_ctor, proto_key);
        return Ok(proto
            .is_strong_ptr()
            .then(|| scope.handle(proto))
            .filter(|p| p.as_tagged(heap).is_strong_ptr()));
    }
    let Some(obj) = level.as_heap_object() else {
        return Ok(None);
    };
    let proto = obj.prototype(heap);
    let hole = roots.the_hole.get(heap);
    if proto.ptr_eq(hole) || proto.ptr_eq(roots.null.get(heap)) {
        return Ok(None);
    }
    // a FixedArray prototype is the Self-style multi-parent form;
    // the chain walk does not model it (ends the enumeration)
    Ok(proto
        .get_as::<FixedArray>(heap)
        .map_or(Some(proto), |_| None)
        .map(|p| scope.handle(p)))
}
