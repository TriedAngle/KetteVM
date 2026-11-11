use crate::JSRuntime;
use vm_core::{
    Args, Compare, Convert, DenseString, Float, Handle, HandleScope, HandleSlice, Heap, HostCtx,
    LanguageMode, Object, Prototype, Result, SlotName, Smi, StoreSemantics, Tagged, ThreadState,
    VM, Value, VmError, extend_lifetime, raise, raise_runtime, rt_try,
};

crate::register_natives! {
    array -> array_constructor,
    array_values,
    array_push,
    array_pop,
    array_map,
    array_reduce,
    array_iterator_next,
    array_iterator_symbol_iterator,
    array_is_array,
    array_join,
    array_to_string,
    array_slice,
    array_sort,
    array_at,
    array_includes,
    array_index_of,
    array_last_index_of,
    array_fill,
    array_reverse,
    array_shift,
    array_unshift,
    array_of,
}

const ARRAY_ITERATOR_KIND_KEY: i64 = 0;
const ARRAY_ITERATOR_KIND_VALUE: i64 = 1;
const ARRAY_ITERATOR_KIND_KEY_VALUE: i64 = 2;

enum IndexedValue<'s> {
    Absent,
    Present(Handle<'s, Value>),
}

/// Get `obj[index]`, using a direct own-element read when possible.
fn get_indexed_value<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: Handle<'_, Value>,
    index: u64,
) -> Result<IndexedValue<'s>> {
    if let Some(o) = obj.as_tagged(heap).as_heap_object()
        && let Some(value) = o.element_value(heap, index as usize)
    {
        return Ok(IndexedValue::Present(scope.handle(value)));
    }
    // TODO: handle the 2^53 max length/key.
    let key = scope.handle(Object::to_property_key(
        vm,
        heap,
        state,
        scope.handle(Smi::new(index as i64).into_tagged()),
    )?);
    let present = Object::has_property(vm, heap, state, obj, key.erase())?;
    if !present {
        return Ok(IndexedValue::Absent);
    }
    let value = Object::get_property(vm, heap, state, obj, obj, key.erase(), false)?;
    Ok(IndexedValue::Present(scope.handle(value)))
}

/// `Array ( ...values )`
/// [ES 23.1.1.1](https://tc39.es/ecma262/#sec-array-constructor-array)
/// Creates an array from `values`; the prototype comes from `new.target`.
fn array_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let proto = match new_target {
            Some(nt) => {
                let name = vm
                    .known()
                    .strings
                    .prototype
                    .get_handle(heap, &scope)
                    .erase();
                match Object::get_property(vm, heap, state, nt, nt, name, false) {
                    Ok(v) => scope.handle(v),
                    Err(err) => return raise_runtime(heap, err),
                }
            }
            None => heap
                .known()
                .array_prototype
                .get_handle(heap, &scope)
                .erase(),
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
                            raise!(vm, heap, state, VmError::IndexOutOfBounds);
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
                Object::set_array_length(heap, &scope, &array, n);
                array
            }
            None => {
                let staged = scope.stage(&argv);
                heap.new_array(&scope, staged).as_handle(&scope)
            }
        };
        if let Some(proto) = proto {
            rt_try!(
                heap,
                Object::set_prototype(vm, heap, state, &scope, array, proto.erase())
            );
        }
        array.as_tagged(heap).erase()
    })
}

/// `Array.prototype.push ( ...items )`
/// [ES 23.1.3.23](https://tc39.es/ecma262/#sec-array.prototype.push)
/// Appends `items` and returns the new length.
fn array_push<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if let Some(len) = array_push_fast(heap, state, args) {
        return Smi::new(len as i64).into_tagged();
    }
    state.handle_scope(|scope| {
        let this = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, this) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };

        let length_key = vm.known().strings.length.get_handle(heap, &scope).erase();
        let mut length = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let arg_count = args.len().saturating_sub(1) as u64;
        if length.saturating_add(arg_count) > (1u64 << 53) - 1 {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }

        for i in 1..args.len() {
            let item = args.get_handle(heap, i);
            let key = rt_try!(heap, index_key(vm, heap, state, &scope, length)).erase();
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
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
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
            Ok(_) => {}
            Err(err) => return raise_runtime(heap, err),
        }
        length_value.as_tagged(heap)
    })
}

/// Allocation Free Array push fast path if space is remaining.
/// Returns `None` when the generic `[[Set]]` algorithm is required.
#[inline]
fn array_push_fast(heap: &mut Heap, state: &ThreadState, args: Args) -> Option<usize> {
    if args.len() != 2 {
        return None;
    }
    let len = {
        let receiver = args.get_handle(heap, 0);
        let r = receiver.as_tagged(heap);
        let obj = r.as_heap_object()?;
        if !obj.as_ref().is_array_exotic(heap) {
            return None;
        }
        let len = obj.as_ref().length();
        // storing the next element would set `length` past 2^32 - 1, which
        // the slow path reports (after the element store) as a RangeError
        if len >= u32::MAX as usize {
            return None;
        }
        if !fast_path_exact(heap, obj, len) {
            return None;
        }
        if let Some(elements) = obj.as_ref().elements_array(heap)
            && len < elements.len()
        {
            let arg = args.get_handle(heap, 1);
            elements.as_ref().set(heap, len, arg.as_tagged(heap));
            obj.as_ref()
                .length
                .set(heap, r.erase(), Smi::new((len + 1) as i64));
            Prototype::element_mutated(heap, obj);
            return Some(len + 1);
        }
        len
    };
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = receiver.as_tagged(heap).as_heap_object()?;
        let receiver_obj = scope.handle(obj);
        let arg = args.get_handle(heap, 1);
        Object::store_array_element(heap, &scope, &receiver_obj, len, &arg);
        Some(len + 1)
    })
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

/// `Array.prototype.pop ( )`
/// [ES 23.1.3.22](https://tc39.es/ecma262/#sec-array.prototype.pop)
/// Removes and returns the last element of an array-like receiver.
fn array_pop<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        if let Some(v) = array_pop_fast(heap, &scope, args) {
            return v.as_tagged(heap);
        }

        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };

        // LengthOfArrayLike (ES 7.3.18): Get(obj, "length") then ToLength.
        let length_key = vm.known().strings.length.get_handle(heap, &scope).erase();
        let length = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );

        if length == 0 {
            let zero = scope.handle(Smi::new(0).into_tagged());
            match Object::set_property(
                vm,
                heap,
                state,
                obj,
                obj,
                length_key,
                zero,
                StoreSemantics::Shadow,
                false,
                LanguageMode::Strict,
            ) {
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
            }
            return vm.known().undefined.get(heap).erase();
        }

        let new_length = length - 1;
        let new_length_value = scope.handle(Smi::new(new_length as i64).into_tagged());
        // ToString(newLen), interned: named keys compare by pointer.
        let index = scope.handle(
            rt_try!(
                heap,
                Object::to_property_key(vm, heap, state, new_length_value)
            )
            .erase(),
        );
        let element = match Object::get_property(vm, heap, state, obj, obj, index, false) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        };
        match Object::delete_property(vm, heap, state, obj, index) {
            Ok(true) => {}
            Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
            Err(err) => return raise_runtime(heap, err),
        }
        match Object::set_property(
            vm,
            heap,
            state,
            obj,
            obj,
            length_key,
            new_length_value,
            StoreSemantics::Shadow,
            false,
            LanguageMode::Strict,
        ) {
            Ok(_) => {}
            Err(err) => return raise_runtime(heap, err),
        }
        element.as_tagged(heap)
    })
}

/// Direct `pop` for a dense array with a writable length: the last element
/// is a configurable data property, so punching a hole and shrinking the
/// length cannot run user code or allocate. Returns `None` for anything that
/// needs the generic algorithm.
#[inline]
fn array_pop_fast<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    args: Args,
) -> Option<Handle<'s, Value>> {
    let receiver = args.get_handle(heap, 0);
    let obj = receiver.as_tagged(heap).as_heap_object()?;
    if !obj.as_ref().is_array_exotic(heap) || !obj.as_ref().map(heap).length_writable(heap) {
        return None;
    }
    let len = obj.as_ref().length();
    if len == 0 {
        return Some(heap.known().undefined.root_handle().erase());
    }
    let last = len - 1;
    let elements = obj.as_ref().elements_array(heap)?;
    if last >= elements.len() || elements.at(heap, last) == heap.known().the_hole.get(heap).erase()
    {
        return None;
    }
    let value = scope.handle(elements.at(heap, last));
    elements
        .as_ref()
        .set(heap, last, heap.known().the_hole.get(heap).erase());
    obj.as_ref()
        .length
        .set(heap, obj.erase(), Smi::new(last as i64));
    Prototype::element_mutated(heap, obj);
    Some(value)
}

/// `Array.prototype.values` / `Array.prototype[@@iterator]`
/// [ES 23.1.3.38](https://tc39.es/ecma262/#sec-array.prototype.values)
/// [ES 23.1.3.40](https://tc39.es/ecma262/#sec-array.prototype-@@iterator)
/// Creates an array iterator over the receiver.
fn array_values<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let map = known.array_iterator_map.get_handle(heap, &scope);
        heap.new_object(
            &scope,
            map,
            scope.stage(&[
                obj.as_tagged(heap),
                Smi::new(0).into_tagged(),
                Smi::new(ARRAY_ITERATOR_KIND_VALUE).into_tagged(),
            ]),
        )
        .erase()
    })
}

/// `%ArrayIteratorPrototype%.next ( )`
/// [ES 23.1.5.2.1](https://tc39.es/ecma262/#sec-%arrayiteratorprototype%.next)
/// Advances an array iterator and returns `{ value, done }`.
fn array_iterator_next<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let Some(iterator) = receiver.as_tagged(heap).as_heap_object() else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let iterator = scope.handle(iterator);
        if !iterator
            .as_tagged(heap)
            .map(heap)
            .ptr_eq(known.array_iterator_map.get(heap))
        {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let (array, index, kind) = {
            let slots = iterator.as_tagged(heap).slots(heap);
            if slots.len() < 3 {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
            (
                scope.handle(slots.at(heap, 0)),
                slots.at(heap, 1).raw(),
                slots.at(heap, 2).raw(),
            )
        };
        let undefined = vm.known().undefined.get_handle(heap, &scope).erase();

        let (result, done) = if array.as_tagged(heap).ptr_eq(undefined.as_tagged(heap)) {
            (undefined, true)
        } else {
            let Some(index) = Smi::decode(index) else {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            };
            let Some(kind) = Smi::decode(kind).map(|s| s.value()) else {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            };
            let index = index.value() as u64;
            let length = rt_try!(
                heap,
                Object::length_of_array_like(vm, heap, state, &scope, array)
            );
            if index >= length {
                iterator
                    .as_tagged(heap)
                    .slots(heap)
                    .set(heap, 0, undefined.as_tagged(heap));
                (undefined, true)
            } else {
                iterator.as_tagged(heap).slots(heap).set(
                    heap,
                    1,
                    Smi::new((index + 1) as i64).into_tagged(),
                );
                let index_number = Smi::new(index as i64).into_tagged();
                let result = match kind {
                    ARRAY_ITERATOR_KIND_KEY => scope.handle(index_number),
                    ARRAY_ITERATOR_KIND_VALUE | ARRAY_ITERATOR_KIND_KEY_VALUE => {
                        let index_key = scope.handle(
                            rt_try!(
                                heap,
                                Object::to_property_key(
                                    vm,
                                    heap,
                                    state,
                                    scope.handle(index_number),
                                )
                            )
                            .erase(),
                        );
                        let value = match Object::get_property(
                            vm, heap, state, array, array, index_key, false,
                        ) {
                            Ok(v) => scope.handle(v),
                            Err(err) => return raise_runtime(heap, err),
                        };
                        if kind == ARRAY_ITERATOR_KIND_VALUE {
                            value
                        } else {
                            let pair = heap
                                .new_array(
                                    &scope,
                                    scope.stage(&[index_number, value.as_tagged(heap)]),
                                )
                                .as_handle(&scope);
                            scope.handle(pair.as_tagged(heap).erase())
                        }
                    }
                    _ => raise!(vm, heap, state, VmError::InvalidOperandType),
                };
                (result, false)
            }
        };
        let map = known.iterator_result_map.get_handle(heap, &scope);
        let done_value = scope.handle(if done {
            vm.known().true_object.get(heap).erase()
        } else {
            vm.known().false_object.get(heap).erase()
        });
        heap.new_object(
            &scope,
            map,
            scope.stage(&[
                result.as_tagged(heap).erase(),
                done_value.as_tagged(heap).erase(),
            ]),
        )
        .erase()
    })
}

/// `Iterator.prototype [ %Symbol.iterator% ] ( )`
/// [ES 27.1.3.3.18](https://tc39.es/ecma262/#sec-iterator.prototype-%symbol.iterator%)
/// Returns the receiver.
fn array_iterator_symbol_iterator<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    args.get(heap, 0)
}

/// `Array.prototype.join ( separator )`
/// [ES 23.1.3.18](https://tc39.es/ecma262/#sec-array.prototype.join)
/// Joins array-like elements with `separator`, using `","` by default.
fn array_join<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = args.get_handle(heap, 0);
    let separator = (args.len() > 1).then(|| args.get_handle(heap, 1));
    join_impl(vm, heap, state, receiver, separator)
}

/// `Array.prototype.map ( callback [ , thisArg ] )`
/// [ES 23.1.3.21](https://tc39.es/ecma262/#sec-array.prototype.map)
/// Maps each present array-like element into a new array.
fn array_map<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let callback = args.get_handle(heap, 1);
        if !callback.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        // ArrayCreate(ES 23.1.3.33) rejects lengths above 2^32 - 1.
        if len > u32::MAX as u64 {
            raise!(vm, heap, state, VmError::IndexOutOfBounds);
        }
        let this_arg = if args.len() > 2 {
            args.get_handle(heap, 2)
        } else {
            vm.known().undefined.get_handle(heap, &scope).erase()
        };
        let exception = vm.known().exception.get_handle(heap, &scope).erase();

        // TODO: ArraySpeciesCreate (ES 23.1.3.34) once species is supported.
        let result = heap.new_array(&scope, HandleSlice::EMPTY).as_handle(&scope);
        for k in 0..len {
            let value = match get_indexed_value(vm, heap, state, &scope, obj, k) {
                Ok(IndexedValue::Absent) => continue,
                Ok(IndexedValue::Present(value)) => value,
                Err(err) => return raise_runtime(heap, err),
            };
            let call_args = scope.stage(&[
                this_arg.as_tagged(heap),
                value.as_tagged(heap),
                Smi::new(k as i64).into_tagged(),
                obj.as_tagged(heap),
            ]);
            let mapped = match HostCtx::enter(
                vm,
                state.runtime(vm, heap),
                heap,
                state,
                callback,
                call_args,
                None,
            ) {
                Ok(value) => scope.handle(value),
                Err(err) => return raise_runtime(heap, err),
            };
            if mapped.as_tagged(heap) == exception.as_tagged(heap) {
                return exception.as_tagged(heap);
            }
            Object::store_array_element(heap, &scope, &result, k as usize, &mapped);
        }
        Object::set_array_length(heap, &scope, &result, len as usize);
        result.as_tagged(heap).erase()
    })
}

/// `Array.prototype.reduce ( callback [ , initialValue ] )`
/// [ES 23.1.3.24](https://tc39.es/ecma262/#sec-array.prototype.reduce)
/// Reduces present array-like elements from left to right.
fn array_reduce<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let callback = args.get_handle(heap, 1);
        if !callback.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let undefined = vm.known().undefined.get_handle(heap, &scope).erase();
        let exception = vm.known().exception.get_handle(heap, &scope).erase();
        let mut k = 0u64;

        let mut accumulator = if args.len() > 2 {
            scope.handle(args.get_handle(heap, 2).as_tagged(heap))
        } else {
            let mut first: Option<Handle<'_, Value>> = None;
            while k < len && first.is_none() {
                match get_indexed_value(vm, heap, state, &scope, obj, k) {
                    Ok(IndexedValue::Absent) => {}
                    Ok(IndexedValue::Present(value)) => first = Some(value),
                    Err(err) => return raise_runtime(heap, err),
                }
                k += 1;
            }
            let Some(value) = first else {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            };
            value
        };

        while k < len {
            let value = match get_indexed_value(vm, heap, state, &scope, obj, k) {
                Ok(IndexedValue::Absent) => {
                    k += 1;
                    continue;
                }
                Ok(IndexedValue::Present(value)) => value,
                Err(err) => return raise_runtime(heap, err),
            };
            let call_args = scope.stage(&[
                undefined.as_tagged(heap),
                accumulator.as_tagged(heap),
                value.as_tagged(heap),
                Smi::new(k as i64).into_tagged(),
                obj.as_tagged(heap),
            ]);
            let result = match HostCtx::enter(
                vm,
                state.runtime(vm, heap),
                heap,
                state,
                callback,
                call_args,
                None,
            ) {
                Ok(value) => scope.handle(value),
                Err(err) => return raise_runtime(heap, err),
            };
            if result.as_tagged(heap) == exception.as_tagged(heap) {
                return exception.as_tagged(heap);
            }
            accumulator = result;
            k += 1;
        }
        accumulator.as_tagged(heap).erase()
    })
}

/// `Array.prototype.toString ( )`
/// [ES 23.1.3.36](https://tc39.es/ecma262/#sec-array.prototype.tostring)
/// Calls the receiver's `join`, falling back to `%Object.prototype.toString%`.
fn array_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let join_key = known.strings.join.get_handle(heap, &scope).erase();
        let join = match Object::get_property(vm, heap, state, obj, obj, join_key, false) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        };
        let func = if join.as_tagged(heap).is_callable(heap) {
            join
        } else {
            known.object_to_string.get_handle(heap, &scope).erase()
        };
        let call_args = scope.stage(&[obj.as_tagged(heap).erase()]);
        match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            func,
            call_args,
            None,
        ) {
            Ok(value) => extend_lifetime!(heap, value),
            Err(err) => raise_runtime(heap, err),
        }
    })
}

fn join_impl<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    receiver: Handle<'_, Value>,
    separator: Option<Handle<'_, Value>>,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };

        let length = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );

        let sep = match separator {
            Some(h) if h.as_tagged(heap) != vm.known().undefined.get(heap) => {
                let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, h)));
                s.as_tagged(heap)
                    .get_as::<DenseString>(heap)
                    .map(|d| d.to_rust_string(heap))
                    .unwrap_or_default()
            }
            _ => ",".to_string(),
        };

        let mut out = String::new();
        for i in 0..length {
            if i > 0 {
                out.push_str(&sep);
            }
            let index = scope.handle(
                rt_try!(
                    heap,
                    Object::to_property_key(
                        vm,
                        heap,
                        state,
                        scope.handle(Smi::new(i as i64).into_tagged()),
                    )
                )
                .erase(),
            );
            let element = match Object::get_property(vm, heap, state, obj, obj, index, false) {
                Ok(v) => scope.handle(v),
                Err(err) => return raise_runtime(heap, err),
            };
            let element_value = element.as_tagged(heap);
            if element_value == vm.known().undefined.get(heap)
                || element_value == vm.known().null.get(heap)
            {
                continue;
            }
            let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, element)));
            if let Some(d) = s.as_tagged(heap).get_as::<DenseString>(heap) {
                out.push_str(&d.to_rust_string(heap));
            }
        }
        DenseString::from_utf8(heap, &scope, &out)
            .as_tagged(heap)
            .erase()
    })
}

/// `Array.isArray ( arg )`
/// [ES 23.1.2.3](https://tc39.es/ecma262/#sec-array.isarray)
/// [ES 7.2.2](https://tc39.es/ecma262/#sec-isarray)
/// Returns true when `arg` is an Array, following Proxy targets.
fn array_is_array<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let arg = args.get(heap, 1);
    let Some(is_array) = Object::is_array(heap, arg) else {
        raise!(vm, heap, state, VmError::RevokedProxy);
    };
    Convert::boolean(heap, is_array)
}

/// A `slice` boundary: absent/undefined → `default`, Smi passes straight
/// through, otherwise ToNumber and truncate (NaN → 0).
fn slice_bound(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    arg: Option<Handle<'_, Value>>,
    default: f64,
) -> Result<f64> {
    let Some(h) = arg else {
        return Ok(default);
    };
    let v = h.as_tagged(heap);
    if v == vm.known().undefined.get(heap) {
        return Ok(default);
    }
    if let Some(smi) = Smi::decode(v.raw()) {
        return Ok(smi.value() as f64);
    }
    let n = Object::to_numeric(vm, heap, state, h)?;
    Ok(if n.is_nan() { 0.0 } else { n.trunc() })
}

/// `Array.prototype.slice ( start, end )`
/// [ES 23.1.3.28](https://tc39.es/ecma262/#sec-array.prototype.slice)
/// Copies the `[start, end)` range into a new array, preserving holes.
fn array_slice<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let len_f = len as f64;

        let start = rt_try!(
            heap,
            slice_bound(
                vm,
                heap,
                state,
                (args.len() > 1).then(|| args.get_handle(heap, 1)),
                0.0,
            )
        );
        let end = rt_try!(
            heap,
            slice_bound(
                vm,
                heap,
                state,
                (args.len() > 2).then(|| args.get_handle(heap, 2)),
                len_f,
            )
        );
        let start = if start < 0.0 {
            (len_f + start).max(0.0)
        } else {
            start.min(len_f)
        };
        let end = if end < 0.0 {
            (len_f + end).max(0.0)
        } else {
            end.min(len_f)
        };
        let count = (end - start).max(0.0) as u64;
        let start = start as u64;
        // ArrayCreate(ES 23.1.3.33) rejects lengths above 2^32 - 1.
        if count > u32::MAX as u64 {
            raise!(vm, heap, state, VmError::IndexOutOfBounds);
        }

        // TODO: ArraySpeciesCreate (ES 23.1.3.34) once species is supported.
        let result = heap.new_array(&scope, HandleSlice::EMPTY).as_handle(&scope);
        for n in 0..count {
            let from = start + n;
            // TODO: handle the 2^53 max length/key.
            let from_key = scope.handle(rt_try!(
                heap,
                Object::to_property_key(
                    vm,
                    heap,
                    state,
                    scope.handle(Smi::new(from as i64).into_tagged()),
                )
            ));
            let present = rt_try!(
                heap,
                Object::has_property(vm, heap, state, obj, from_key.erase())
            );
            if !present {
                continue;
            }
            let value =
                match Object::get_property(vm, heap, state, obj, obj, from_key.erase(), false) {
                    Ok(v) => scope.handle(v),
                    Err(err) => return raise_runtime(heap, err),
                };
            let to_key = scope.handle(rt_try!(
                heap,
                Object::to_property_key(
                    vm,
                    heap,
                    state,
                    scope.handle(Smi::new(n as i64).into_tagged()),
                )
            ));
            let result_value = scope.handle(result.as_tagged(heap).erase());
            let key_value = scope.handle(to_key.as_tagged(heap).erase());
            match Object::create_data_property_or_throw(
                vm,
                heap,
                state,
                &scope,
                result_value,
                key_value,
                value,
            ) {
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
            }
        }
        Object::set_array_length(heap, &scope, &result, count as usize);
        result.as_tagged(heap).erase()
    })
}

/// `Array.prototype.sort ( comparator )`
/// [ES 23.1.3.30](https://tc39.es/ecma262/#sec-array.prototype.sort)
/// Sorts array-like elements in place with Rust's stable driftsort, keeping holes at the end.
fn array_sort<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // Comparator validation happens before ToObject (ES step 1).
        let cmpfn = if args.len() > 1 {
            let c = args.get(heap, 1);
            if c != vm.known().undefined.get(heap) {
                if !c.is_callable(heap) {
                    raise!(vm, heap, state, VmError::InvalidOperandType);
                }
                Some(scope.handle(c))
            } else {
                None
            }
        } else {
            None
        };

        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let undefined = vm.known().undefined.get_handle(heap, &scope).erase();
        let exception = vm.known().exception.get_handle(heap, &scope).erase();

        // SortIndexedProperties with skip-holes: read present values in
        // index order, keeping `undefined` separate so it stays at the end.
        let mut elems: Vec<Handle<'_, Value>> = Vec::new();
        let mut undefined_count = 0usize;
        for i in 0..len {
            // TODO: handle the 2^53 max length/key.
            let key = scope.handle(rt_try!(
                heap,
                Object::to_property_key(
                    vm,
                    heap,
                    state,
                    scope.handle(Smi::new(i as i64).into_tagged()),
                )
            ));
            let present = rt_try!(
                heap,
                Object::has_property(vm, heap, state, obj, key.erase())
            );
            if !present {
                continue;
            }
            let value = match Object::get_property(vm, heap, state, obj, obj, key.erase(), false) {
                Ok(v) => scope.handle(v),
                Err(err) => return raise_runtime(heap, err),
            };
            if value.as_tagged(heap) == undefined.as_tagged(heap) {
                undefined_count += 1;
            } else {
                elems.push(value);
            }
        }

        match &cmpfn {
            None => {
                let all_strings = elems
                    .iter()
                    .all(|h| h.as_tagged(heap).get_as::<DenseString>(heap).is_some());
                if all_strings {
                    elems.sort_by(|a, b| {
                        cmp_dense_strings(heap, a.as_tagged(heap), b.as_tagged(heap))
                    });
                } else {
                    // Materialize each ToString once (rooted), then sort the
                    // (value, key) pairs directly by key.
                    let mut pairs: Vec<(Handle<'_, Value>, Handle<'_, Value>)> =
                        Vec::with_capacity(elems.len());
                    for h in elems.iter() {
                        let key =
                            scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, *h)));
                        pairs.push((*h, key));
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
                    state.handle_scope(|cscope| {
                        let x = a.as_tagged(heap);
                        let y = b.as_tagged(heap);
                        let call_args = cscope.stage(&[undefined.as_tagged(heap), x, y]);
                        let result = match HostCtx::enter(
                            vm,
                            state.runtime(vm, heap),
                            heap,
                            state,
                            *cmp,
                            call_args,
                            None,
                        ) {
                            Ok(v) => cscope.handle(v),
                            Err(err) => {
                                raise_runtime(heap, err);
                                threw.set(true);
                                return core::cmp::Ordering::Equal;
                            }
                        };
                        if result.as_tagged(heap) == exception.as_tagged(heap) {
                            threw.set(true);
                            return core::cmp::Ordering::Equal;
                        }
                        match Object::to_numeric(vm, heap, state, result) {
                            Ok(n) => {
                                if n < 0.0 {
                                    core::cmp::Ordering::Less
                                } else if n > 0.0 {
                                    core::cmp::Ordering::Greater
                                } else {
                                    core::cmp::Ordering::Equal
                                }
                            }
                            Err(err) => {
                                raise_runtime(heap, err);
                                threw.set(true);
                                core::cmp::Ordering::Equal
                            }
                        }
                    })
                });
                if threw.get() {
                    return vm.known().exception.get(heap).erase();
                }
            }
        }

        // undefined values go after all defined values, before holes.
        for _ in 0..undefined_count {
            elems.push(undefined);
        }
        let item_count = elems.len() as u64;

        // Set(O, ToString(j), sortedList[j], true).
        for (j, value) in elems.iter().enumerate() {
            // TODO: handle the 2^53 max length/key.
            let key = scope
                .handle(rt_try!(
                    heap,
                    Object::to_property_key(
                        vm,
                        heap,
                        state,
                        scope.handle(Smi::new(j as i64).into_tagged()),
                    )
                ))
                .erase();
            match Object::set_property(
                vm,
                heap,
                state,
                obj,
                obj,
                key,
                *value,
                StoreSemantics::Shadow,
                false,
                LanguageMode::Strict,
            ) {
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
            }
        }

        // Delete the remaining indices so holes are preserved.
        for j in item_count..len {
            // TODO: handle the 2^53 max length/key.
            let key = scope
                .handle(rt_try!(
                    heap,
                    Object::to_property_key(
                        vm,
                        heap,
                        state,
                        scope.handle(Smi::new(j as i64).into_tagged()),
                    )
                ))
                .erase();
            match Object::delete_property(vm, heap, state, obj, key) {
                Ok(true) => {}
                Ok(false) => {
                    raise!(vm, heap, state, VmError::InvalidOperandType);
                }
                Err(err) => return raise_runtime(heap, err),
            }
        }
        obj.as_tagged(heap)
    })
}

/// `Array.prototype.at ( index )`
/// [ES 23.1.3.1](https://tc39.es/ecma262/#sec-array.prototype.at)
/// Element at `index`, counting from the end when negative.
fn array_at<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let k = rt_try!(
            heap,
            Convert::to_absolute_index(vm, heap, state, args.get_handle(heap, 1), len)
        );
        if k < 0.0 || k >= len as f64 {
            return vm.known().undefined.get(heap).erase();
        }
        let key = match index_key(vm, heap, state, &scope, k as u64) {
            Ok(key) => key,
            Err(err) => return raise_runtime(heap, err),
        };
        let value = match Object::get_property(vm, heap, state, obj, obj, key.erase(), false) {
            Ok(value) => scope.handle(value),
            Err(err) => return raise_runtime(heap, err),
        };
        value.as_tagged(heap).erase()
    })
}

/// `Array.prototype.includes ( searchElement [ , fromIndex ] )`
/// [ES 23.1.3.16](https://tc39.es/ecma262/#sec-array.prototype.includes)
/// SameValueZero search from `fromIndex`; holes read as `undefined`.
fn array_includes<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        if len == 0 {
            return Convert::boolean(heap, false);
        }
        let search = args.get_handle(heap, 1);
        let len_f = len as f64;
        let mut k = rt_try!(
            heap,
            Convert::to_clamped_index(vm, heap, state, args.get_handle(heap, 2), len)
        );
        while k < len_f {
            let key = rt_try!(heap, index_key(vm, heap, state, &scope, k as u64));
            let value = match Object::get_property(vm, heap, state, obj, obj, key.erase(), false) {
                Ok(value) => scope.handle(value),
                Err(err) => return raise_runtime(heap, err),
            };
            if Compare::same_value_zero(heap, search.as_tagged(heap), value.as_tagged(heap)) {
                return Convert::boolean(heap, true);
            }
            k += 1.0;
        }
        Convert::boolean(heap, false)
    })
}

/// `Array.prototype.indexOf ( searchElement [ , fromIndex ] )`
/// [ES 23.1.3.17](https://tc39.es/ecma262/#sec-array.prototype.indexOf)
/// Smallest strictly-equal index at or after `fromIndex`, else `-1`.
fn array_index_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        if len == 0 {
            return heap.new_number(-1.0);
        }
        let search = args.get_handle(heap, 1);
        let len_f = len as f64;
        let mut k = rt_try!(
            heap,
            Convert::to_clamped_index(vm, heap, state, args.get_handle(heap, 2), len)
        );
        while k < len_f {
            let index = k as u64;
            match get_indexed_value(vm, heap, state, &scope, obj, index) {
                Ok(IndexedValue::Absent) => {}
                Ok(IndexedValue::Present(value)) => {
                    if Compare::strict_equal(heap, search.as_tagged(heap), value.as_tagged(heap)) {
                        return heap.new_number(index as f64);
                    }
                }
                Err(err) => return raise_runtime(heap, err),
            }
            k += 1.0;
        }
        heap.new_number(-1.0)
    })
}

/// `Array.prototype.lastIndexOf ( searchElement [ , fromIndex ] )`
/// [ES 23.1.3.20](https://tc39.es/ecma262/#sec-array.prototype.lastindexof)
/// Largest strictly-equal index at or before `fromIndex`, else `-1`.
fn array_last_index_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        if len == 0 {
            return heap.new_number(-1.0);
        }
        let search = args.get_handle(heap, 1);
        let len_f = len as f64;
        let mut k = if args.len() > 2 {
            let absolute = rt_try!(
                heap,
                Convert::to_absolute_index(vm, heap, state, args.get_handle(heap, 2), len)
            );
            absolute.min(len_f - 1.0)
        } else {
            len_f - 1.0
        };
        while k >= 0.0 {
            let index = k as u64;
            match get_indexed_value(vm, heap, state, &scope, obj, index) {
                Ok(IndexedValue::Absent) => {}
                Ok(IndexedValue::Present(value)) => {
                    if Compare::strict_equal(heap, search.as_tagged(heap), value.as_tagged(heap)) {
                        return heap.new_number(index as f64);
                    }
                }
                Err(err) => return raise_runtime(heap, err),
            }
            k -= 1.0;
        }
        heap.new_number(-1.0)
    })
}

/// `Array.prototype.fill ( value [ , start [ , end ] ] )`
/// [ES 23.1.3.7](https://tc39.es/ecma262/#sec-array.prototype.fill)
/// Writes `value` over `[start, end)` and returns the receiver.
fn array_fill<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let len_f = len as f64;
        let value = args.get_handle(heap, 1);
        let mut k = rt_try!(
            heap,
            Convert::to_clamped_index(vm, heap, state, args.get_handle(heap, 2), len)
        );
        let end_arg = args.get_handle(heap, 3);
        let final_index = if end_arg.as_tagged(heap) == vm.known().undefined.get(heap) {
            len_f
        } else {
            rt_try!(
                heap,
                Convert::to_clamped_index(vm, heap, state, end_arg, len)
            )
        };
        while k < final_index {
            let key = rt_try!(heap, index_key(vm, heap, state, &scope, k as u64));
            match Object::set_property(
                vm,
                heap,
                state,
                obj,
                obj,
                key.erase(),
                value,
                StoreSemantics::Shadow,
                false,
                LanguageMode::Strict,
            ) {
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
            }
            k += 1.0;
        }
        obj.as_tagged(heap)
    })
}

/// `Array.prototype.reverse ( )`
/// [ES 23.1.3.26](https://tc39.es/ecma262/#sec-array.prototype.reverse)
/// Reverses the elements in place, moving holes with them.
fn array_reverse<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let middle = len / 2;
        let mut lower = 0u64;
        while lower != middle {
            let upper = len - lower - 1;
            let lower_key = rt_try!(heap, index_key(vm, heap, state, &scope, lower));
            let upper_key = rt_try!(heap, index_key(vm, heap, state, &scope, upper));
            let lower_present = rt_try!(
                heap,
                Object::has_property(vm, heap, state, obj, lower_key.erase())
            );
            let lower_value = if lower_present {
                match Object::get_property(vm, heap, state, obj, obj, lower_key.erase(), false) {
                    Ok(value) => Some(scope.handle(value)),
                    Err(err) => return raise_runtime(heap, err),
                }
            } else {
                None
            };
            let upper_present = rt_try!(
                heap,
                Object::has_property(vm, heap, state, obj, upper_key.erase())
            );
            let upper_value = if upper_present {
                match Object::get_property(vm, heap, state, obj, obj, upper_key.erase(), false) {
                    Ok(value) => Some(scope.handle(value)),
                    Err(err) => return raise_runtime(heap, err),
                }
            } else {
                None
            };
            match (lower_value, upper_value) {
                (Some(lower_value), Some(upper_value)) => {
                    if let Err(err) = Object::set_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        lower_key.erase(),
                        upper_value,
                        StoreSemantics::Shadow,
                        false,
                        LanguageMode::Strict,
                    ) {
                        return raise_runtime(heap, err);
                    }
                    if let Err(err) = Object::set_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        upper_key.erase(),
                        lower_value,
                        StoreSemantics::Shadow,
                        false,
                        LanguageMode::Strict,
                    ) {
                        return raise_runtime(heap, err);
                    }
                }
                (None, Some(upper_value)) => {
                    if let Err(err) = Object::set_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        lower_key.erase(),
                        upper_value,
                        StoreSemantics::Shadow,
                        false,
                        LanguageMode::Strict,
                    ) {
                        return raise_runtime(heap, err);
                    }
                    match Object::delete_property(vm, heap, state, obj, upper_key.erase()) {
                        Ok(true) => {}
                        Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
                        Err(err) => return raise_runtime(heap, err),
                    }
                }
                (Some(lower_value), None) => {
                    match Object::delete_property(vm, heap, state, obj, lower_key.erase()) {
                        Ok(true) => {}
                        Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
                        Err(err) => return raise_runtime(heap, err),
                    }
                    if let Err(err) = Object::set_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        upper_key.erase(),
                        lower_value,
                        StoreSemantics::Shadow,
                        false,
                        LanguageMode::Strict,
                    ) {
                        return raise_runtime(heap, err);
                    }
                }
                (None, None) => {}
            }
            lower += 1;
        }
        obj.as_tagged(heap)
    })
}

/// `Array.prototype.shift ( )`
/// [ES 23.1.3.27](https://tc39.es/ecma262/#sec-array.prototype.shift)
/// Removes the first element and shifts the rest down one index.
fn array_shift<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let length_key = vm.known().strings.length.get_handle(heap, &scope).erase();
        if len == 0 {
            let zero = scope.handle(heap.new_number(0.0));
            if let Err(err) = Object::set_property(
                vm,
                heap,
                state,
                obj,
                obj,
                length_key,
                zero,
                StoreSemantics::Shadow,
                false,
                LanguageMode::Strict,
            ) {
                return raise_runtime(heap, err);
            }
            return vm.known().undefined.get(heap).erase();
        }
        let first_key = rt_try!(heap, index_key(vm, heap, state, &scope, 0));
        let first = match Object::get_property(vm, heap, state, obj, obj, first_key.erase(), false)
        {
            Ok(value) => scope.handle(value),
            Err(err) => return raise_runtime(heap, err),
        };
        let mut k = 1u64;
        while k < len {
            let from_key = rt_try!(heap, index_key(vm, heap, state, &scope, k));
            let to_key = rt_try!(heap, index_key(vm, heap, state, &scope, k - 1));
            let present = rt_try!(
                heap,
                Object::has_property(vm, heap, state, obj, from_key.erase())
            );
            if present {
                let value = match Object::get_property(
                    vm,
                    heap,
                    state,
                    obj,
                    obj,
                    from_key.erase(),
                    false,
                ) {
                    Ok(value) => scope.handle(value),
                    Err(err) => return raise_runtime(heap, err),
                };
                if let Err(err) = Object::set_property(
                    vm,
                    heap,
                    state,
                    obj,
                    obj,
                    to_key.erase(),
                    value,
                    StoreSemantics::Shadow,
                    false,
                    LanguageMode::Strict,
                ) {
                    return raise_runtime(heap, err);
                }
            } else {
                match Object::delete_property(vm, heap, state, obj, to_key.erase()) {
                    Ok(true) => {}
                    Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
                    Err(err) => return raise_runtime(heap, err),
                }
            }
            k += 1;
        }
        let last_key = rt_try!(heap, index_key(vm, heap, state, &scope, len - 1));
        match Object::delete_property(vm, heap, state, obj, last_key.erase()) {
            Ok(true) => {}
            Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
            Err(err) => return raise_runtime(heap, err),
        }
        let new_len = scope.handle(heap.new_number((len - 1) as f64));
        if let Err(err) = Object::set_property(
            vm,
            heap,
            state,
            obj,
            obj,
            length_key,
            new_len,
            StoreSemantics::Shadow,
            false,
            LanguageMode::Strict,
        ) {
            return raise_runtime(heap, err);
        }
        first.as_tagged(heap).erase()
    })
}

/// `Array.prototype.unshift ( ...items )`
/// [ES 23.1.3.36](https://tc39.es/ecma262/#sec-array.prototype.unshift)
/// Prepends the arguments and returns the new length.
fn array_unshift<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, state, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(heap, err),
        };
        let len = rt_try!(
            heap,
            Object::length_of_array_like(vm, heap, state, &scope, obj)
        );
        let arg_count = args.len().saturating_sub(1) as u64;
        if arg_count > 0 {
            if len.saturating_add(arg_count) > (1u64 << 53) - 1 {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
            // an array's length store would throw at the end; reject first
            // so a huge sparse receiver is not walked 2^32 times
            if obj
                .as_tagged(heap)
                .as_heap_object()
                .is_some_and(|o| o.as_ref().is_array_exotic(heap))
                && len.saturating_add(arg_count) > u32::MAX as u64
            {
                raise!(vm, heap, state, VmError::InvalidArrayLength);
            }
            let mut k = len;
            while k > 0 {
                let from_key = rt_try!(heap, index_key(vm, heap, state, &scope, k - 1));
                let to_key = rt_try!(heap, index_key(vm, heap, state, &scope, k + arg_count - 1));
                let present = rt_try!(
                    heap,
                    Object::has_property(vm, heap, state, obj, from_key.erase())
                );
                if present {
                    let value = match Object::get_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        from_key.erase(),
                        false,
                    ) {
                        Ok(value) => scope.handle(value),
                        Err(err) => return raise_runtime(heap, err),
                    };
                    if let Err(err) = Object::set_property(
                        vm,
                        heap,
                        state,
                        obj,
                        obj,
                        to_key.erase(),
                        value,
                        StoreSemantics::Shadow,
                        false,
                        LanguageMode::Strict,
                    ) {
                        return raise_runtime(heap, err);
                    }
                } else {
                    match Object::delete_property(vm, heap, state, obj, to_key.erase()) {
                        Ok(true) => {}
                        Ok(false) => raise!(vm, heap, state, VmError::InvalidOperandType),
                        Err(err) => return raise_runtime(heap, err),
                    }
                }
                k -= 1;
            }
            for j in 0..arg_count {
                let key = rt_try!(heap, index_key(vm, heap, state, &scope, j));
                let item = args.get_handle(heap, 1 + j as usize);
                if let Err(err) = Object::set_property(
                    vm,
                    heap,
                    state,
                    obj,
                    obj,
                    key.erase(),
                    item,
                    StoreSemantics::Shadow,
                    false,
                    LanguageMode::Strict,
                ) {
                    return raise_runtime(heap, err);
                }
            }
        }
        let new_len = len + arg_count;
        let length_key = vm.known().strings.length.get_handle(heap, &scope).erase();
        let length_value = scope.handle(heap.new_number(new_len as f64));
        if let Err(err) = Object::set_property(
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
            return raise_runtime(heap, err);
        }
        length_value.as_tagged(heap)
    })
}

/// `Array.of ( ...items )`
/// [ES 23.1.2.3](https://tc39.es/ecma262/#sec-array.of)
/// Builds an array (or constructs a subclass instance) from the arguments.
fn array_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let count = args.len().saturating_sub(1) as u64;
        let this = args.get_handle(heap, 0);
        let length_value = scope.handle(heap.new_number(count as f64));
        let is_constructor = this
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|obj| obj.map(heap).kind().is_constructor());
        let array: Handle<'_, Value> = if is_constructor {
            match Object::construct_forward(vm, heap, state, this, &[length_value], this) {
                Ok(value) => scope.handle(value),
                Err(err) => return raise_runtime(heap, err),
            }
        } else {
            // ArrayCreate rejects lengths above 2^32 - 1.
            if count > u32::MAX as u64 {
                raise!(vm, heap, state, VmError::IndexOutOfBounds);
            }
            heap.new_array(&scope, HandleSlice::EMPTY)
                .as_handle(&scope)
                .erase()
        };
        for j in 0..count {
            let key = rt_try!(heap, index_key(vm, heap, state, &scope, j));
            let item = args.get_handle(heap, 1 + j as usize);
            match Object::create_data_property_or_throw(
                vm,
                heap,
                state,
                &scope,
                array,
                key.erase(),
                item,
            ) {
                Ok(_) => {}
                Err(err) => return raise_runtime(heap, err),
            }
        }
        let length_key = vm.known().strings.length.get_handle(heap, &scope).erase();
        if let Err(err) = Object::set_property(
            vm,
            heap,
            state,
            array,
            array,
            length_key,
            length_value,
            StoreSemantics::Shadow,
            false,
            LanguageMode::Strict,
        ) {
            return raise_runtime(heap, err);
        }
        array.as_tagged(heap).erase()
    })
}

/// `ToString(𝔽(index))` as a property key.
fn index_key<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    index: u64,
) -> Result<Handle<'s, SlotName>> {
    // TODO: handle the 2^53 max length/key.
    let number = scope.handle(Smi::new(index as i64).into_tagged());
    Ok(scope.handle(Object::to_property_key(vm, heap, state, number)?))
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
