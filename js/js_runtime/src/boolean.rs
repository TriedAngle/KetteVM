use vm_core::{
    Args, Convert, Handle, HostCtx, Object, Tagged, Value, VmError, WrapperKind, extend_lifetime,
    raise, rt_try,
};

crate::register_natives! {
    boolean -> boolean_constructor,
    boolean_value_of,
    boolean_to_string,
}

/// `Boolean ( value )`
/// [ES 20.3.1.1](https://tc39.es/ecma262/#sec-boolean-constructor-boolean-value)
/// Converts `value` with ToBoolean; under `new` it wraps that Boolean.
fn boolean_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(new_target) = new_target else {
        let arg = args.get(heap, 1);
        return Convert::boolean(heap, arg.is_truthy(heap));
    };
    state.handle_scope(|scope| {
        let arg = args.get(heap, 1);
        let value = scope.handle(Convert::boolean(heap, arg.is_truthy(heap)));
        let obj = scope.handle(rt_try!(
            heap,
            Object::create_wrapper_from_constructor(
                vm,
                heap,
                state,
                new_target,
                WrapperKind::Boolean,
                value,
            )
        ));
        obj.as_tagged(heap).erase()
    })
}

/// `Boolean.prototype.valueOf ( )`
/// [ES 20.3.3.3](https://tc39.es/ecma262/#sec-boolean.prototype.valueof)
/// Returns the [[BooleanData]] of the receiver (ThisBooleanValue).
fn boolean_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match args
        .get(heap, 0)
        .wrapper_value(heap, vm.known(), WrapperKind::Boolean)
    {
        Some(v) => extend_lifetime!(heap, v),
        None => raise!(vm, heap, state, VmError::IncompatibleBooleanReceiver),
    }
}

/// `Boolean.prototype.toString ( )`
/// [ES 20.3.3.2](https://tc39.es/ecma262/#sec-boolean.prototype.tostring)
/// Returns "true" or "false" for the receiver's [[BooleanData]].
fn boolean_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(v) = args
        .get(heap, 0)
        .wrapper_value(heap, vm.known(), WrapperKind::Boolean)
    else {
        raise!(vm, heap, state, VmError::IncompatibleBooleanReceiver);
    };
    let value = v == vm.known().true_object.get(heap);
    let known = vm.known();
    if value {
        known.strings.true_.get(heap).erase()
    } else {
        known.strings.false_.get(heap).erase()
    }
}
