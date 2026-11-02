//! ES 23.1 + 23.1.5: the Array constructor, Array.isArray,
//! Array.prototype.values/[@@iterator], and the array iterator.

use vm_core::HostCtx;
use vm_core::proxy::Flow;
use vm_core::{
    Args, Coercion, Convert, DenseString, Float, Handle, HandleSlice, Heap, LanguageMode, Object,
    Prototype, Smi, StoreSemantics, Tagged, ThreadState, VM, Value, VmError,
};
use vm_core::{raise_runtime, rt_try};

use crate::object::box_primitive;

/// `Array ( ...values )`
/// [ES 23.1.1.1](https://tc39.es/ecma262/#sec-array-constructor-array)
/// Creates an array from `values`; the prototype comes from `new.target`.
pub fn array_constructor<'a>(
    nctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let proto = match new_target {
            Some(nt) => {
                let name = scope.handle(vm.known().strings.prototype.get(heap).erase());
                match Object::get_property(vm, heap, state, nt, nt, name, false) {
                    Ok(Coercion::Value(v)) => scope.handle(v),
                    Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                    Err(err) => return raise_runtime(vm, heap, state, err),
                }
            }
            None => scope.handle(vm.known().array_prototype.get(heap).erase()),
        };
        let proto = scope.cast::<Object>(heap, proto.as_tagged(heap));

        let argv: Vec<Tagged<'_, Value>> = args.iter(heap).skip(1).collect();
        let single_len = match argv.as_slice() {
            [v] => {
                let number = if let Some(smi) = Smi::decode(v.raw()) {
                    Some(smi.value() as f64)
                } else {
                    v.get_as::<Float>(heap).map(|f| f.value.get())
                };
                match number {
                    Some(n) => {
                        let len = Convert::number_to_uint32(n);
                        if len as f64 != n {
                            return raise_runtime(vm, heap, state, VmError::OutOfBounds);
                        }
                        Some(len as usize)
                    }
                    None => None,
                }
            }
            _ => None,
        };

        let array = match single_len {
            Some(n) => {
                let array = heap.new_array(&scope, HandleSlice::EMPTY).as_handle(&scope);
                if let Err(err) = Object::set_array_length(heap, &scope, &array, n) {
                    return raise_runtime(vm, heap, state, err);
                }
                array
            }
            None => {
                let staged = scope.stage(&argv);
                heap.new_array(&scope, staged).as_handle(&scope)
            }
        };
        if let Some(proto) = proto {
            rt_try!(
                vm,
                heap,
                state,
                Object::set_prototype(heap, &scope, array, proto.erase())
            );
        }
        array.as_tagged(heap).erase()
    })
}

/// `Array.prototype.push ( ...items )`
/// [ES 23.1.3.23](https://tc39.es/ecma262/#sec-array.prototype.push)
/// Appends `items` and returns the new length.
pub fn array_push<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    if let Some(v) = array_push_fast(heap, args) {
        return v;
    }
    state.handle_scope(|scope| {
        let this = args.get_handle(heap, 0);
        let obj = if this.as_tagged(heap).is_primitive(heap) {
            match box_primitive(vm, heap, &scope, this) {
                Some(o) => o.erase(),
                None => return raise_runtime(vm, heap, state, VmError::Type),
            }
        } else {
            this
        };

        let length_key = scope.handle(vm.known().strings.length.get(heap).erase());
        let length_value = match Object::get_property(vm, heap, state, obj, obj, length_key, false)
        {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let length_number = match Object::to_numeric(vm, heap, state, length_value) {
            Ok(Some(n)) => n,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        const MAX_SAFE: f64 = 9007199254740991.0;
        let mut length = if length_number.is_nan() || length_number <= 0.0 {
            0u64
        } else {
            length_number.min(MAX_SAFE) as u64
        };
        let arg_count = args.len().saturating_sub(1) as u64;
        if length.saturating_add(arg_count) > (1u64 << 53) - 1 {
            return raise_runtime(vm, heap, state, VmError::Type);
        }

        for i in 1..args.len() {
            let item = args.get_handle(heap, i);
            let text = Convert::number_to_string(length as f64);
            let key = DenseString::from_utf8(heap, &scope, &text).erase();
            match Object::set_property(
                vm,
                heap,
                state,
                obj,
                obj,
                key,
                item,
                StoreSemantics::Shadow,
                false,
                LanguageMode::Strict,
            ) {
                Ok(Flow::Value(_)) => {}
                Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
            length += 1;
        }

        let length_value = scope.handle(heap.new_number(length as f64));
        match Object::set_property(
            vm,
            heap,
            state,
            obj,
            obj,
            length_key,
            length_value,
            StoreSemantics::Shadow,
            false,
            LanguageMode::Strict,
        ) {
            Ok(Flow::Value(_)) => {}
            Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
        length_value.as_tagged(heap)
    })
}

/// One argument, receiver already an array with spare capacity: append with
/// no allocation and no handle scope. Returns `None` for anything else.
#[inline(always)]
fn array_push_fast<'a>(heap: &Heap, args: Args) -> Option<Tagged<'a, Value>> {
    if args.len() != 2 {
        return None;
    }
    let receiver = args.get_handle(heap, 0);
    let arg = args.get_handle(heap, 1);
    let r = receiver.as_tagged(heap);
    let obj = r.as_heap_object()?;
    if !obj.as_ref().is_array(heap) {
        return None;
    }
    let len = obj.as_ref().length();
    let elements = obj.as_ref().elements_array(heap)?;
    if len >= elements.len() || !fast_path_exact(heap, obj, len) {
        return None;
    }
    elements.as_ref().set(heap, len, arg.as_tagged(heap));
    obj.as_ref()
        .length
        .set(heap, r.erase(), Smi::new((len + 1) as i64));
    Prototype::element_mutated(heap, obj);
    Some(Smi::new((len + 1) as i64).into_tagged())
}

/// Whether a direct element store matches `Set(obj, index, item, true)`.
#[inline]
fn fast_path_exact(heap: &Heap, obj: Tagged<'_, Object>, index: usize) -> bool {
    let known = heap.known();
    let kind = obj.map(heap).kind();
    if !kind.is_extendable() || kind.is_prototype() || !obj.map(heap).length_writable(heap) {
        return false;
    }
    let array_proto = known.array_prototype.get(heap);
    let object_proto = known.object_prototype.get(heap);
    obj.prototype(heap) == array_proto.erase()
        && array_proto.prototype(heap) == object_proto.erase()
        && object_proto.prototype(heap) == known.null.get(heap)
        && !has_own_element(heap, array_proto, index)
        && !has_own_element(heap, object_proto, index)
}

#[inline]
fn has_own_element(heap: &Heap, obj: Tagged<'_, Object>, index: usize) -> bool {
    if let Some(dict) = obj.element_dictionary(heap) {
        dict.find(heap, index).is_some()
    } else if let Some(elements) = obj.elements_array(heap) {
        index < elements.len() && elements.at(heap, index) != heap.known().the_hole.get(heap)
    } else {
        false
    }
}

/// `Array.prototype.pop` (ES 23.1.3.22): remove the last element, shorten
/// `length`, and punch a hole so the store releases the value.
pub fn array_pop<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let Some(obj) = scope.cast::<Object>(heap, receiver.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let is_array = obj.as_tagged(heap).is_array(heap);
        if !is_array {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let this = obj.as_tagged(heap);
        let len = this.as_ref().length();
        if len == 0 {
            return vm.known().undefined.get(heap).erase();
        }
        let last = len - 1;
        let value = this
            .as_ref()
            .element_value(heap, last)
            .unwrap_or_else(|| vm.known().undefined.get(heap).erase());
        if let Some(dict) = this.as_ref().element_dictionary(heap) {
            dict.as_ref().delete(heap, last);
        } else {
            let elements = this.as_ref().elements(heap);
            if elements.is_strong_ptr() && last < elements.as_ref().len() {
                elements
                    .as_ref()
                    .set(heap, last, vm.known().the_hole.get(heap).erase());
            }
        }
        this.as_ref()
            .length
            .set(heap, this.erase(), Smi::new(last as i64));
        Prototype::element_mutated(heap, this);
        value
    })
}

/// `Array.prototype.values` / `Array.prototype[@@iterator]`
/// [ES 23.1.3.38](https://tc39.es/ecma262/#sec-array.prototype.values)
/// [ES 23.1.3.40](https://tc39.es/ecma262/#sec-array.prototype-@@iterator)
/// Creates an array iterator over the receiver.
pub fn array_values<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let is_array = receiver
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.as_ref().is_array(heap));
        if !is_array {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let map = scope.handle(vm.known().array_iterator_map.get(heap));
        heap.new_object(
            &scope,
            map,
            scope.stage(&[receiver.as_tagged(heap), Smi::new(0).into_tagged()]),
        )
        .erase()
    })
}

/// `%ArrayIteratorPrototype%.next` (ES 23.1.5.2.1): one step over the
/// iterated array, producing `{ value, done }`.
pub fn array_iterator_next<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let receiver = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let (array, index) = {
            let Some(obj) = receiver.as_tagged(heap).as_heap_object() else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let slots = obj.as_ref().slots(heap);
            if slots.len() < 2 {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
            (scope.handle(slots.at(heap, 0)), slots.at(heap, 1).raw())
        };
        let Some(index) = Smi::decode(index) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let len = array
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| o.as_ref().length())
            .unwrap_or(0);
        let done = index.value() as usize >= len;
        let (value, done_value) = if done {
            (
                scope.handle(vm.known().undefined.get(heap).erase()),
                scope.handle(vm.known().true_object.get(heap).erase()),
            )
        } else {
            // element reads see holes as undefined
            let v = scope.handle(
                array
                    .as_tagged(heap)
                    .as_heap_object()
                    .and_then(|o| o.as_ref().element_value(heap, index.value() as usize))
                    .unwrap_or_else(|| vm.known().undefined.get(heap).erase()),
            );
            (v, scope.handle(vm.known().false_object.get(heap).erase()))
        };
        // advance the index slot
        {
            let Some(obj) = receiver.as_tagged(heap).as_heap_object() else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let slots = obj.as_ref().slots(heap);
            slots.set(heap, 1, Smi::new(index.value() + 1).into_tagged());
        };
        let map = scope.handle(vm.known().iterator_result_map.get(heap));
        heap.new_object(
            &scope,
            map,
            scope.stage(&[
                value.as_tagged(heap).erase(),
                done_value.as_tagged(heap).erase(),
            ]),
        )
        .erase()
    })
}

/// `%ArrayIteratorPrototype%[@@iterator]`: returns the receiver.
pub fn array_iterator_symbol_iterator<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm: _,
        heap,
        state: _,
        ..
    } = nctx;
    let arg = args.get(heap, 0);
    arg
}

/// `Array.prototype.join(separator)` (ES 23.1.3.18): ToString each element
/// in index order, separated by `separator` (default `","`). Holes,
/// `undefined`, and `null` render as the empty string.
pub fn array_join<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let receiver = args.get_handle(heap, 0);
    let separator = (args.len() > 1).then(|| args.get_handle(heap, 1));
    join_impl(vm, heap, state, receiver, separator)
}

/// `Array.prototype.toString` (ES 23.1.3.36): `join` with the default
/// separator; any arguments are ignored.
pub fn array_to_string<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    let receiver = args.get_handle(heap, 0);
    join_impl(vm, heap, state, receiver, None)
}

fn join_impl<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    receiver: Handle<'_, Value>,
    separator: Option<Handle<'_, Value>>,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let is_array = receiver
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.as_ref().is_array(heap));
        if !is_array {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // separator = ToString(separator); absent/undefined defaults to ","
        let sep = match separator {
            Some(h) if h.as_tagged(heap) != vm.known().undefined.get(heap) => {
                match Object::to_string(vm, heap, state, h) {
                    Ok(Some(s)) => {
                        let s = scope.handle(s);
                        s.as_tagged(heap)
                            .get_as::<DenseString>(heap)
                            .map(|d| d.to_rust_string(heap))
                            .unwrap_or_default()
                    }
                    Ok(None) => return vm.known().exception.get(heap).erase(),
                    Err(err) => return raise_runtime(vm, heap, state, err),
                }
            }
            _ => ",".to_string(),
        };
        // length is read once, then elements are Get in index order
        let len = receiver
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| o.as_ref().length())
            .unwrap_or(0);
        let mut out = String::new();
        for i in 0..len {
            if i > 0 {
                out.push_str(&sep);
            }
            let Some(v) = receiver
                .as_tagged(heap)
                .as_heap_object()
                .and_then(|o| o.as_ref().element_value(heap, i))
            else {
                continue;
            };
            if v == vm.known().undefined.get(heap) || v == vm.known().null.get(heap) {
                continue;
            }
            let h = scope.handle(v);
            match Object::to_string(vm, heap, state, h) {
                Ok(Some(s)) => {
                    let s = scope.handle(s);
                    if let Some(d) = s.as_tagged(heap).get_as::<DenseString>(heap) {
                        out.push_str(&d.to_rust_string(heap));
                    }
                }
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        }
        DenseString::from_utf8(heap, &scope, &out)
            .as_tagged(heap)
            .erase()
    })
}

pub fn array_is_array<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm: _,
        heap,
        state: _,
        ..
    } = nctx;
    let arg = args.get(heap, 1);
    let is_array = arg
        .as_heap_object()
        .is_some_and(|o| o.as_ref().is_array(heap));
    Convert::boolean(heap, is_array)
}

/// A `slice` boundary: absent/undefined → `default`, Smi passes straight
/// through, otherwise ToNumber and truncate (NaN → 0). `Ok(None)` is a
/// pending exception.
fn slice_bound(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    arg: Option<Handle<'_, Value>>,
    default: f64,
) -> Result<Option<f64>, VmError> {
    let Some(h) = arg else {
        return Ok(Some(default));
    };
    let v = h.as_tagged(heap);
    if v == vm.known().undefined.get(heap) {
        return Ok(Some(default));
    }
    if let Some(smi) = Smi::decode(v.raw()) {
        return Ok(Some(smi.value() as f64));
    }
    match Object::to_numeric(vm, heap, state, h)? {
        Some(n) => Ok(Some(if n.is_nan() { 0.0 } else { n.trunc() })),
        None => Ok(None),
    }
}

/// `Array.prototype.slice(start, end)` (ES 23.1.3.28): a new array with
/// the `[start, end)` elements, holes preserved. An absent/undefined bound
/// is the fast path (no coercion; `slice()` clones the whole backing
/// store).
pub fn array_slice<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let Some(obj) = receiver.as_tagged(heap).as_heap_object() else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        if !obj.as_ref().is_array(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let obj = scope.handle(obj);
        let len = obj.as_tagged(heap).length() as f64;

        let start = match slice_bound(
            vm,
            heap,
            state,
            (args.len() > 1).then(|| args.get_handle(heap, 1)),
            0.0,
        ) {
            Ok(Some(v)) => v,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let end = match slice_bound(
            vm,
            heap,
            state,
            (args.len() > 2).then(|| args.get_handle(heap, 2)),
            len,
        ) {
            Ok(Some(v)) => v,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let start = if start < 0.0 {
            (len + start).max(0.0)
        } else {
            start.min(len)
        };
        let end = if end < 0.0 {
            (len + end).max(0.0)
        } else {
            end.min(len)
        };
        let count = (end - start).max(0.0) as usize;
        let start = start as usize;

        // one gather pass, then a single bulk FixedArray allocation
        let hole = vm.known().the_hole.get(heap).erase().raw();
        let mut values: Vec<Tagged<'_, Value>> = Vec::with_capacity(count);
        let mut has_hole = false;
        for i in start..start + count {
            match obj.as_tagged(heap).element_value(heap, i) {
                Some(v) => values.push(v),
                None => {
                    // Safety: old-gen singleton word.
                    values.push(unsafe { Tagged::<Value>::from_value_unchecked(hole) });
                    has_hole = true;
                }
            }
        }
        let staged = scope.stage(&values);
        let arr = heap.new_array(&scope, staged).as_handle(&scope);
        if has_hole {
            Object::promote_holey(heap, &scope, &arr);
        }
        arr.as_tagged(heap).erase()
    })
}

/// `Array.prototype.sort(comparefn)` (ES 23.1.3.30): in-place, stable.
/// Holes move last, `undefined` before them without invoking `comparefn`;
/// the default comparator is the elements' `ToString` order. Returns the
/// receiver.
pub fn array_sort<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let Some(obj) = receiver.as_tagged(heap).as_heap_object() else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        if !obj.as_ref().is_array(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let obj = scope.handle(obj);
        // an explicit `undefined` comparator means the default one
        let cmpfn = if args.len() > 1 {
            let c = args.get(heap, 1);
            if c != vm.known().undefined.get(heap) {
                if !c.is_callable(heap) {
                    return raise_runtime(vm, heap, state, VmError::Type);
                }
                Some(scope.handle(c))
            } else {
                None
            }
        } else {
            None
        };
        let undefined = vm.known().undefined.get(heap).erase().raw();
        let exception = vm.known().exception.get(heap).erase().raw();
        let len = obj.as_tagged(heap).length();

        // Root every element up front. The sort can allocate (`ToString`
        // for the default keys, user code for a comparator); a collection
        // would invalidate raw words, but handles are updated in place by
        // the GC, so they stay valid throughout.
        let mut elems: Vec<Handle<'_, Value>> = Vec::new();
        let mut undefined_count = 0usize;
        for i in 0..len {
            match obj.as_tagged(heap).element_value(heap, i) {
                None => {}
                Some(v) if v.raw() == undefined => undefined_count += 1,
                Some(v) => elems.push(scope.handle(v)),
            }
        }
        let count = elems.len();

        match &cmpfn {
            None => {
                // default comparator. When every element is already a
                // string, compare the rooted DenseStrings in place — no key
                // materialization at all.
                let all_strings = elems
                    .iter()
                    .all(|h| h.as_tagged(heap).get_as::<DenseString>(heap).is_some());
                if all_strings {
                    elems.sort_by(|a, b| {
                        cmp_dense_strings(heap, a.as_tagged(heap), b.as_tagged(heap))
                    });
                } else {
                    // materialize each element's ToString once (rooted),
                    // then sort the (value, key) pairs directly by key
                    let mut pairs: Vec<(Handle<'_, Value>, Handle<'_, Value>)> =
                        Vec::with_capacity(count);
                    for h in elems.iter() {
                        let key = match Object::to_string(vm, heap, state, *h) {
                            Ok(Some(s)) => s,
                            Ok(None) => {
                                // Safety: old-gen singleton word.
                                return unsafe { Tagged::<Value>::from_value_unchecked(exception) };
                            }
                            Err(err) => return raise_runtime(vm, heap, state, err),
                        };
                        let kh = scope
                            .handle(unsafe { Tagged::<Value>::from_value_unchecked(key.raw()) });
                        pairs.push((*h, kh));
                    }
                    pairs.sort_by(|a, b| {
                        cmp_dense_strings(heap, a.1.as_tagged(heap), b.1.as_tagged(heap))
                    });
                    elems = pairs.into_iter().map(|p| p.0).collect();
                }
            }
            Some(cmp) => {
                let threw = core::cell::Cell::new(false);
                elems.sort_by(|a, b| {
                    if threw.get() {
                        return core::cmp::Ordering::Equal;
                    }
                    // a fresh scope per comparison reclaims its slots
                    state.handle_scope(|cscope| {
                        let x = a.as_tagged(heap);
                        let y = b.as_tagged(heap);
                        let call_args = cscope.stage(&[
                            // Safety: old-gen singleton word.
                            unsafe { Tagged::<Value>::from_value_unchecked(undefined) },
                            x,
                            y,
                        ]);
                        let v = match HostCtx::enter(vm, heap, state, *cmp, call_args, None) {
                            Ok(v) => v,
                            Err(_) => {
                                threw.set(true);
                                return core::cmp::Ordering::Equal;
                            }
                        };
                        if v.raw() == exception {
                            threw.set(true);
                            return core::cmp::Ordering::Equal;
                        }
                        // Safety: fresh word, no allocation since the read.
                        let arg = unsafe { Tagged::<Value>::from_value_unchecked(v.raw()) };
                        match Object::to_numeric(vm, heap, state, cscope.handle(arg)) {
                            Ok(Some(n)) => {
                                if n < 0.0 {
                                    core::cmp::Ordering::Less
                                } else if n > 0.0 {
                                    core::cmp::Ordering::Greater
                                } else {
                                    core::cmp::Ordering::Equal
                                }
                            }
                            _ => {
                                threw.set(true);
                                core::cmp::Ordering::Equal
                            }
                        }
                    })
                });
                if threw.get() {
                    // Safety: old-gen singleton word.
                    return unsafe { Tagged::<Value>::from_value_unchecked(exception) };
                }
            }
        }

        // write back: a tight direct loop into the elements backing store
        // when it has room; per-element stores only when it must grow
        let total = count + undefined_count;
        let capacity = obj
            .as_tagged(heap)
            .as_ref()
            .elements_array(heap)
            .map(|e| e.len())
            .unwrap_or(0);
        if total <= capacity {
            let elements = obj
                .as_tagged(heap)
                .as_ref()
                .elements_array(heap)
                .expect("capacity > 0");
            for (j, h) in elems.iter().enumerate() {
                // Safety: rooted handle re-read under the shared borrow.
                let v = h.as_tagged(heap);
                elements.as_ref().set(heap, j, v);
            }
            for j in count..total {
                // Safety: old-gen singleton word.
                elements.as_ref().set(heap, j, unsafe {
                    Tagged::<Value>::from_value_unchecked(undefined)
                });
            }
        } else {
            for (j, h) in elems.iter().enumerate() {
                if let Err(e) = Object::store_array_element(heap, &scope, &obj, j, h) {
                    return raise_runtime(vm, heap, state, e);
                }
            }
            for j in count..total {
                let h = scope.handle(unsafe { Tagged::<Value>::from_value_unchecked(undefined) });
                if let Err(e) = Object::store_array_element(heap, &scope, &obj, j, &h) {
                    return raise_runtime(vm, heap, state, e);
                }
            }
        }
        // the remainder become holes (or removed dictionary entries)
        if total < len {
            if let Some(dict) = obj.as_tagged(heap).element_dictionary(heap) {
                let mut doomed: Vec<usize> = Vec::new();
                dict.as_ref().for_each_entry(heap, |k, _, _| {
                    if k >= total {
                        doomed.push(k);
                    }
                });
                for k in doomed {
                    dict.as_ref().delete(heap, k);
                }
            } else {
                let hole = vm.known().the_hole.get(heap).erase().raw();
                if let Some(elements) = obj.as_tagged(heap).elements_array(heap) {
                    for j in total..len.min(elements.len()) {
                        // Safety: old-gen singleton word.
                        elements.as_ref().set(heap, j, unsafe {
                            Tagged::<Value>::from_value_unchecked(hole)
                        });
                    }
                }
                Object::promote_holey(heap, &scope, &obj);
            }
        }
        Prototype::element_mutated(heap, obj.as_tagged(heap));
        obj.as_tagged(heap).erase()
    })
}

/// Lexicographic `DenseString` comparison over UTF-16 code units, in
/// place (no allocation). Non-strings compare equal (the caller only uses
/// this where both sides are known strings).
#[inline]
fn cmp_dense_strings(
    heap: &Heap,
    a: Tagged<'_, Value>,
    b: Tagged<'_, Value>,
) -> core::cmp::Ordering {
    match (a.get_as::<DenseString>(heap), b.get_as::<DenseString>(heap)) {
        (Some(x), Some(y)) => x.as_ref().data(heap).cmp(&y.as_ref().data(heap)),
        _ => core::cmp::Ordering::Equal,
    }
}
