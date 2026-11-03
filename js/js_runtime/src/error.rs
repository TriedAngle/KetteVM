use vm_core::Coercion;
use vm_core::Errors;
use vm_core::HostCtx;
use vm_core::{
    Args, Convert, DenseString, Handle, HandleScope, Heap, Map, Object, Tagged, ThreadState, Value,
    VmError,
};
use vm_core::{VM, raise_runtime};

/// `Error ( message [ , options ] )`
/// [ES 20.5.1.1](https://tc39.es/ecma262/#sec-error-constructor)
/// Creates an Error object with non-enumerable `message` and `cause`.
pub fn error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `EvalError ( message [ , options ] )`
/// [ES 20.5.5.1](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-evalerror)
pub fn eval_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().eval_error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `RangeError ( message [ , options ] )`
/// [ES 20.5.5.2](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-rangeerror)
pub fn range_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().range_error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `ReferenceError ( message [ , options ] )`
/// [ES 20.5.5.3](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-referenceerror)
pub fn reference_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().reference_error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `SyntaxError ( message [ , options ] )`
/// [ES 20.5.5.4](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-syntaxerror)
pub fn syntax_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().syntax_error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `TypeError ( message [ , options ] )`
/// [ES 20.5.5.5](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-typeerror)
pub fn type_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().type_error_map.get(heap));
        make_error(vm, heap, state, &scope, new_target, map, args)
    })
}

/// `URIError ( message [ , options ] )`
/// [ES 20.5.5.6](https://tc39.es/ecma262/#sec-native-error-types-used-in-this-standard-urierror)
pub fn uri_error_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let map = scope.handle(vm.known().uri_error_map.get(heap));
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
    let error = match Errors::create(vm, heap, state, new_target, map, message, options) {
        Ok(Some(error)) => scope.handle(error),
        Ok(None) => return vm.known().exception.get(heap).erase(),
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    error.as_tagged(heap).erase()
}

/// `Error.isError ( arg )`
/// [ES 20.5.2.1](https://tc39.es/ecma262/#sec-error.iserror)
/// True when `arg` is an Object with an [[ErrorData]] slot.
pub fn error_is_error<'a>(
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
pub fn error_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(args.get(heap, 0));
        if receiver.as_tagged(heap).is_primitive(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let name_key = scope.handle(vm.known().strings.name.get(heap).erase());
        let name_default = vm.interner().intern_str(heap, &scope, "Error").erase();
        let name = match string_property(vm, heap, state, &scope, receiver, name_key, name_default)
        {
            Ok(Some(name)) => name,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let message_key = scope.handle(vm.known().strings.message.get(heap).erase());
        let message_default = scope.handle(vm.known().strings.empty.get(heap).erase());
        let message = match string_property(
            vm,
            heap,
            state,
            &scope,
            receiver,
            message_key,
            message_default,
        ) {
            Ok(Some(message)) => message,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
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
/// `Ok(None)` means the lookup threw and the exception is pending.
fn string_property<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    receiver: Handle<'_, Value>,
    key: Handle<'_, Value>,
    default: Handle<'s, Value>,
) -> Result<Option<Handle<'s, Value>>, VmError> {
    let value = match Object::get_property(vm, heap, state, receiver, receiver, key, false)? {
        Coercion::Threw => return Ok(None),
        Coercion::Value(value) => scope.handle(value),
    };
    if value.as_tagged(heap) == vm.known().undefined.get(heap) {
        return Ok(Some(default));
    }
    Ok(match Object::to_string(vm, heap, state, value)? {
        Some(string) => Some(scope.handle(string)),
        None => None,
    })
}
