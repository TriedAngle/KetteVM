use vm_core::HostCtx;
use vm_core::Object;
use vm_core::raise_runtime;
use vm_core::{Args, DenseString, Handle, Symbol, Tagged, Value, WrapperKind};

/// `String ( value )`
/// [ES 22.1.1.1](https://tc39.es/ecma262/#sec-string-constructor-string-value)
/// Converts `value` to a string (describing Symbols); under `new` it wraps it.
pub fn string_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let arg = scope.handle(args.get(heap, 1));
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

/// `String.prototype.valueOf ( )`
/// [ES 22.1.3.35](https://tc39.es/ecma262/#sec-string.prototype.valueof)
/// Returns the [[StringData]] of the receiver.
pub fn string_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let v = match args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::String)
        {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        v.as_tagged(heap).erase()
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
