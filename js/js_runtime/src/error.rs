use vm_core::Coercion;
use vm_core::Errors;
use vm_core::HostCtx;

use vm_core::{
    Args, Convert, DenseString, Handle, HandleSlice, Object, PropertyDescriptor, Tagged, Value,
};
use vm_core::{raise_runtime, rt_try};

pub fn error_constructor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    make_error(nctx, args, "Error")
}

pub fn type_error_constructor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    make_error(nctx, args, "TypeError")
}

pub fn reference_error_constructor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    make_error(nctx, args, "ReferenceError")
}

pub fn range_error_constructor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    make_error(nctx, args, "RangeError")
}

pub fn syntax_error_constructor<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    make_error(nctx, args, "SyntaxError")
}

pub fn make_error<'a>(nctx: HostCtx<'a>, args: Args, class: &str) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        // root the message right away: the allocations below (new_object,
        // interning) would leave a raw copy stale
        let message = if args.len() > 1 {
            let v = scope.handle(args.get(heap, 1));
            scope.handle(match Convert::to_string(heap, &scope, v).map(|v| v.raw()) {
                Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
                Err(err) => return raise_runtime(vm, heap, state, err),
            })
        } else {
            scope.handle(
                vm.interner()
                    .intern_str(heap, &scope, "")
                    .as_tagged(heap)
                    .erase(),
            )
        };
        let map = Errors::map_for(vm, heap, class);

        let obj = heap
            .new_object(&scope, scope.handle(map), HandleSlice::EMPTY)
            .as_handle(&scope);
        let name = scope.handle(vm.known().strings.name.get(heap));
        let message_key = scope.handle(vm.known().strings.message.get(heap));
        let class_value = vm.interner().intern_str(heap, &scope, class);
        let _ = Object::define_own_property(
            heap,
            &scope,
            obj,
            name,
            PropertyDescriptor::data(class_value.erase()),
        );
        let _ = Object::define_own_property(
            heap,
            &scope,
            obj,
            message_key,
            PropertyDescriptor::data(message.erase()),
        );
        obj.as_tagged(heap).erase()
    })
}

pub fn error_to_string<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    state.handle_scope(|scope| {
        let receiver = args.get_handle(heap, 0);

        let name_key = vm.interner().intern_str(heap, &scope, "name");
        let name = match rt_try!(
            vm,
            heap,
            state,
            Object::get_property(vm, heap, state, receiver, receiver, name_key.erase(), false)
        ) {
            Coercion::Value(v) => scope.handle(v),
            Coercion::Threw => scope.handle(vm.known().exception.get(heap).erase()),
        };
        let message_key = vm.interner().intern_str(heap, &scope, "message");
        let message = match rt_try!(
            vm,
            heap,
            state,
            Object::get_property(
                vm,
                heap,
                state,
                receiver,
                receiver,
                message_key.erase(),
                false
            )
        ) {
            Coercion::Value(v) => scope.handle(v),
            Coercion::Threw => scope.handle(vm.known().exception.get(heap).erase()),
        };

        let a = scope.handle(
            match Convert::to_string(heap, &scope, name).map(|v| v.raw()) {
                Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
                Err(err) => return raise_runtime(vm, heap, state, err),
            },
        );
        let b = scope.handle(
            match Convert::to_string(heap, &scope, message).map(|v| v.raw()) {
                Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
                Err(err) => return raise_runtime(vm, heap, state, err),
            },
        );
        let colon = vm.interner().intern_str(heap, &scope, ": ");
        let ab = DenseString::concat(heap, &scope, a, colon.erase());
        let out = DenseString::concat(heap, &scope, ab.erase(), b);
        out.as_tagged(heap).erase()
    })
}
