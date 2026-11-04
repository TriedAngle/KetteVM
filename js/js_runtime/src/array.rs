use vm_core::HostCtx;
use vm_core::proxy::Flow;
use vm_core::{
    Args, Coercion, Convert, DenseString, Float, Handle, HandleScope, HandleSlice, Heap,
    LanguageMode, Object, PropertyDescriptor, Prototype, Smi, StoreSemantics, Tagged, ThreadState,
    VM, Value, VmError,
};
use vm_core::{raise_runtime, rt_try};

use crate::JSRuntime;
use crate::object::is_array;

const ARRAY_ITERATOR_KIND_KEY: i64 = 0;
const ARRAY_ITERATOR_KIND_VALUE: i64 = 1;
const ARRAY_ITERATOR_KIND_KEY_VALUE: i64 = 2;

enum IndexedValue<'s> {
    Absent,
    Present(Handle<'s, Value>),
    Threw,
}

/// Get `obj[index]`, using a direct own-element read when possible.
fn get_indexed_value<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: Handle<'_, Value>,
    index: u64,
) -> Result<IndexedValue<'s>, VmError> {
    if let Some(o) = obj.as_tagged(heap).as_heap_object()
        && let Some(value) = o.element_value(heap, index as usize)
    {
        return Ok(IndexedValue::Present(scope.handle(value)));
    }
    // TODO: handle the 2^53 max length/key.
    let key = match Object::to_property_key(
        vm,
        heap,
        state,
        scope.handle(Smi::new(index as i64).into_tagged()),
    ) {
        Ok(Some(key)) => scope.handle(key),
        Ok(None) => return Ok(IndexedValue::Threw),
        Err(err) => return Err(err),
    };
    let present = match Object::has_property(vm, heap, state, obj, key.erase()) {
        Ok(Coercion::Value(v)) => scope.handle(v),
        Ok(Coercion::Threw) => return Ok(IndexedValue::Threw),
        Err(err) => return Err(err),
    };
    if !present.as_tagged(heap).is_truthy(heap) {
        return Ok(IndexedValue::Absent);
    }
    match Object::get_property(vm, heap, state, obj, obj, key.erase(), false) {
        Ok(Coercion::Value(v)) => Ok(IndexedValue::Present(scope.handle(v))),
        Ok(Coercion::Threw) => Ok(IndexedValue::Threw),
        Err(err) => Err(err),
    }
}

/// `Array ( ...values )`
/// [ES 23.1.1.1](https://tc39.es/ecma262/#sec-array-constructor-array)
/// Creates an array from `values`; the prototype comes from `new.target`.
pub fn array_constructor<'a>(
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
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match array_push_fast(heap, state, args) {
        Some(Ok(len)) => return Smi::new(len as i64).into_tagged(),
        Some(Err(err)) => return raise_runtime(vm, heap, state, err),
        None => {}
    }
    state.handle_scope(|scope| {
        let this = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, &scope, this) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };

        let length_key = scope.handle(vm.known().strings.length.get(heap).erase());
        let mut length = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(length)) => length,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
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

/// Allocation Free Array push fast path if space is remaining.
/// Returns `None` when the generic `[[Set]]` algorithm is required.
#[inline]
fn array_push_fast(
    heap: &mut Heap,
    state: &ThreadState,
    args: Args,
) -> Option<Result<usize, VmError>> {
    if args.len() != 2 {
        return None;
    }
    let len = {
        let receiver = args.get_handle(heap, 0);
        let r = receiver.as_tagged(heap);
        let obj = r.as_heap_object()?;
        if !obj.as_ref().is_array(heap) {
            return None;
        }
        let len = obj.as_ref().length();
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
            return Some(Ok(len + 1));
        }
        len
    };
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = receiver.as_tagged(heap).as_heap_object()?;
        let receiver_obj = scope.handle(obj);
        let arg = args.get_handle(heap, 1);
        match Object::store_array_element(heap, &scope, &receiver_obj, len, &arg) {
            Ok(()) => Some(Ok(len + 1)),
            Err(err) => Some(Err(err)),
        }
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
pub fn array_pop<'a>(
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
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };

        // LengthOfArrayLike (ES 7.3.18): Get(obj, "length") then ToLength.
        let length_key = scope.handle(vm.known().strings.length.get(heap).erase());
        let length = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(length)) => length,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };

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
                Ok(Flow::Value(_)) => {}
                Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
            return vm.known().undefined.get(heap).erase();
        }

        let new_length = length - 1;
        let new_length_value = scope.handle(Smi::new(new_length as i64).into_tagged());
        // ToString(newLen), interned: named keys compare by pointer.
        let index = match Object::to_property_key(vm, heap, state, new_length_value) {
            Ok(Some(key)) => scope.handle(key.erase()),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let element = match Object::get_property(vm, heap, state, obj, obj, index, false) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        match Object::delete_property(vm, heap, state, obj, index) {
            Ok(Flow::Value(true)) => {}
            Ok(Flow::Value(false)) => return raise_runtime(vm, heap, state, VmError::Type),
            Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
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
            Ok(Flow::Value(_)) => {}
            Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
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
    if !obj.as_ref().is_array(heap) || !obj.as_ref().map(heap).length_writable(heap) {
        return None;
    }
    let len = obj.as_ref().length();
    if len == 0 {
        return Some(scope.handle(heap.known().undefined.get(heap).erase()));
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
pub fn array_values<'a>(
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
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let map = scope.handle(known.array_iterator_map.get(heap));
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
pub fn array_iterator_next<'a>(
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
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let iterator = scope.handle(iterator);
        if !iterator
            .as_tagged(heap)
            .map(heap)
            .ptr_eq(known.array_iterator_map.get(heap))
        {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let (array, index, kind) = {
            let slots = iterator.as_tagged(heap).slots(heap);
            if slots.len() < 3 {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
            (
                scope.handle(slots.at(heap, 0)),
                slots.at(heap, 1).raw(),
                slots.at(heap, 2).raw(),
            )
        };
        let undefined = scope.handle(vm.known().undefined.get(heap).erase());

        let (result, done) = if array.as_tagged(heap).ptr_eq(undefined.as_tagged(heap)) {
            (undefined, true)
        } else {
            let Some(index) = Smi::decode(index) else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let Some(kind) = Smi::decode(kind).map(|s| s.value()) else {
                return raise_runtime(vm, heap, state, VmError::Type);
            };
            let index = index.value() as u64;
            let length = match Object::length_of_array_like(vm, heap, state, &scope, array) {
                Ok(Some(length)) => length,
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
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
                        let index_key = match Object::to_property_key(
                            vm,
                            heap,
                            state,
                            scope.handle(index_number),
                        ) {
                            Ok(Some(key)) => scope.handle(key.erase()),
                            Ok(None) => return vm.known().exception.get(heap).erase(),
                            Err(err) => return raise_runtime(vm, heap, state, err),
                        };
                        let value = match Object::get_property(
                            vm, heap, state, array, array, index_key, false,
                        ) {
                            Ok(Coercion::Value(v)) => scope.handle(v),
                            Ok(Coercion::Threw) => {
                                return vm.known().exception.get(heap).erase();
                            }
                            Err(err) => return raise_runtime(vm, heap, state, err),
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
                    _ => return raise_runtime(vm, heap, state, VmError::Type),
                };
                (result, false)
            }
        };
        let map = scope.handle(known.iterator_result_map.get(heap));
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
pub fn array_iterator_symbol_iterator<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let arg = args.get(heap, 0);
    arg
}

/// `Array.prototype.join ( separator )`
/// [ES 23.1.3.18](https://tc39.es/ecma262/#sec-array.prototype.join)
/// Joins array-like elements with `separator`, using `","` by default.
pub fn array_join<'a>(
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
pub fn array_map<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let len = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(len)) => len,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let callback = args.get_handle(heap, 1);
        if !callback.as_tagged(heap).is_callable(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // ArrayCreate(ES 23.1.3.33) rejects lengths above 2^32 - 1.
        if len > u32::MAX as u64 {
            return raise_runtime(vm, heap, state, VmError::OutOfBounds);
        }
        let this_arg = if args.len() > 2 {
            args.get_handle(heap, 2)
        } else {
            scope.handle(vm.known().undefined.get(heap).erase())
        };
        let exception = scope.handle(vm.known().exception.get(heap).erase());

        // TODO: ArraySpeciesCreate (ES 23.1.3.34) once species is supported.
        let result = heap.new_array(&scope, HandleSlice::EMPTY).as_handle(&scope);
        for k in 0..len {
            let value = match get_indexed_value(vm, heap, state, &scope, obj, k) {
                Ok(IndexedValue::Absent) => continue,
                Ok(IndexedValue::Present(value)) => value,
                Ok(IndexedValue::Threw) => return exception.as_tagged(heap),
                Err(err) => return raise_runtime(vm, heap, state, err),
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
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            if mapped.as_tagged(heap) == exception.as_tagged(heap) {
                return exception.as_tagged(heap);
            }
            if let Err(err) =
                Object::store_array_element(heap, &scope, &result, k as usize, &mapped)
            {
                return raise_runtime(vm, heap, state, err);
            }
        }
        if let Err(err) = Object::set_array_length(heap, &scope, &result, len as usize) {
            return raise_runtime(vm, heap, state, err);
        }
        result.as_tagged(heap).erase()
    })
}

/// `Array.prototype.reduce ( callback [ , initialValue ] )`
/// [ES 23.1.3.24](https://tc39.es/ecma262/#sec-array.prototype.reduce)
/// Reduces present array-like elements from left to right.
pub fn array_reduce<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let len = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(len)) => len,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let callback = args.get_handle(heap, 1);
        if !callback.as_tagged(heap).is_callable(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let undefined = scope.handle(vm.known().undefined.get(heap).erase());
        let exception = scope.handle(vm.known().exception.get(heap).erase());
        let mut k = 0u64;

        let mut accumulator = if args.len() > 2 {
            scope.handle(args.get_handle(heap, 2).as_tagged(heap))
        } else {
            let mut first: Option<Handle<'_, Value>> = None;
            while k < len && first.is_none() {
                match get_indexed_value(vm, heap, state, &scope, obj, k) {
                    Ok(IndexedValue::Absent) => {}
                    Ok(IndexedValue::Present(value)) => first = Some(value),
                    Ok(IndexedValue::Threw) => return exception.as_tagged(heap),
                    Err(err) => return raise_runtime(vm, heap, state, err),
                }
                k += 1;
            }
            let Some(value) = first else {
                return raise_runtime(vm, heap, state, VmError::Type);
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
                Ok(IndexedValue::Threw) => return exception.as_tagged(heap),
                Err(err) => return raise_runtime(vm, heap, state, err),
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
                Err(err) => return raise_runtime(vm, heap, state, err),
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
pub fn array_to_string<'a>(
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
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let join_key = scope.handle(known.strings.join.get(heap).erase());
        let join = match Object::get_property(vm, heap, state, obj, obj, join_key, false) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let func = if join.as_tagged(heap).is_callable(heap) {
            join
        } else {
            scope.handle(known.object_to_string.get(heap).erase())
        };
        let call_args = scope.stage(&[obj.as_tagged(heap).erase()]);
        let outcome = match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            func,
            call_args,
            None,
        ) {
            Ok(value) => Ok(scope.handle(value)),
            Err(err) => Err(err),
        };
        match outcome {
            Ok(value) => value.as_tagged(heap).erase(),
            Err(err) => raise_runtime(vm, heap, state, err),
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
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };

        let length = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(length)) => length,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };

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

        let mut out = String::new();
        for i in 0..length {
            if i > 0 {
                out.push_str(&sep);
            }
            let index = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(i as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key.erase()),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            let element = match Object::get_property(vm, heap, state, obj, obj, index, false) {
                Ok(Coercion::Value(v)) => scope.handle(v),
                Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            let element_value = element.as_tagged(heap);
            if element_value == vm.known().undefined.get(heap)
                || element_value == vm.known().null.get(heap)
            {
                continue;
            }
            match Object::to_string(vm, heap, state, element) {
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

/// `Array.isArray ( arg )`
/// [ES 23.1.2.3](https://tc39.es/ecma262/#sec-array.isarray)
/// [ES 7.2.2](https://tc39.es/ecma262/#sec-isarray)
/// Returns true when `arg` is an Array, following Proxy targets.
pub fn array_is_array<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let arg = args.get(heap, 1);
    let is_array = match is_array(heap, arg) {
        Ok(is_array) => is_array,
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
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

/// `Array.prototype.slice ( start, end )`
/// [ES 23.1.3.28](https://tc39.es/ecma262/#sec-array.prototype.slice)
/// Copies the `[start, end)` range into a new array, preserving holes.
pub fn array_slice<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let len = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(len)) => len,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let len_f = len as f64;

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
            len_f,
        ) {
            Ok(Some(v)) => v,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
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
            return raise_runtime(vm, heap, state, VmError::OutOfBounds);
        }

        // TODO: ArraySpeciesCreate (ES 23.1.3.34) once species is supported.
        let result = heap.new_array(&scope, HandleSlice::EMPTY).as_handle(&scope);
        for n in 0..count {
            let from = start + n;
            // TODO: handle the 2^53 max length/key.
            let from_key = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(from as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            let present = match Object::has_property(vm, heap, state, obj, from_key.erase()) {
                Ok(Coercion::Value(v)) => scope.handle(v),
                Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            if !present.as_tagged(heap).is_truthy(heap) {
                continue;
            }
            let value =
                match Object::get_property(vm, heap, state, obj, obj, from_key.erase(), false) {
                    Ok(Coercion::Value(v)) => scope.handle(v),
                    Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                    Err(err) => return raise_runtime(vm, heap, state, err),
                };
            let to_key = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(n as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            match Object::define_own_property(
                heap,
                &scope,
                result,
                to_key,
                PropertyDescriptor::data(value),
            ) {
                Ok(true) => {}
                Ok(false) => return raise_runtime(vm, heap, state, VmError::Type),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        }
        if let Err(err) = Object::set_array_length(heap, &scope, &result, count as usize) {
            return raise_runtime(vm, heap, state, err);
        }
        result.as_tagged(heap).erase()
    })
}

/// `Array.prototype.sort ( comparator )`
/// [ES 23.1.3.30](https://tc39.es/ecma262/#sec-array.prototype.sort)
/// Sorts array-like elements in place with Rust's stable driftsort, keeping holes at the end.
pub fn array_sort<'a>(
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
                    return raise_runtime(vm, heap, state, VmError::Type);
                }
                Some(scope.handle(c))
            } else {
                None
            }
        } else {
            None
        };

        let receiver = args.get_handle(heap, 0);
        let obj = match Convert::to_object(vm, heap, &scope, receiver) {
            Ok(obj) => obj.erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let len = match Object::length_of_array_like(vm, heap, state, &scope, obj) {
            Ok(Some(len)) => len,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let undefined = scope.handle(vm.known().undefined.get(heap).erase());
        let exception = scope.handle(vm.known().exception.get(heap).erase());

        // SortIndexedProperties with skip-holes: read present values in
        // index order, keeping `undefined` separate so it stays at the end.
        let mut elems: Vec<Handle<'_, Value>> = Vec::new();
        let mut undefined_count = 0usize;
        for i in 0..len {
            // TODO: handle the 2^53 max length/key.
            let key = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(i as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            let present = match Object::has_property(vm, heap, state, obj, key.erase()) {
                Ok(Coercion::Value(v)) => scope.handle(v),
                Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            if !present.as_tagged(heap).is_truthy(heap) {
                continue;
            }
            let value = match Object::get_property(vm, heap, state, obj, obj, key.erase(), false) {
                Ok(Coercion::Value(v)) => scope.handle(v),
                Ok(Coercion::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
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
                        let key = match Object::to_string(vm, heap, state, *h) {
                            Ok(Some(s)) => scope.handle(s),
                            Ok(None) => return vm.known().exception.get(heap).erase(),
                            Err(err) => return raise_runtime(vm, heap, state, err),
                        };
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
                                raise_runtime(vm, heap, state, err);
                                threw.set(true);
                                return core::cmp::Ordering::Equal;
                            }
                        };
                        if result.as_tagged(heap) == exception.as_tagged(heap) {
                            threw.set(true);
                            return core::cmp::Ordering::Equal;
                        }
                        match Object::to_numeric(vm, heap, state, result) {
                            Ok(Some(n)) => {
                                if n < 0.0 {
                                    core::cmp::Ordering::Less
                                } else if n > 0.0 {
                                    core::cmp::Ordering::Greater
                                } else {
                                    core::cmp::Ordering::Equal
                                }
                            }
                            Ok(None) => {
                                threw.set(true);
                                core::cmp::Ordering::Equal
                            }
                            Err(err) => {
                                raise_runtime(vm, heap, state, err);
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
            let key = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(j as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key).erase(),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
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
                Ok(Flow::Value(_)) => {}
                Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        }

        // Delete the remaining indices so holes are preserved.
        for j in item_count..len {
            // TODO: handle the 2^53 max length/key.
            let key = match Object::to_property_key(
                vm,
                heap,
                state,
                scope.handle(Smi::new(j as i64).into_tagged()),
            ) {
                Ok(Some(key)) => scope.handle(key).erase(),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            match Object::delete_property(vm, heap, state, obj, key) {
                Ok(Flow::Value(true)) => {}
                Ok(Flow::Value(false)) => return raise_runtime(vm, heap, state, VmError::Type),
                Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        }
        obj.as_tagged(heap)
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
