use vm_core::HostCtx;
use vm_core::Object;
use vm_core::VmError;
use vm_core::raise;
use vm_core::rt_try;
use vm_core::{Args, Handle, Symbol, Tagged, Value, WrapperKind};

use crate::symbol::descriptive_string;

/// `String ( value )`
/// [ES 22.1.1.1](https://tc39.es/ecma262/#sec-string-constructor-string-value)
/// Converts `value` to a string (describing Symbols); under `new` it wraps it.
pub fn string_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let arg = scope.handle(args.get(heap, 1));
        if new_target.is_none()
            && let Some(sym) = arg.as_tagged(heap).get_as::<Symbol>(heap)
        {
            let sym = scope.handle(sym);
            return descriptive_string(heap, &scope, sym)
                .as_tagged(heap)
                .erase();
        }
        let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, arg)));
        let Some(new_target) = new_target else {
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
pub fn string_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(v) = args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::String)
        else {
            raise!(vm, heap, state, VmError::IncompatibleStringReceiver);
        };
        scope.handle(v).as_tagged(heap).erase()
    })
}

/// `String.prototype.toString ( )`
/// [ES 22.1.3.29](https://tc39.es/ecma262/#sec-string.prototype.tostring)
/// Returns the [[StringData]] of the receiver.
pub fn string_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    string_value_of(ctx, None, args)
}
