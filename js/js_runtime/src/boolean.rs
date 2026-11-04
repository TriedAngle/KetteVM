use vm_core::HostCtx;
use vm_core::Object;
use vm_core::raise_runtime;
use vm_core::{Args, Convert, Handle, Tagged, Value, WrapperKind};

/// `Boolean ( value )`
/// [ES 20.3.1.1](https://tc39.es/ecma262/#sec-boolean-constructor-boolean-value)
/// Converts `value` with ToBoolean; under `new` it wraps that Boolean.
pub fn boolean_constructor<'a>(
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
        let obj = match Object::create_wrapper_from_constructor(
            vm,
            heap,
            state,
            new_target,
            WrapperKind::Boolean,
            value,
        ) {
            Ok(Some(obj)) => scope.handle(obj),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        obj.as_tagged(heap).erase()
    })
}

/// `Boolean.prototype.valueOf ( )`
/// [ES 20.3.3.3](https://tc39.es/ecma262/#sec-boolean.prototype.valueof)
/// Returns the [[BooleanData]] of the receiver (ThisBooleanValue).
pub fn boolean_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let v = match args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::Boolean)
        {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        v.as_tagged(heap).erase()
    })
}

/// `Boolean.prototype.toString ( )`
/// [ES 20.3.3.2](https://tc39.es/ecma262/#sec-boolean.prototype.tostring)
/// Returns "true" or "false" for the receiver's [[BooleanData]].
pub fn boolean_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = match args
        .get(heap, 0)
        .wrapper_value(heap, vm.known(), WrapperKind::Boolean)
    {
        Ok(v) => v == vm.known().true_object.get(heap),
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    let known = vm.known();
    if value {
        known.strings.true_.get(heap).erase()
    } else {
        known.strings.false_.get(heap).erase()
    }
}
