//! Iterator protocol natives (ES 8.5): GetIterator, IteratorNext,
//! IteratorComplete and IteratorValue.
use crate::JSRuntime;
use js_compiler::native::NativeIds;
use vm_core::{
    Args, Convert, Handle, HostCtx, Object, Tagged, Value, VmError, raise, raise_runtime, rt_try,
};

crate::register_natives! {
    NativeIds;
    get_iterator,
    iterator_next,
    iterator_done,
    iterator_value,
}

/// GetIterator (ES 8.5.4): (obj) -> iterator.
pub fn get_iterator<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let obj = scope.handle(args.get(heap, 0));
        let symbol = known.iterator_symbol.get_handle(heap, &scope).erase();
        let method = scope.handle(rt_try!(
            heap,
            Object::get_property(vm, heap, state, obj, obj, symbol, false)
        ));
        let method_tagged = method.as_tagged(heap);
        if method_tagged.ptr_eq(vm.known().undefined.get(heap))
            || method_tagged.ptr_eq(vm.known().null.get(heap))
            || !method_tagged.is_callable(heap)
        {
            raise!(vm, heap, state, VmError::InvalidOperandType); // "obj is not iterable"
        }
        let call_args = scope.stage_handles(&[obj]);
        match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            method,
            call_args,
            None,
        ) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        }
        .as_tagged(heap)
    })
}

/// IteratorNext (ES 8.5.6): (iterator) -> result object.
pub fn iterator_next<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let iter = scope.handle(args.get(heap, 0));
        let next_name = vm.known().strings.next.get_handle(heap, &scope).erase();
        let next = scope.handle(rt_try!(
            heap,
            Object::get_property(vm, heap, state, iter, iter, next_name, false)
        ));
        let call_args = scope.stage_handles(&[iter]);
        let result = match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            next,
            call_args,
            None,
        ) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        };
        if result
            .as_tagged(heap)
            .ptr_eq(vm.known().exception.get(heap))
        {
            return result.as_tagged(heap);
        }
        if result.as_tagged(heap).is_primitive(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType); // IteratorNext result must be an Object
        }
        result.as_tagged(heap)
    })
}

/// IteratorComplete (ES 8.5.7): (result) -> bool.
pub fn iterator_done<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let result = scope.handle(args.get(heap, 0));
        let done_name = known.strings.done.get_handle(heap, &scope).erase();
        let v = scope.handle(rt_try!(
            heap,
            Object::get_property(vm, heap, state, result, result, done_name, false)
        ));
        let truthy = v.as_tagged(heap).is_truthy(heap);
        Convert::boolean(heap, truthy)
    })
}

/// IteratorValue (ES 8.5.8): (result) -> value.
pub fn iterator_value<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let result = scope.handle(args.get(heap, 0));
        let value_name = vm.known().strings.value.get_handle(heap, &scope).erase();
        let v = scope.handle(rt_try!(
            heap,
            Object::get_property(vm, heap, state, result, result, value_name, false)
        ));
        v.as_tagged(heap)
    })
}
