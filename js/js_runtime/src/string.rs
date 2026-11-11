use crate::symbol::descriptive_string;
use vm_core::{
    Args, CallInfo, HostCtx, Object, Symbol, Tagged, Value, VmError, WrapperKind, extend_lifetime,
    raise, rt_try,
};

crate::register_natives! {
    string -> string_constructor,
    string_value_of,
    string_to_string,
}

/// `String ( value )`
/// [ES 22.1.1.1](https://tc39.es/ecma262/#sec-string-constructor-string-value)
/// Converts `value` to a string (describing Symbols); under `new` it wraps it.
fn string_constructor<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // ES 22.1.1.1: a missing argument is the empty string, not "undefined".
        let arg = if !args.is_empty() {
            args.get_handle(heap, 0)
        } else {
            heap.known().strings.empty.get_handle(heap, &scope).erase()
        };
        if call.new_target.is_none()
            && let Some(sym) = arg.as_tagged(heap).get_as::<Symbol>(heap)
        {
            let sym = scope.handle(sym);
            return descriptive_string(heap, &scope, sym)
                .as_tagged(heap)
                .erase();
        }
        let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, arg)));
        let Some(new_target) = call.new_target else {
            return s.as_tagged(heap).erase();
        };
        let obj = scope.handle(rt_try!(
            heap,
            Object::create_wrapper_from_constructor(
                vm,
                heap,
                state,
                new_target,
                WrapperKind::String,
                s.erase(),
            )
        ));
        obj.as_tagged(heap).erase()
    })
}

/// `String.prototype.valueOf ( )`
/// [ES 22.1.3.35](https://tc39.es/ecma262/#sec-string.prototype.valueof)
/// Returns the [[StringData]] of the receiver.
fn string_value_of<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match call
        .receiver
        .as_tagged(heap)
        .wrapper_value(heap, vm.known(), WrapperKind::String)
    {
        Some(v) => extend_lifetime!(heap, v),
        None => raise!(vm, heap, state, VmError::IncompatibleStringReceiver),
    }
}

/// `String.prototype.toString ( )`
/// [ES 22.1.3.29](https://tc39.es/ecma262/#sec-string.prototype.tostring)
/// Returns the [[StringData]] of the receiver.
fn string_to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    string_value_of(ctx, call, args)
}
