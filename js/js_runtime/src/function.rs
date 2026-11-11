//! ES 20.2: the Function constructor and Function.prototype
//! toString/call/apply/bind.

use crate::JSRuntime;
use vm_core::{
    Args, CallInfo, ContextObject, DenseString, Handle, HandleScope, HandleSlice, Heap, HostCtx,
    Object, PropertyDescriptor, Result, Runtime, Smi, Tagged, ThreadState, VM, Value, VmError,
    bail, extend_lifetime, materialize::Materialize, raise, raise_runtime, rt_try,
};

crate::register_natives! {
    function_to_string,
    function_bind,
    function_call,
    function_apply,
    function_constructor,
}

/// `Function ( ...parameterArgs, bodyArg )`
/// [ES 20.2.1.1](https://tc39.es/ecma262/#sec-function-p1-p2-pn-body)
/// Builds and evaluates `function (parameters) { body }`.
fn function_constructor<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let ids = &ctx.runtime::<JSRuntime>().ids;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        // root the caller context before the allocating ToString loop below
        let context = scope.handle(
            state
                .current_context(heap)
                .unwrap_or_else(|| vm.known().empty_context.get(heap).erase()),
        );
        let mut parts: Vec<String> = Vec::with_capacity(args.len());
        for i in 0..args.len() {
            let arg = args.get_handle(heap, i);
            let text =
                if i + 1 == args.len() && arg.as_tagged(heap) == vm.known().undefined.get(heap) {
                    String::new()
                } else {
                    let text = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, arg)));
                    text.as_tagged(heap)
                        .get_as::<DenseString>(heap)
                        .map(|s| s.to_rust_string(heap))
                        .unwrap_or_default()
                };
            parts.push(text);
        }
        let (params, body) = match parts.split_last() {
            Some((body, params)) => (params.join(", "), body.clone()),
            None => (String::new(), String::new()),
        };
        // TODO: the dynamic function is parsed in the caller's context; the
        // spec parses it in the global scope (ES 20.2.1.1 step 8).
        let source = format!("(function ({params}) {{\n{body}\n}})");
        let program = match js_compiler::compile_js(&source, bytecode::SourceMode::Eval, ids) {
            Ok(program) => program,
            Err(_) => {
                raise!(vm, heap, state, VmError::InvalidDeclaration);
            }
        };
        let Some(context) = scope.cast::<ContextObject>(heap, context.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let closure =
            Materialize::closure_vm(vm, heap, state, &scope, &program, JSRuntime::ID, context);
        let result = match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            closure.erase(),
            HandleSlice::EMPTY,
            None,
        ) {
            Ok(result) => scope.handle(result),
            Err(err) => return raise_runtime(heap, err),
        };
        result.as_tagged(heap).erase()
    })
}

/// `Function.prototype.toString ( )`
/// [ES 20.2.3.5](https://tc39.es/ecma262/#sec-function.prototype.tostring)
/// Returns a NativeFunction-syntax source string for a callable receiver.
fn function_to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let function = call.receiver;
    if !function.as_tagged(heap).is_callable(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    // TODO: step 2 ([[SourceText]]) is not modelled, so user functions also
    // get the implementation-defined NativeFunction form.
    state.handle_scope(|scope| {
        let name_key = vm.known().strings.name.get_handle(heap, &scope);
        let name = match Object::own_property_ordinary(
            heap,
            &scope,
            function,
            scope.handle(name_key.as_tagged(heap).erase()),
        )
        .map(|own| own.desc)
        {
            Some(PropertyDescriptor::Data { value, .. }) => value
                .as_tagged(heap)
                .get_as::<DenseString>(heap)
                .map(|s| s.to_rust_string(heap)),
            _ => None,
        };
        let source = match name {
            Some(name) => format!("function {name}() {{ [native code] }}"),
            None => "function () { [native code] }".to_string(),
        };
        DenseString::from_utf8(heap, &scope, &source)
            .as_tagged(heap)
            .erase()
    })
}

/// `Function.prototype.call ( thisArg, ...args )`
/// [ES 20.2.3.3](https://tc39.es/ecma262/#sec-function.prototype.call)
/// Invokes the callable receiver with `thisArg` and the forwarded arguments.
fn function_call<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let function = call.receiver;
    if !function.as_tagged(heap).is_callable(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    // the arguments are already exactly (thisArg, ...args)
    let forwarded = state.stack().slice(args);
    match HostCtx::enter(
        vm,
        state.runtime(vm, heap),
        heap,
        state,
        function,
        forwarded,
        None,
    ) {
        Ok(result) => extend_lifetime!(heap, result),
        Err(err) => raise_runtime(heap, err),
    }
}

/// `Function.prototype.apply ( thisArg, argArray )`
/// [ES 20.2.3.1](https://tc39.es/ecma262/#sec-function.prototype.apply)
/// Invokes the callable receiver with `thisArg` and the elements of `argArray`.
fn function_apply<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let function = call.receiver;
    if !function.as_tagged(heap).is_callable(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    let this_arg = args.get_handle(heap, 0);
    let arg_array = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let elements = if arg_array.as_tagged(heap).is_nullish(heap) {
            Vec::new()
        } else {
            rt_try!(
                heap,
                create_list_from_array_like(vm, heap, state, &scope, arg_array)
            )
        };
        let mut call_args = Vec::with_capacity(elements.len() + 1);
        call_args.push(this_arg.as_tagged(heap));
        call_args.extend(elements.iter().map(|element| element.as_tagged(heap)));
        let call_args = scope.stage(&call_args);
        let result = match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            function,
            call_args,
            None,
        ) {
            Ok(result) => scope.handle(result),
            Err(err) => return raise_runtime(heap, err),
        };
        result.as_tagged(heap).erase()
    })
}

/// `CreateListFromArrayLike ( obj )`
/// [ES 7.3.19](https://tc39.es/ecma262/#sec-createlistfromarraylike)
/// Reads indices `0 .. LengthOfArrayLike(obj)`.
fn create_list_from_array_like<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    object: Handle<'_, Value>,
) -> Result<Vec<Handle<'s, Value>>> {
    if object.as_tagged(heap).is_primitive(heap) {
        bail!(vm, heap, state, VmError::CreateListFromArrayLikeNonObject);
    }
    let length = Object::length_of_array_like(vm, heap, state, scope, object)?;
    let mut elements = Vec::with_capacity(length as usize);
    for index in 0..length {
        let index = scope.handle(Smi::new(index as i64).into_tagged());
        let key = scope.handle(Object::to_property_key(vm, heap, state, index)?);
        let element = scope.handle(Object::get_property(
            vm,
            heap,
            state,
            object,
            object,
            key.erase(),
            false,
        )?);
        elements.push(element);
    }
    Ok(elements)
}

/// `Function.prototype.bind ( thisArg, ...args )`
/// [ES 20.2.3.2](https://tc39.es/ecma262/#sec-function.prototype.bind)
/// Returns a bound function with `thisArg` and the leading arguments fixed.
fn function_bind<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = call.receiver;
    if !target.as_tagged(heap).is_callable(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    let this_arg = args.get_handle(heap, 0);
    let prepend: Vec<Handle<'_, Value>> = (1..args.len())
        .map(|index| args.get_handle(heap, index))
        .collect();
    let bound_count = prepend.len();
    state.handle_scope(|scope| {
        let Some(target_object) = target.as_tagged(heap).as_heap_object() else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let constructable = target_object.map(heap).kind().is_constructor();
        // BoundFunctionCreate: the [[Prototype]] is the target's
        let prototype = scope.handle(target_object.as_ref().prototype(heap));
        let staged = scope.stage(
            &prepend
                .iter()
                .map(|value| value.as_tagged(heap))
                .collect::<Vec<_>>(),
        );
        let bound_args = scope.handle(heap.new_array(&scope, staged));
        let map = scope.handle(if constructable {
            vm.known().bound_constructor_map.get(heap)
        } else {
            vm.known().bound_function_map.get(heap)
        });
        let slots = scope.stage(&[
            target.as_tagged(heap),
            this_arg.as_tagged(heap),
            bound_args.as_tagged(heap).erase(),
        ]);
        let bound = scope.handle(heap.new_object(&scope, map, slots));
        if let Err(err) = Object::set_prototype(vm, heap, state, &scope, bound, prototype) {
            return raise_runtime(heap, err);
        }
        copy_bound_name_and_length(vm, heap, state, &scope, bound.erase(), target, bound_count)
    })
}

/// `Function.prototype.bind ( thisArg, ...args )` steps 4-10
/// [ES 20.2.3.2](https://tc39.es/ecma262/#sec-function.prototype.bind)
/// Copies `target`'s `length` (minus the bound argument count) and its
/// `"bound " + name` onto `bound`, the freshly created bound function.
/// Returns `bound`, or the exception sentinel when a lookup threw.
fn copy_bound_name_and_length<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    bound: Handle<'_, Value>,
    target: Handle<'_, Value>,
    bound_count: usize,
) -> Tagged<'a, Value> {
    let Some(bound_object) = scope.cast::<Object>(heap, bound.as_tagged(heap)) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let length_key = vm.known().strings.length.get_handle(heap, scope);
    let mut length = 0.0;
    // TODO: HasOwnProperty must run the getOwnPropertyDescriptor trap when the
    // target is a proxy; this reads its ordinary own descriptor instead.
    let has_length = Object::own_property_ordinary(
        heap,
        scope,
        target,
        scope.handle(length_key.as_tagged(heap).erase()),
    )
    .map(|own| own.desc)
    .is_some();
    if has_length {
        let target_length = match Object::get_property(
            vm,
            heap,
            state,
            target,
            target,
            length_key.erase(),
            false,
        ) {
            Ok(value) => scope.handle(value),
            Err(err) => return raise_runtime(heap, err),
        };
        if let Some(number) = target_length.as_tagged(heap).as_number(heap) {
            // ToIntegerOrInfinity, then the bound arguments are subtracted
            length = if number.is_nan() { 0.0 } else { number.trunc() };
            if length != f64::INFINITY {
                length = (length - bound_count as f64).max(0.0);
            }
        }
    }
    let length_value = scope.handle(heap.new_number(length));
    if let Err(err) = Object::define_own_property_ordinary(
        heap,
        scope,
        bound_object,
        length_key,
        PropertyDescriptor::non_enumerable(length_value.erase()),
    ) {
        return raise_runtime(heap, err);
    }
    let name_key = vm.known().strings.name.get_handle(heap, scope);
    let target_name =
        match Object::get_property(vm, heap, state, target, target, name_key.erase(), false) {
            Ok(value) => scope.handle(value),
            Err(err) => return raise_runtime(heap, err),
        };
    // SetFunctionName(F, targetName, "bound"); a non-string name adds nothing
    let name = match target_name.as_tagged(heap).get_as::<DenseString>(heap) {
        Some(name) => format!("bound {}", name.to_rust_string(heap)),
        None => "bound ".to_string(),
    };
    let name = DenseString::from_utf8(heap, scope, &name);
    if let Err(err) = Object::define_own_property_ordinary(
        heap,
        scope,
        bound_object,
        name_key,
        PropertyDescriptor::non_enumerable(name.erase()),
    ) {
        return raise_runtime(heap, err);
    }
    bound.as_tagged(heap).erase()
}
