//! ES 20.4: Symbol objects.
use vm_core::{
    Args, CallInfo, DenseString, Errors, FixedArray, Handle, HandleScope, Heap, HostCtx, MapKind,
    Object, Symbol, Tagged, Value, VmError, raise, rt_try,
};

crate::register_natives! {
    symbol -> symbol_constructor,
    symbol_description -> symbol_prototype_description,
    symbol_to_string -> symbol_prototype_to_string,
    symbol_value_of -> symbol_prototype_value_of,
    symbol_to_primitive -> symbol_prototype_to_primitive,
    symbol_for,
    symbol_key_for,
}

/// `Symbol ( [ description ] )`
/// [ES 20.4.1.1](https://tc39.es/ecma262/#sec-symbol-constructor)
/// Creates a new Symbol whose `[[Description]]` is `description` (or
/// `undefined`); `new Symbol()` is a TypeError.
fn symbol_constructor<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if call.new_target.is_some() {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    state.handle_scope(|scope| {
        let description = scope.handle(args.get(heap, 0));
        if description.as_tagged(heap) == vm.known().undefined.get(heap).erase() {
            return Symbol::new(heap, &scope, None).as_tagged(heap).erase();
        }
        let description = scope.handle(rt_try!(
            heap,
            Object::to_string(vm, heap, state, description)
        ));
        let description = scope
            .cast::<DenseString>(heap, description.as_tagged(heap))
            .expect("ToString returns a String");
        Symbol::new(heap, &scope, Some(description))
            .as_tagged(heap)
            .erase()
    })
}

/// `get Symbol.prototype.description`
/// [ES 20.4.3.2](https://tc39.es/ecma262/#sec-symbol.prototype.description)
/// Returns the `[[Description]]` of the receiver.
fn symbol_prototype_description<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        match this_symbol_value(heap, &scope, call.receiver.as_tagged(heap)) {
            Some(symbol) => symbol.as_tagged(heap).as_ref().description(heap),
            None => {
                raise!(vm, heap, state, VmError::IncompatibleSymbolReceiver);
            }
        }
    })
}

/// `Symbol.prototype.toString ( )`
/// [ES 20.4.3.3](https://tc39.es/ecma262/#sec-symbol.prototype.tostring)
/// Returns `"Symbol(" + description + ")"`.
fn symbol_prototype_to_string<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(call.receiver.as_tagged(heap));
        let Some(symbol) = this_symbol_value(heap, &scope, receiver.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::IncompatibleSymbolReceiver);
        };
        descriptive_string(heap, &scope, symbol)
            .as_tagged(heap)
            .erase()
    })
}

/// `Symbol.prototype.valueOf ( )`
/// [ES 20.4.3.4](https://tc39.es/ecma262/#sec-symbol.prototype.valueof)
/// Returns the receiver's `[[SymbolData]]`.
fn symbol_prototype_value_of<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        match this_symbol_value(heap, &scope, call.receiver.as_tagged(heap)) {
            Some(symbol) => symbol.as_tagged(heap).erase(),
            None => Errors::raise(vm, heap, state, VmError::IncompatibleSymbolReceiver),
        }
    })
}

/// `Symbol.prototype [ %Symbol.toPrimitive% ] ( hint )`
/// [ES 20.4.3.5](https://tc39.es/ecma262/#sec-symbol.prototype-@@toprimitive)
/// Returns the receiver's symbol; `hint` is ignored.
fn symbol_prototype_to_primitive<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    symbol_prototype_value_of(ctx, call, args)
}

/// `Symbol.for ( key )`
/// [ES 20.4.2.4](https://tc39.es/ecma262/#sec-symbol.for)
/// Returns the registered symbol for `key`, registering a new one on first use.
fn symbol_for<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let key = scope.handle(args.get(heap, 0));
        let key = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, key)));
        let key = scope
            .cast::<DenseString>(heap, key.as_tagged(heap))
            .expect("ToString returns a String");
        let table = vm.known().global_symbol_registry.get(heap);
        for index in (0..table.len()).step_by(2) {
            let Some(registered) = table.as_ref().at(heap, index).get_as::<DenseString>(heap)
            else {
                continue;
            };
            if registered.as_ref().content_eq(heap, key.as_tagged(heap)) {
                return table.as_ref().at(heap, index + 1);
            }
        }
        let symbol = Symbol::new(heap, &scope, Some(key));
        register_symbol(heap, &scope, key, symbol);
        symbol.as_tagged(heap).erase()
    })
}

/// `Symbol.keyFor ( symbol )`
/// [ES 20.4.2.8](https://tc39.es/ecma262/#sec-symbol.keyfor)
/// Returns the registry key of `symbol`, or `undefined` when it was not
/// created by `Symbol.for`.
fn symbol_key_for<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let symbol = scope.handle(args.get(heap, 0));
        let Some(symbol) = symbol.as_tagged(heap).get_as::<Symbol>(heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let table = vm.known().global_symbol_registry.get(heap);
        for index in (0..table.as_ref().len()).step_by(2) {
            if table.as_ref().at(heap, index + 1) == symbol.erase() {
                return table.as_ref().at(heap, index);
            }
        }
        vm.known().undefined.get(heap).erase()
    })
}

/// `SymbolDescriptiveString ( symbol )`
/// [ES 20.4.3.3.1](https://tc39.es/ecma262/#sec-symboldescriptivestring)
/// `"Symbol(" + (description or "") + ")"`.
pub fn descriptive_string<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    symbol: Handle<'_, Symbol>,
) -> Handle<'s, DenseString> {
    let description = symbol.as_tagged(heap).as_ref().description(heap);
    let description = if description == heap.known().undefined.get(heap).erase() {
        heap.known().strings.empty.get(heap).erase()
    } else {
        description
    };
    let text = description
        .get_as::<DenseString>(heap)
        .expect("[[Description]] is a String");
    let mut units: Vec<u16> = Vec::with_capacity(text.as_ref().len() + 8);
    units.extend("Symbol(".encode_utf16());
    text.as_ref().data(heap).write_units(&mut units);
    units.push(')' as u16);
    DenseString::from_units(heap, scope, &units)
}

/// `ThisSymbolValue ( arg )`
/// [ES 20.4.3.4.1](https://tc39.es/ecma262/#sec-thissymbolvalue)
/// The primitive itself, or the `[[SymbolData]]` of a Symbol wrapper.
fn this_symbol_value<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    value: Tagged<'_, Value>,
) -> Option<Handle<'s, Symbol>> {
    if let Some(symbol) = value.get_as::<Symbol>(heap) {
        return Some(scope.handle(symbol));
    }
    let object = value.as_heap_object()?;
    if !object.map(heap).kind().contains(MapKind::PRIMITIVE_WRAPPER) {
        return None;
    }
    let symbol = object.slots(heap).at(heap, 0).get_as::<Symbol>(heap)?;
    Some(scope.handle(symbol))
}

/// Appends `[key, symbol]` to the `[[GlobalSymbolRegistry]]`.
fn register_symbol(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    key: Handle<'_, DenseString>,
    symbol: Handle<'_, Symbol>,
) {
    let length = heap.known().global_symbol_registry.get(heap).as_ref().len();
    let table: Handle<'_, FixedArray> = heap.allocate_hole_array(length + 2).as_handle(scope);
    // re-read the registry: the allocation above may have moved it
    let registry = heap.known().global_symbol_registry.get(heap);
    for index in 0..length {
        table
            .as_tagged(heap)
            .as_ref()
            .set(heap, index, registry.as_ref().at(heap, index));
    }
    table
        .as_tagged(heap)
        .as_ref()
        .set(heap, length, key.as_tagged(heap).erase());
    table
        .as_tagged(heap)
        .as_ref()
        .set(heap, length + 1, symbol.as_tagged(heap).erase());
    // a root slot is scanned by every collection, so no remembered-set
    // entry is needed for the young table
    heap.known()
        .global_symbol_registry
        .init(table.as_tagged(heap));
}
