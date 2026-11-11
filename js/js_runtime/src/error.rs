use vm_core::{
    Args, Convert, DenseString, Errors, Handle, HandleScope, Heap, HostCtx, Map, Object, Tagged,
    ThreadState, VM, Value, VmError, raise, rt_try,
};

crate::register_natives! {
    reference_error -> reference_error_constructor,
    range_error -> range_error_constructor,
    syntax_error -> syntax_error_constructor,
    eval_error -> eval_error_constructor,
    uri_error -> uri_error_constructor,
    error -> error_constructor,
    type_error -> type_error_constructor,
    error_is_error,
    error_to_string,
}

/// `Error ( message [ , options ] )`
/// [ES 20.5.1.1](https://tc39.es/ecma262/#sec-error-constructor)
/// Creates an Error object with non-enumerable `message` and `cause`.
fn error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `EvalError ( message [ , options ] )`
/// [ES 20.5.5.1](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-evalerror)
fn eval_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().eval_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `RangeError ( message [ , options ] )`
/// [ES 20.5.5.2](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-rangeerror)
fn range_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().range_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `ReferenceError ( message [ , options ] )`
/// [ES 20.5.5.3](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-referenceerror)
fn reference_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().reference_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `SyntaxError ( message [ , options ] )`
/// [ES 20.5.5.4](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-syntaxerror)
fn syntax_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().syntax_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `TypeError ( message [ , options ] )`
/// [ES 20.5.5.5](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-typeerror)
fn type_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().type_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `URIError ( message [ , options ] )`
/// [ES 20.5.5.6](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-urierror)
fn uri_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let map = vm.known().uri_error_map.get_handle(heap, &scope);
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// The shared code of 20.5.1.1 and 20.5.6.1.1; `map` selects the class.
fn make_error<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    new_target: Option<Handle<'_, Value>>,
    map: Handle<'_, Map>,
    args: Args,
) -> Tagged<'a, Value> {
    let message = args.get_handle(heap, 1);
    let options = args.get_handle(heap, 2);
    let error = scope.handle(rt_try!(
        heap,
        Errors::create(vm, heap, state, new_target, map, message, options)
    ));
    error.as_tagged(heap).erase()
}

/// `Error.isError ( arg )`
/// [ES 20.5.2.1](https://tc39.es/ecma262/#sec-error.iserror)
/// True when `arg` is an Object with an [[ErrorData]] slot.
fn error_is_error<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let arg = args.get(heap, 1);
    let is_error = arg
        .as_heap_object()
        .is_some_and(|object| object.map(heap).kind().is_error());
    Convert::boolean(heap, is_error)
}

/// `Error.prototype.toString ( )`
/// [ES 20.5.3.4](https://tc39.es/ecma262/#sec-error.prototype.tostring)
/// Returns `"name: message"`, omitting either part when it is empty.
fn error_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        if receiver.as_tagged(heap).is_primitive(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let name_key = vm.known().strings.name.get_handle(heap, &scope).erase();
        let name_default = vm.interner().intern_str(heap, &scope, "Error").erase();
        let name = rt_try!(
            heap,
            string_property(vm, heap, state, &scope, receiver, name_key, name_default)
        );
        let message_key = vm.known().strings.message.get_handle(heap, &scope).erase();
        let message_default = vm.known().strings.empty.get_handle(heap, &scope).erase();
        let message = rt_try!(
            heap,
            string_property(
                vm,
                heap,
                state,
                &scope,
                receiver,
                message_key,
                message_default,
            )
        );
        if name.as_tagged(heap).is_empty_string(heap) {
            return message.as_tagged(heap).erase();
        }
        if message.as_tagged(heap).is_empty_string(heap) {
            return name.as_tagged(heap).erase();
        }
        let separator = vm.interner().intern_str(heap, &scope, ": ").erase();
        let head = DenseString::concat(heap, &scope, name, separator);
        DenseString::concat(heap, &scope, head.erase(), message)
            .as_tagged(heap)
            .erase()
    })
}

/// `Get ( receiver, key )`, `undefined` replaced by `default`, then `ToString`.
fn string_property<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    receiver: Handle<'_, Value>,
    key: Handle<'_, Value>,
    default: Handle<'s, Value>,
) -> vm_core::Result<Handle<'s, Value>> {
    let value = scope.handle(Object::get_property(
        vm, heap, state, receiver, receiver, key, false,
    )?);
    if value.as_tagged(heap) == vm.known().undefined.get(heap) {
        return Ok(default);
    }
    Ok(scope.handle(Object::to_string(vm, heap, state, value)?))
}
