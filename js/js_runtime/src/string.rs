//! ES 22.1: the String constructor and prototype methods.

use vm_core::HostCtx;
use vm_core::Object;
use vm_core::raise_runtime;
use vm_core::{Args, DenseString, Handle, Symbol, Tagged, Value, WrapperKind};

/// `String ( value )`
/// [ES 22.1.1.1](https://tc39.es/ecma262/#sec-string-constructor-string-value)
/// Returns the string for `value`; under `new` it wraps that String.
pub fn string_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let arg = scope.handle(args.get(heap, 1));
        // `String(sym)` is SymbolDescriptiveString (ES 22.1.1.1 step 2.a)
        if new_target.is_none()
            && let Some(sym) = arg.as_tagged(heap).get_as::<Symbol>(heap)
        {
            let mut text = String::from("Symbol(");
            text.push_str(&String::from_utf8_lossy(sym.as_ref().description(heap)));
            text.push(')');
            let s = DenseString::from_utf8(heap, &scope, &text);
            return s.as_tagged(heap).erase();
        }
        let s = match Object::to_string(vm, heap, state, arg) {
            Ok(Some(s)) => scope.handle(s),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let Some(new_target) = new_target else {
            return s.as_tagged(heap).erase();
        };
        let obj = match Object::create_wrapper_from_constructor(
            vm,
            heap,
            state,
            new_target,
            WrapperKind::String,
            s.erase(),
        ) {
            Ok(Some(obj)) => scope.handle(obj),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        obj.as_tagged(heap).erase()
    })
}

pub fn string_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let arg = args.get(heap, 0);
    match unsafe { Tagged::<Value>::from_value_unchecked(arg.raw()) }
        .wrapper_value(heap, vm.known(), WrapperKind::String)
        .map(|v| v.raw())
    {
        Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
        Err(err) => return raise_runtime(vm, heap, state, err),
    }
}

pub fn string_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let arg = args.get(heap, 0);
    match unsafe { Tagged::<Value>::from_value_unchecked(arg.raw()) }
        .wrapper_value(heap, vm.known(), WrapperKind::String)
        .map(|v| v.raw())
    {
        Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
        Err(err) => return raise_runtime(vm, heap, state, err),
    }
}
