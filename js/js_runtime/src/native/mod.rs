//! The natives `CallNative` dispatches to: compiled-language semantics (each
//! cites its ES section), not JS-visible library functions. [`register`]
//! assigns their ids in the VM's native registry.

use vm_core::Runtime;
mod for_in;
mod iterators;

use vm_core::{
    AccessorPair, Args, Coercion, ContextObject, Convert, DenseString, FixedArray, Handle,
    HandleSlice, Heap, Key, LanguageMode, Object, ObjectSlotsInit, PropertyDescriptor, SlotName,
    Smi, StoreSemantics, StringData, Symbol, Tagged, Value, VmError, info_is_immutable,
    info_is_lexical,
};

use crate::JSRuntime;
use for_in::{for_in_enumerate, for_in_next};
use iterators::{get_iterator, iterator_done, iterator_next, iterator_value};
use js_compiler::native::NativeIds;
use vm_core::HandleScope;
use vm_core::PartialDescriptor;
use vm_core::ThreadState;
use vm_core::intrinsics;
use vm_core::lookup::own_property_enumerable;
use vm_core::materialize::Materialize;
use vm_core::proxy::Flow;
use vm_core::proxy::Proxy;
use vm_core::{HostCtx, VM, frame_context_value, raise_runtime, rt_try};

/// Register every helper and collect the ids the compiler emits.
pub fn register(vm: &mut VM) -> NativeIds {
    NativeIds {
        get_iterator: vm.register_native(get_iterator),
        iterator_next: vm.register_native(iterator_next),
        iterator_done: vm.register_native(iterator_done),
        iterator_value: vm.register_native(iterator_value),
        has_property: vm.register_native(has_property),
        copy_data_properties: vm.register_native(copy_data_properties),
        create_private_name: vm.register_native(create_private_name),
        private_get: vm.register_native(private_get),
        private_set: vm.register_native(private_set),
        private_in: vm.register_native(private_in),
        set_class_fields: vm.register_native(set_class_fields),
        init_instance_fields: vm.register_native(init_instance_fields),
        require_object_coercible: vm.register_native(require_object_coercible),
        delete_property_operator: vm.register_native(delete_property_operator),
        delete_identifier_sloppy: vm.register_native(delete_identifier_sloppy),
        delete_super_property: vm.register_native(delete_super_property),
        for_in_enumerate: vm.register_native(for_in_enumerate),
        for_in_next: vm.register_native(for_in_next),
        set_function_name: vm.register_native(set_function_name),
        install_accessor: vm.register_native(install_accessor),
        define_own_property: vm.register_native(define_own_property),
        set_prototype: vm.register_native(set_prototype),
        throw_if_not_constructor_or_null: vm.register_native(throw_if_not_constructor_or_null),
        throw_if_not_object_or_null: vm.register_native(throw_if_not_object_or_null),
        throw_super_not_called_if_hole: vm.register_native(throw_super_not_called_if_hole),
        throw_super_already_called_if_not_hole: vm
            .register_native(throw_super_already_called_if_not_hole),
        construct_super: vm.register_native(construct_super),
        construct_super_all_args: vm.register_native(construct_super_all_args),
        construct_super_via: vm.register_native(construct_super_via),
        load_dynamic_name: vm.register_native(load_dynamic_name),
        store_dynamic_name: vm.register_native(store_dynamic_name),
        declare_eval_var: vm.register_native(declare_eval_var),
        declare_eval_function: vm.register_native(declare_eval_function),
        resolve_direct_eval: vm.register_native(resolve_direct_eval),
        store_repl_global_init: vm.register_native(store_repl_global_init),
        validate_eval_declarations: vm.register_native(validate_eval_declarations),
        declare_globals: vm.register_native(intrinsics::declare_globals),
        throw_const_assign: vm.register_native(throw_const_assign_error),
        create_rest_parameter: vm.register_native(create_rest_parameter),
        super_get_property: vm.register_native(super_get_property),
        super_set_property: vm.register_native(super_set_property),
    }
}

// ---- runtime helpers ------------------------------------------------------

/// RequireObjectCoercible ( arg )
/// [ES 7.2.1](https://tc39.es/ecma262/#sec-requireobjectcoercible)
/// Throws on null/undefined; returns the argument for the destructuring caller.
fn require_object_coercible<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let nullish = args.get(heap, 0).is_nullish(heap);
    if nullish {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    args.get(heap, 0)
}

// ---- delete (ES 13.5.1) ----------------------------------------------------

/// `delete obj.key`
/// [ES 13.5.1.2](https://tc39.es/ecma262/#sec-delete-operator-runtime-semantics-evaluation)
/// (target, key) -> bool; [[Delete]] on the base object.
fn delete_property_operator<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    // TODO: getting strict this way is really cumbersome this should be made easier.
    let strict = state
        .stack()
        .frame_header(state.frame_base())
        .callable(heap)
        .callable_info(heap)
        .expect("a bytecode frame's callable must have callable info")
        .mode()
        .is_strict();
    state.handle_scope(|scope| {
        // ToObject(ref.[[Base]]) runs before the key's ToPropertyKey, so a
        // nullish base throws without coercing a computed key
        if target.as_tagged(heap).is_nullish(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }

        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        let key = scope.handle(key);
        // proxies run their `deleteProperty` trap (ES 20.2.5.4); the
        // returned boolean flows through the strict handling below
        let ok = match Object::delete_property(vm, heap, state, target, key.erase()) {
            Ok(Flow::Value(ok)) => ok,
            Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        if strict && !ok {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        Convert::boolean(heap, ok)
    })
}

/// `delete x` for a dynamic name in sloppy code
/// [ES 9.1.1.4.7](https://tc39.es/ecma262/#sec-global-environment-records-deletebinding)
/// `DeleteBinding` for the frame's environment chain.
fn delete_identifier_sloppy<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let name = args.get_handle(heap, 0);
    let ok = match state.handle_scope(|scope| {
        match dynamic_lookup_frame(heap, state, &scope, name) {
            // environment-record slots are never deletable
            Ok(Some(DynamicBinding::Slot(_))) => Ok(Flow::Value(false)),
            // an eval-declared binding lives on a context extension object
            Ok(Some(DynamicBinding::Extension { holder, .. })) => {
                Object::delete_property(vm, heap, state, holder, name)
            }
            // unresolved: the global object's own property decides
            Ok(None) => {
                let global = vm.known().global_object.get_handle(heap, &scope).erase();
                Object::delete_property(vm, heap, state, global, name)
            }
            Err(err) => Err(err),
        }
    }) {
        Ok(Flow::Value(ok)) => ok,
        Ok(Flow::Threw) => return vm.known().exception.get(heap).erase(),
        Err(err) => return raise_runtime(vm, heap, state, err),
    };
    Convert::boolean(heap, ok)
}

/// `delete super.property`
/// [ES 13.5.1.2](https://tc39.es/ecma262/#sec-delete-operator-runtime-semantics-evaluation)
/// ReferenceError in both language modes; the key is never coerced.
fn delete_super_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    raise_runtime(vm, heap, state, VmError::Reference)
}

/// `in` operator
/// [ES 13.10.1](https://tc39.es/ecma262/#sec-relational-operators-runtime-semantics-evaluation)
/// (key, obj) -> bool; proxies run their `has` trap.
fn has_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let key = args.get_handle(heap, 0);
    let obj = args.get_handle(heap, 1);
    // a non-Object right operand is a TypeError, thrown before the key is
    // coerced
    if !Proxy::is_js_receiver(heap, obj.as_tagged(heap)) {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        let key = scope.handle(key);
        let has = match rt_try!(
            vm,
            heap,
            state,
            Object::has_property(vm, heap, state, obj, key.erase())
        ) {
            Coercion::Threw => return vm.known().exception.get(heap).erase(),
            Coercion::Value(v) => scope.handle(v),
        };
        has.as_tagged(heap)
    })
}

/// Own property keys of an ordinary object or string exotic in
/// specification order: ascending indices, strings, then symbols.
/// TODO: a proxy source needs its `ownKeys` trap.
fn own_property_keys<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    receiver: Tagged<'_, Value>,
) -> Vec<Handle<'s, Value>> {
    let mut keys: Vec<Handle<'s, Value>> = Vec::new();
    // string exotics expose their code units as own index properties
    if let Some(s) = DenseString::from_receiver(heap, receiver) {
        keys.extend((0..s.len()).map(|i| scope.handle(Smi::new(i as i64))));
    }
    let Some(object) = receiver.as_heap_object() else {
        return keys;
    };
    keys.extend(
        object
            .element_keys(heap)
            .into_iter()
            .map(|i| scope.handle(Smi::new(i as i64))),
    );
    let map = object.map(heap);
    let descriptors = map.descriptors();
    for symbols in [false, true] {
        keys.extend(
            descriptors
                .iter()
                .filter(|d| d.name(heap).erase().get_as::<Symbol>(heap).is_some() == symbols)
                .map(|d| scope.handle(d.name(heap).erase())),
        );
    }
    keys
}

/// CopyDataProperties ( target, source, excludedItems )
/// [ES 7.3.25](https://tc39.es/ecma262/#sec-copydataproperties)
/// Object rest/spread: (excluded..., target, source) -> target.
fn copy_data_properties<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = args.len();
    if n < 2 {
        return raise_runtime(vm, heap, state, VmError::Arity);
    }
    // the register window roots the target, the source and the exclusions
    // across the copy (getters run user code)
    let target = args.get_handle(heap, n - 2);
    let source = args.get_handle(heap, n - 1);
    if source.as_tagged(heap).is_nullish(heap) {
        return target.as_tagged(heap);
    }
    state.handle_scope(|scope| {
        // the compiler passes property keys; the ABI hands them over raw
        let mut excluded: Vec<Handle<'_, SlotName>> = Vec::with_capacity(n - 2);
        for i in 0..n - 2 {
            match rt_try!(
                vm,
                heap,
                state,
                Object::to_property_key(vm, heap, state, args.get_handle(heap, i))
            ) {
                Some(key) => excluded.push(scope.handle(key)),
                None => return vm.known().exception.get(heap).erase(),
            }
        }
        let target = scope
            .cast::<Object>(heap, target.as_tagged(heap))
            .expect("copy target is an object");
        for key in own_property_keys(heap, &scope, source.as_tagged(heap)) {
            let name = scope.handle(key.as_tagged(heap).as_name());
            if excluded
                .iter()
                .any(|e| e.as_tagged(heap).ptr_eq(name.as_tagged(heap)))
            {
                continue;
            }
            // [[GetOwnProperty]] is re-read per key: an earlier getter may
            // have deleted or hidden a later key
            if own_property_enumerable(heap, source.as_tagged(heap), key.as_tagged(heap))
                != Some(true)
            {
                continue;
            }
            let value = match rt_try!(
                vm,
                heap,
                state,
                Object::get_property(vm, heap, state, source, source, key, false)
            ) {
                Coercion::Threw => return vm.known().exception.get(heap).erase(),
                Coercion::Value(v) => scope.handle(v),
            };
            // CreateDataPropertyOrThrow
            if !rt_try!(
                vm,
                heap,
                state,
                Object::define_own_property(
                    heap,
                    &scope,
                    target,
                    name,
                    PropertyDescriptor::data(value)
                )
            ) {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }
        target.as_tagged(heap).erase()
    })
}

/// A fresh private name: (description) -> Symbol.
fn create_private_name<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm: _, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let description = args.get(heap, 1);
        let description = scope.cast::<DenseString>(heap, description);
        Symbol::new(heap, &scope, description)
            .as_tagged(heap)
            .erase()
    })
}

/// PrivateGet (ES 7.3.30): (obj, key) -> value, TypeError when absent.
fn private_get<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let obj = args.get(heap, 0);
    let key = args.get(heap, 1);
    match obj.private_find(heap, key) {
        Some(s) => s.get(heap),
        None => raise_runtime(vm, heap, state, VmError::Type),
    }
}

/// PrivateSet (ES 7.3.31): (obj, key, value), TypeError when absent.
fn private_set<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let obj = args.get_handle(heap, 0);
    let key = args.get_handle(heap, 1);
    let value = args.get_handle(heap, 2);
    match obj.as_tagged(heap).private_find(heap, key.as_tagged(heap)) {
        Some(slot) => {
            slot.set(heap, obj.as_tagged(heap), value.as_tagged(heap));
            value.as_tagged(heap)
        }
        None => raise_runtime(vm, heap, state, VmError::Type),
    }
}

/// `#x in obj`: (key, obj) -> bool (own private presence only).
fn private_in<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let key = args.get_handle(heap, 0);
    let obj = args.get_handle(heap, 1);
    let has = obj
        .as_tagged(heap)
        .private_find(heap, key.as_tagged(heap))
        .is_some();
    Convert::boolean(heap, has)
}

/// Attach the instance-field array to the class constructor:
/// (ctor, fields).
fn set_class_fields<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ctor = args.get_handle(heap, 0);
    let fields = args.get_handle(heap, 1);
    let mut ok = false;
    if let Some(obj) = ctor.as_tagged(heap).as_heap_object() {
        let slots = obj.slots(heap);
        if obj.map(heap).kind().is_class_constructor() && slots.len() >= 3 {
            slots
                .element_slot(2)
                .set(heap, ctor.as_tagged(heap), fields.as_tagged(heap));
            ok = true;
        }
    }
    if !ok {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    ctor.as_tagged(heap)
}

/// InitializeInstanceElements (ES 7.3.33): (ctor, instance) -> instance.
/// Runs each field initializer with the instance as receiver and defines
/// the result onto it ({w+, e+, c+}).
fn init_instance_fields<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let ctor = args.get_handle(heap, 0);
    let instance = args.get_handle(heap, 1);
    state.handle_scope(|scope| -> Tagged<'a, Value> {
        let Some(obj) = ctor.as_tagged(heap).as_heap_object() else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let slots = obj.slots(heap);
        if slots.len() < 3 {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let fields = slots.at(heap, 2);
        if fields.ptr_eq(vm.known().undefined.get(heap)) {
            return instance.as_tagged(heap);
        }
        // root the field list across the initializer calls below
        let fields = scope.handle(fields);
        let count = fields
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| o.length())
            .unwrap_or(0);
        let mut i = 0;
        while i + 1 < count {
            let raw_key = fields
                .as_tagged(heap)
                .as_heap_object()
                .and_then(|o| o.element_value(heap, i))
                .unwrap_or_else(|| vm.known().undefined.get(heap).erase());
            // computed keys need ToPropertyKey canonicalization
            let Some(key) = rt_try!(
                vm,
                heap,
                state,
                Object::to_property_key(vm, heap, state, scope.handle(raw_key))
            ) else {
                return vm.known().exception.get(heap).erase();
            };
            let key = scope.handle(key);
            // recompute the initializer after the coercion (it allocated)
            let init = fields
                .as_tagged(heap)
                .as_heap_object()
                .and_then(|o| o.element_value(heap, i + 1))
                .unwrap_or_else(|| vm.known().undefined.get(heap).erase());
            let inst = instance.as_tagged(heap);
            let call_args = scope.stage(&[inst]);
            let value = scope.handle(rt_try!(
                vm,
                heap,
                state,
                HostCtx::enter(
                    vm,
                    state.runtime(vm, heap),
                    heap,
                    state,
                    scope.handle(init),
                    call_args,
                    None,
                )
            ));
            if value.as_tagged(heap).ptr_eq(vm.known().exception.get(heap)) {
                return vm.known().exception.get(heap).erase();
            }
            let instance_obj = scope
                .cast::<Object>(heap, instance.as_tagged(heap))
                .expect("class instance is an object");
            let defined = rt_try!(
                vm,
                heap,
                state,
                Object::define_own_property(
                    heap,
                    &scope,
                    instance_obj,
                    key,
                    PropertyDescriptor::Data {
                        value,
                        writable: true,
                        enumerable: true,
                        configurable: true,
                    },
                )
            );
            if !defined {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
            i += 2;
        }
        instance.as_tagged(heap)
    })
}

// ---- frame access -----------------------------------------------------------

/// Where a dynamically resolved (direct eval) name lives.
enum DynamicBinding<'s> {
    /// An environment-record slot (possibly the hole): not deletable.
    Slot(Handle<'s, Value>),
    /// A property of a context extension object, created by a sloppy eval
    /// declaration: deletable (ES 19.2.1.3 CreateMutableBinding(..., true)).
    Extension {
        holder: Handle<'s, Value>,
        value: Handle<'s, Value>,
    },
}

/// Walk the current frame's context chain looking for a binding named
/// `name` (direct eval). The result is rooted in `scope` so callers may
/// allocate before inspecting it.
fn dynamic_lookup_frame<'s>(
    heap: &Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    name: Handle<'_, Value>,
) -> Result<Option<DynamicBinding<'s>>, VmError> {
    let context = frame_context_value(state, heap)?;
    let mut context = context.get_as::<ContextObject>(heap).ok_or(VmError::Type)?;
    loop {
        let ctx = context;
        if !ctx.scope_info.get(heap).is_repl_context()
            && let Some((i, _)) = ctx
                .scope_info
                .get(heap)
                .context_slot_index(heap, name.as_tagged(heap))
        {
            return Ok(Some(DynamicBinding::Slot(
                scope.handle(ctx.slots.get(heap).as_ref().element_slot(i).get(heap)),
            )));
        }
        if let Some(extension) = ctx.extension.get(heap)
            && let Some(value) = extension_property(heap, scope, extension, name)
        {
            return Ok(Some(DynamicBinding::Extension {
                holder: scope.handle(extension.erase()),
                value,
            }));
        }
        match ctx.outer.get(heap) {
            Some(outer) => context = outer,
            None => {
                let table = heap.known().script_context_table.get(heap);
                let Some(found) = table.lookup(heap, name.as_tagged(heap)) else {
                    return Ok(None);
                };
                let context = table.context(heap, found.context_index);
                return Ok(Some(DynamicBinding::Slot(
                    scope.handle(
                        context
                            .slots
                            .get(heap)
                            .as_ref()
                            .element_slot(found.slot_index)
                            .get(heap),
                    ),
                )));
            }
        }
    }
}

/// Own-property lookup on a context extension object. Extension objects are
/// internal variable environments: they behave as if they have no prototype.
fn extension_property<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    extension: Tagged<'_, Object>,
    name: Handle<'_, Value>,
) -> Option<Handle<'s, Value>> {
    let extension = scope.handle(extension.erase());
    let mut it = vm_core::lookup::Lookup::new(heap, scope, extension, name, true);
    match it.next(heap, scope) {
        vm_core::lookup::Hit::Miss => None,
        vm_core::lookup::Hit::Data { value, .. }
        | vm_core::lookup::Hit::Element { value, .. }
        | vm_core::lookup::Hit::Parent { value, .. } => Some(scope.handle(value)),
        _ => Some(heap.known().undefined.root_handle().erase()),
    }
}

/// The current frame's super constructor and new.target (direct
/// super() calls, ES 15.4.3): the running closure's [[Prototype]] must
/// be a constructor.
fn frame_super_parts<'a>(
    heap: &'a Heap,
    state: &ThreadState,
) -> Result<(Tagged<'a, Value>, Tagged<'a, Value>), VmError> {
    if !state.is_frame_active() {
        return Err(VmError::Type);
    }
    let base = state.frame_base();
    let Some(callee) = state.stack().super_constructor(heap, base) else {
        return Err(VmError::Type);
    };
    Ok((
        callee,
        state.stack().frame_header(base).new_target_slot().get(heap),
    ))
}

// ---- store outcomes ---------------------------------------------------------

/// A full [[Get]] that treats non-callable getters (an absent half of an
/// accessor pair) as undefined instead of throwing.
fn get_property_lenient<'a>(
    ctx: HostCtx<'a>,
    receiver: Handle<'_, Value>,
    name: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let result = match Object::get_property(vm, heap, state, receiver, receiver, name, false) {
            Ok(result) => result,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        match result {
            Coercion::Value(v) => {
                let v = scope.handle(v);
                v.as_tagged(heap).erase()
            }
            Coercion::Threw => vm.known().exception.get(heap).erase(),
        }
    })
}

// ---- class definition helpers -----------------------------------------------

/// SetFunctionName (ES 8.4.4): (fn, key, prefix) -> fn. Redefines `name`
/// on the closure ({w−, e−, c+}); the prefix discriminant (a Smi) is
/// 0 none, 1 "get ", 2 "set ". Class members named `name` define over the
/// constructor after ClassDefinitionEvaluation set its name — an already
/// explicitly defined `name` wins (ES 15.7.14: SetFunctionName happens
/// before element installation).
fn set_function_name<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let fn_value = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let prefix = args.get(heap, 2).to_i64().unwrap_or(0);
    // name construction allocates (interning): the closure stays rooted
    // in its argument handle across it
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key).erase();
        // a Symbol key contributes "[description]", any other key is
        // ToString'd, and a prefix prepends "prefix ".
        let mut units: Vec<u16> = match prefix {
            1 => b"get ".iter().map(|&b| b as u16).collect(),
            2 => b"set ".iter().map(|&b| b as u16).collect(),
            _ => Vec::new(),
        };
        if let Some(symbol) = key.as_tagged(heap).get_as::<Symbol>(heap) {
            units.push('[' as u16);
            let description = scope.handle(symbol.as_ref().description(heap));
            if let Some(description) = description.as_tagged(heap).get_as::<DenseString>(heap) {
                description.as_ref().data(heap).write_units(&mut units);
            }
            units.push(']' as u16);
        } else {
            let text = scope.handle(rt_try!(
                vm,
                heap,
                state,
                Convert::to_string(heap, &scope, key)
            ));
            let text = text
                .as_tagged(heap)
                .get_as::<DenseString>(heap)
                .expect("ToString yields a string");
            text.as_ref().data(heap).write_units(&mut units);
        }
        let name = vm
            .interner()
            .intern(heap, &scope, StringData::Utf16(&units))
            .erase();
        let Some(fn_obj) = scope.cast::<Object>(heap, fn_value.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let name_key = vm.known().strings.name.get_handle(heap, &scope);
        // the closure's own placeholder is never writable nor an accessor,
        // so only explicit member defines match here
        let explicit = match Object::ordinary_own_descriptor(
            heap,
            &scope,
            fn_obj.as_tagged(heap).erase(),
            name_key.as_tagged(heap).erase(),
        ) {
            Some(PropertyDescriptor::Data { writable, .. }) => writable,
            Some(PropertyDescriptor::Accessor { .. }) => true,
            None => false,
        };
        let defined = if explicit {
            true
        } else {
            rt_try!(
                vm,
                heap,
                state,
                Object::define_own_property(
                    heap,
                    &scope,
                    fn_obj,
                    name_key,
                    PropertyDescriptor::Data {
                        value: name,
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                )
            )
        };
        if !defined {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        fn_value.as_tagged(heap)
    })
}

/// Accessor member installation (ES 14.3.10): (target, key, closure,
/// flags). Defines one accessor half, merging with an existing pair under
/// the same key; flags bit 0 marks the getter half, PropertyFlags bits
/// carry enumerability.
fn install_accessor<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let closure = args.get_handle(heap, 2);
    let flags = args.get(heap, 3).to_i64().map(|v| v as u32).unwrap_or(0);
    // the key coercion allocates: target and closure stay rooted in
    // their argument handles across it
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let is_getter = flags & 1 != 0;
        let enumerable = flags & bytecode::PropertyFlags::DontEnum.bits() == 0;
        // ES 7.3.5 ToPropertyDescriptor target: Type(O) must be Object
        if target.as_tagged(heap).is_primitive(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        // completion merges with the current other half
        let partial = PartialDescriptor {
            get: is_getter.then_some(closure),
            set: (!is_getter).then_some(closure),
            enumerable: Some(enumerable),
            configurable: Some(true),
            ..PartialDescriptor::default()
        };
        let defined = match rt_try!(
            vm,
            heap,
            state,
            Proxy::define_own_property(vm, heap, state, &scope, target, key.erase(), partial)
        ) {
            Flow::Value(defined) => defined,
            Flow::Threw => return vm.known().exception.get(heap).erase(),
        };
        if !defined {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        closure.as_tagged(heap)
    })
}

/// [[DefineOwnProperty]] with exact attributes (class member
/// installation): (obj, key, value, flags) -> obj. Define sites are
/// strict-mode code: a rejected define throws a TypeError. flags are
/// PropertyFlags bits (the Accessor bit: the value is an AccessorPair).
fn define_own_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let value = args.get_handle(heap, 2);
    let flags = args.get(heap, 3).to_i64().map(|v| v as u32).unwrap_or(0);
    // the key coercion allocates: receiver and value stay rooted in
    // their argument handles across it
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        // proxies run their `defineProperty` trap (ES 20.2.5.6); define
        // sites are strict-mode: a rejected define throws
        if Proxy::is_proxy(heap, receiver.as_tagged(heap)) {
            let enumerable = flags & bytecode::PropertyFlags::DontEnum.bits() == 0;
            let configurable = flags & bytecode::PropertyFlags::DontDelete.bits() == 0;
            let partial = if flags & bytecode::PropertyFlags::Accessor.bits() != 0 {
                let pair = rt_try!(
                    vm,
                    heap,
                    state,
                    value
                        .as_tagged(heap)
                        .get_as::<AccessorPair>(heap)
                        .ok_or(VmError::Type)
                );
                let pair = pair;
                PartialDescriptor {
                    value: None,
                    get: Some(scope.handle(pair.get.get(heap))),
                    set: Some(scope.handle(pair.set.get(heap))),
                    writable: None,
                    enumerable: Some(enumerable),
                    configurable: Some(configurable),
                }
            } else {
                PartialDescriptor {
                    value: Some(scope.handle(value.as_tagged(heap))),
                    get: None,
                    set: None,
                    writable: Some(flags & bytecode::PropertyFlags::ReadOnly.bits() == 0),
                    enumerable: Some(enumerable),
                    configurable: Some(configurable),
                }
            };
            return match rt_try!(
                vm,
                heap,
                state,
                Proxy::define_own_property(vm, heap, state, &scope, receiver, key.erase(), partial,)
            ) {
                Flow::Threw => vm.known().exception.get(heap).erase(),
                Flow::Value(false) => raise_runtime(vm, heap, state, VmError::Type),
                Flow::Value(true) => receiver.as_tagged(heap),
            };
        }
        // Type(O) must be Object (ES 20.1.2.4 [[DefineOwnProperty]])
        if receiver.as_tagged(heap).is_primitive(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let name = match rt_try!(
            vm,
            heap,
            state,
            key.as_tagged(heap).erase().classify_key(heap)
        ) {
            Key::Element(i) => Tagged::from(Smi::new(i as i64)),
            Key::Name(name) => name,
        };
        let enumerable = flags & bytecode::PropertyFlags::DontEnum.bits() == 0;
        let configurable = flags & bytecode::PropertyFlags::DontDelete.bits() == 0;
        let desc = if flags & bytecode::PropertyFlags::Accessor.bits() != 0 {
            let pair = rt_try!(
                vm,
                heap,
                state,
                value
                    .as_tagged(heap)
                    .get_as::<AccessorPair>(heap)
                    .ok_or(VmError::Type)
            );
            let pair = pair;
            PropertyDescriptor::Accessor {
                get: scope.handle(pair.get.get(heap)),
                set: scope.handle(pair.set.get(heap)),
                enumerable,
                configurable,
            }
        } else {
            PropertyDescriptor::Data {
                value: scope.handle(value.as_tagged(heap)),
                writable: flags & bytecode::PropertyFlags::ReadOnly.bits() == 0,
                enumerable,
                configurable,
            }
        };
        let name = scope.handle(name);
        let receiver_obj = scope
            .cast::<Object>(heap, receiver.as_tagged(heap))
            .expect("checked object above");
        let defined = rt_try!(
            vm,
            heap,
            state,
            Object::define_own_property(heap, &scope, receiver_obj, name, desc)
        );
        if !defined {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        receiver.as_tagged(heap)
    })
}

/// [[SetPrototypeOf]] (class prototype wiring): (obj, proto) -> obj.
fn set_prototype<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let obj = args.get_handle(heap, 0);
        let proto = args.get_handle(heap, 1);
        let obj_ref = scope
            .cast::<Object>(heap, obj.as_tagged(heap))
            .expect("obj is an object");
        rt_try!(
            vm,
            heap,
            state,
            Object::set_prototype(heap, &scope, obj_ref, proto)
        );
        obj.as_tagged(heap)
    })
}

/// Class extends validation (ES 15.7.14 step 15.e): (value) -> value,
/// TypeError unless the superclass is null or a constructor.
fn throw_if_not_constructor_or_null<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let v = args.get_handle(heap, 0);
    let null = vm.known().null.get(heap);
    let ok = v.as_tagged(heap) == null
        || v.as_tagged(heap)
            .as_heap_object()
            .is_some_and(|obj| obj.map(heap).kind().is_constructor());
    if !ok {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    v.as_tagged(heap)
}

/// superCtor.prototype validation: (value) -> value, TypeError unless the
/// value is an Object or null.
fn throw_if_not_object_or_null<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let v = args.get_handle(heap, 0);
    let null = vm.known().null.get(heap);
    let ok = v.as_tagged(heap) == null || !v.as_tagged(heap).is_primitive(heap);
    if !ok {
        return raise_runtime(vm, heap, state, VmError::Type);
    }
    v.as_tagged(heap)
}

/// [[ThisBindingStatus]] guard of derived constructors (ES 10.2.2):
/// (value) -> value, ReferenceError when `this` is still the hole.
fn throw_super_not_called_if_hole<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let v = args.get_handle(heap, 0);
    if v.as_tagged(heap) == vm.known().the_hole.get(heap) {
        // "Must call super constructor before accessing 'this'"
        return raise_runtime(vm, heap, state, VmError::Reference);
    }
    v.as_tagged(heap)
}

/// InitializeThisBinding guard (ES 10.2.2): (value) -> value,
/// ReferenceError unless `this` is still the hole (super() runs once).
fn throw_super_already_called_if_not_hole<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let v = args.get_handle(heap, 0);
    if v.as_tagged(heap) != vm.known().the_hole.get(heap) {
        // "Super constructor may only be called once"
        return raise_runtime(vm, heap, state, VmError::Reference);
    }
    v.as_tagged(heap)
}

// ---- super() construction (ES 15.4.3) ---------------------------------------

/// The shared ConstructSuper tail: construct `callee` with `new_target`,
/// giving derived parents the hole receiver. The instance lands in the
/// return value; the exception sentinel escapes when user code threw.
fn construct_super_construct<'a>(
    ctx: HostCtx<'a>,
    callee: Handle<'_, Value>,
    new_target: Handle<'_, Value>,
    args: &[Handle<'_, Value>],
) -> Tagged<'a, Value> {
    let runtime = ctx.runtime;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let new_target_tagged = new_target.as_tagged(heap);
        if new_target_tagged.ptr_eq(vm.known().undefined.get(heap))
            || new_target_tagged.ptr_eq(vm.known().the_hole.get(heap))
        {
            // not inside a [[Construct]]: reachable via an arrow that escaped
            // the constructor
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let derived = callee
            .as_tagged(heap)
            .as_heap_object()
            .filter(|obj| !obj.map(heap).kind().is_native())
            .and_then(|obj| obj.callable_info(heap))
            .is_some_and(|info| info.function_kind().is_derived_class_constructor());
        let Some(callee) = scope.cast::<Object>(heap, callee.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let Some(new_target) = scope.cast::<Object>(heap, new_target.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let (receiver, allocated) = if derived {
            (vm.known().the_hole.get_handle(heap, &scope).erase(), false)
        } else {
            // TODO: native constructors allocate their own receiver, so this
            // synthesized object is unused (and Array reads
            // `new.target.prototype` again).
            match Object::create_construct_receiver_value(vm, heap, state, new_target.erase()) {
                // the receiver must stay rooted across the callee call:
                // a primitive return falls back to it after the call
                // allocated (and possibly moved it)
                Ok(Some(r)) => (scope.handle(r), true),
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        };
        let mut args_v = Vec::with_capacity(args.len() + 1);
        args_v.push(receiver);
        args_v.extend_from_slice(args);
        let call_args = scope.stage_handles(&args_v);
        let result = scope.handle(rt_try!(
            vm,
            heap,
            state,
            HostCtx::enter(
                vm,
                runtime,
                heap,
                state,
                callee.erase(),
                call_args,
                Some(new_target.erase()),
            )
        ));
        if result
            .as_tagged(heap)
            .ptr_eq(vm.known().exception.get(heap))
        {
            return result.as_tagged(heap);
        }
        if result.as_tagged(heap).is_primitive(heap) {
            if allocated {
                receiver.as_tagged(heap)
            } else {
                // a derived constructor returned a primitive: only
                // reachable via `return <primitive>` (ES 9.2.2.1)
                raise_runtime(vm, heap, state, VmError::Type)
            }
        } else {
            result.as_tagged(heap)
        }
    })
}

/// super(...): (args...) -> instance. Resolves the super constructor and
/// new.target from the executing frame.
fn construct_super<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm,
        runtime,
        heap,
        state,
    } = ctx;
    state.handle_scope(|scope| {
        let (callee, new_target) = rt_try!(vm, heap, state, frame_super_parts(heap, state));
        // keep the resolved parts rooted across the construct below
        let callee = scope.handle(callee);
        let new_target = scope.handle(new_target);
        let arg_words: Vec<Handle<'_, Value>> = args.iter(heap).map(|t| scope.handle(t)).collect();
        construct_super_construct(
            HostCtx::new(vm, runtime, heap, state),
            callee,
            new_target,
            &arg_words,
        )
    })
}

/// super() forwarding the frame's full argument list (synthesized default
/// derived constructors, ES 15.7.13): () -> instance.
fn construct_super_all_args<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm,
        runtime,
        heap,
        state,
    } = ctx;
    state.handle_scope(|scope| {
        let (callee, new_target) = rt_try!(vm, heap, state, frame_super_parts(heap, state));
        let callee = scope.handle(callee);
        let new_target = scope.handle(new_target);
        if !state.is_frame_active() {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let base = state.frame_base();
        let argc = state.stack().argc(base).saturating_sub(1);
        // params descend below the frame anchor: read them one by one in
        // argument order (formal j at operand -(j+1))
        let arg_words: Vec<Handle<'_, Value>> = (1..=argc)
            .map(|j| scope.handle(state.stack().reg(heap, base, j as i32)))
            .collect();
        construct_super_construct(
            HostCtx::new(vm, runtime, heap, state),
            callee,
            new_target,
            &arg_words,
        )
    })
}

/// Arrow-delegated super(): (args..., closure, new_target) -> instance.
/// The constructor closure and its new.target ride the tail of the
/// argument window (threaded through .this_function).
fn construct_super_via<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm,
        runtime,
        heap,
        state,
    } = ctx;
    let n = args.len();
    if n < 2 {
        return raise_runtime(vm, heap, state, VmError::Arity);
    }
    state.handle_scope(|scope| {
        let closure = args.get_handle(heap, n - 2);
        let new_target = args.get_handle(heap, n - 1);
        let Some(obj) = closure.as_tagged(heap).as_heap_object() else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let proto = obj.prototype(heap);
        let Some(proto_obj) = proto.as_heap_object() else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        if !proto_obj.map(heap).kind().is_constructor() {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let callee = scope.handle(proto);
        let arg_words: Vec<Handle<'_, Value>> =
            (0..n - 2).map(|i| args.get_handle(heap, i)).collect();
        construct_super_construct(
            HostCtx::new(vm, runtime, heap, state),
            callee,
            new_target,
            &arg_words,
        )
    })
}

// ---- dynamic names (direct eval) ---------------------------------------------

/// Direct-eval name load: (name) -> value. Walks the frame context chain
/// by name (slots, then extension objects); unresolved names fall back to
/// the global object.
fn load_dynamic_name<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let name = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let found = rt_try!(
            vm,
            heap,
            state,
            dynamic_lookup_frame(heap, state, &scope, name)
        );
        match found {
            Some(DynamicBinding::Slot(slot))
                if !slot.as_tagged(heap).ptr_eq(vm.known().the_hole.get(heap)) =>
            {
                slot.as_tagged(heap)
            }
            Some(DynamicBinding::Slot(_)) => raise_runtime(vm, heap, state, VmError::Reference),
            Some(DynamicBinding::Extension { value, .. }) => value.as_tagged(heap),
            None => {
                // unresolved: fall back to a global object property
                let global = vm.known().global_object.get_handle(heap, &scope).erase();
                get_property_lenient(
                    HostCtx::new(vm, state.runtime(vm, heap), heap, state),
                    global,
                    name,
                )
            }
        }
    })
}

/// Direct-eval name store: (value, name) -> value. Walks the caller's
/// context chain in order (slots then extension object per context);
/// unresolved names store on the global object.
fn store_dynamic_name<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    let name = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        debug_assert!(state.is_frame_active());
        let header = state.stack().frame_header(state.frame_base());
        let callable = header.callable(heap);
        let info = callable
            .callable_info(heap)
            .expect("a bytecode frame's callable must have callable info");
        let mode = info.mode();

        let context = rt_try!(vm, heap, state, frame_context_value(state, heap));
        let mut context = rt_try!(
            vm,
            heap,
            state,
            context.get_as::<ContextObject>(heap).ok_or(VmError::Type)
        );
        let target = loop {
            let ctx = context;
            if !ctx.scope_info.get(heap).is_repl_context()
                && let Some((i, info)) = ctx
                    .scope_info
                    .get(heap)
                    .context_slot_index(heap, name.as_tagged(heap))
            {
                let slot = ctx.slots.get(heap).as_ref().element_slot(i);
                if slot.get(heap) == heap.known().the_hole.get(heap).erase() {
                    return raise_runtime(vm, heap, state, VmError::Reference);
                }
                if info_is_immutable(info) {
                    return raise_runtime(
                        vm,
                        heap,
                        state,
                        VmError::Message(vm_core::Message::ConstAssign),
                    );
                }
                slot.set(heap, ctx.erase(), value.as_tagged(heap));
                return value.as_tagged(heap);
            }
            if let Some(extension) = ctx.extension.get(heap)
                && extension_property(heap, &scope, extension, name).is_some()
            {
                break scope.handle(extension.erase());
            }
            match ctx.outer.get(heap) {
                Some(outer) => context = outer,
                None => break vm.known().global_object.get_handle(heap, &scope).erase(),
            }
        };
        if let Some(found) = heap
            .known()
            .script_context_table
            .get(heap)
            .lookup(heap, name.as_tagged(heap))
        {
            let ctx = heap
                .known()
                .script_context_table
                .get(heap)
                .context(heap, found.context_index);
            let slot = ctx.slots.get(heap).as_ref().element_slot(found.slot_index);
            if slot.get(heap) == heap.known().the_hole.get(heap).erase() {
                return raise_runtime(vm, heap, state, VmError::Reference);
            }
            if info_is_immutable(found.info) {
                return raise_runtime(
                    vm,
                    heap,
                    state,
                    VmError::Message(vm_core::Message::ConstAssign),
                );
            }
            slot.set(heap, ctx.erase(), value.as_tagged(heap));
            return value.as_tagged(heap);
        }

        let key = scope.handle(name.as_tagged(heap).erase());
        match rt_try!(
            vm,
            heap,
            state,
            Object::set_property(
                vm,
                heap,
                state,
                target,
                target,
                key,
                value,
                StoreSemantics::WriteThrough,
                true,
                mode,
            )
        ) {
            Flow::Threw => vm.known().exception.get(heap).erase(),
            Flow::Value(_) => value.as_tagged(heap),
        }
    })
}

fn store_repl_global_init<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let value = args.get_handle(heap, 0);
    let name = args.get_handle(heap, 1);
    state.handle_scope(|_scope| {
        let table = heap.known().script_context_table.get(heap);
        let Some(found) = table.lookup(heap, name.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Reference);
        };
        let context = table.context(heap, found.context_index);
        context
            .slots
            .get(heap)
            .as_ref()
            .element_slot(found.slot_index)
            .set(heap, context.erase(), value.as_tagged(heap));
        value.as_tagged(heap)
    })
}

/// Sloppy direct-eval declaration. `value == None` is a var; otherwise a
/// function declaration is installed with that value.
fn resolve_direct_eval<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let js = &ctx.runtime::<JSRuntime>();
    let ids = &js.ids;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let callee = args.get_handle(heap, 0);
    if callee.as_tagged(heap) != js.known.eval_function.get(heap).erase() {
        return callee.as_tagged(heap).erase();
    }
    let source = args.get_handle(heap, 1);
    let Some(text) = source
        .as_tagged(heap)
        .get_as::<DenseString>(heap)
        .map(|s| s.to_rust_string(heap))
    else {
        return callee.as_tagged(heap).erase();
    };
    let strict = state.is_frame_active()
        && state
            .stack()
            .frame_header(state.frame_base())
            .callable(heap)
            .callable_info(heap)
            .is_some_and(|info| info.mode() == LanguageMode::Strict);
    let mode = if strict {
        bytecode::SourceMode::EvalStrict
    } else {
        bytecode::SourceMode::Eval
    };
    let program = match js_compiler::compile_js(&text, mode, ids) {
        Ok(program) => program,
        Err(_) => return raise_runtime(vm, heap, state, VmError::Syntax),
    };
    state.handle_scope(|scope| {
        let context = rt_try!(vm, heap, state, frame_context_value(state, heap));
        let Some(context) = scope.cast::<ContextObject>(heap, context) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        match Materialize::closure_vm(vm, heap, state, &scope, &program, JSRuntime::ID, context) {
            Ok(closure) => closure.as_tagged(heap).erase(),
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    })
}

fn validate_eval_binding<'s>(
    heap: &Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    name: Handle<'_, Value>,
    is_function: bool,
) -> Result<Handle<'s, ContextObject>, VmError> {
    let context = frame_context_value(state, heap)?;
    let mut context = context.get_as::<ContextObject>(heap).ok_or(VmError::Type)?;
    loop {
        let scope_info = context.scope_info.get(heap);
        if let Some((_, info)) = scope_info.context_slot_index(heap, name.as_tagged(heap))
            && info_is_lexical(info)
        {
            return Err(VmError::Syntax);
        }
        if scope_info.is_declaration_context() {
            break;
        }
        context = context.outer.get(heap).ok_or(VmError::Type)?;
    }
    let context = scope
        .cast::<ContextObject>(heap, context.erase())
        .ok_or(VmError::Type)?;
    let ctx = context.as_tagged(heap);
    if let Some((_, info)) = ctx
        .scope_info
        .get(heap)
        .context_slot_index(heap, name.as_tagged(heap))
    {
        if info_is_lexical(info) {
            return Err(VmError::Syntax);
        }
        return Ok(context);
    }
    if let Some(extension) = ctx.extension.get(heap)
        && extension_property(heap, scope, extension, name).is_some()
    {
        return Ok(context);
    }
    if ctx.scope_info.get(heap).is_script_context() {
        if heap
            .known()
            .script_context_table
            .get(heap)
            .lookup(heap, name.as_tagged(heap))
            .is_some()
        {
            return Err(VmError::Syntax);
        }
        if is_function {
            if !intrinsics::can_declare_global_function(heap, scope, name) {
                return Err(VmError::Type);
            }
        } else if !intrinsics::can_declare_global_var(heap, scope, name) {
            return Err(VmError::Type);
        }
    }
    Ok(context)
}

fn declare_eval_binding<'a>(
    ctx: HostCtx<'a>,
    name: Handle<'_, Value>,
    value: Option<Handle<'_, Value>>,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let context = rt_try!(
            vm,
            heap,
            state,
            validate_eval_binding(heap, state, &scope, name, value.is_some())
        );
        let ctx = context.as_tagged(heap);
        if let Some((slot, info)) = ctx
            .scope_info
            .get(heap)
            .context_slot_index(heap, name.as_tagged(heap))
        {
            if info_is_lexical(info) {
                return raise_runtime(vm, heap, state, VmError::Syntax);
            }
            if let Some(value) = value {
                ctx.slots.get(heap).as_ref().element_slot(slot).set(
                    heap,
                    ctx.erase(),
                    value.as_tagged(heap),
                );
            }
            return vm.known().undefined.get(heap).erase();
        }

        if let Some(extension) = ctx.extension.get(heap)
            && extension_property(heap, &scope, extension, name).is_some()
        {
            if let Some(value) = value {
                let Some(extension) = scope.cast::<Object>(heap, extension.erase()) else {
                    return raise_runtime(vm, heap, state, VmError::Type);
                };
                let key = scope.handle(name.as_tagged(heap).as_name());
                let defined = rt_try!(
                    vm,
                    heap,
                    state,
                    Object::define_own_property(
                        heap,
                        &scope,
                        extension,
                        key,
                        PropertyDescriptor::data(value),
                    )
                );
                if !defined {
                    return raise_runtime(vm, heap, state, VmError::Type);
                }
            }
            return vm.known().undefined.get(heap).erase();
        }

        if ctx.scope_info.get(heap).is_script_context() {
            if let Some(value) = value {
                rt_try!(
                    vm,
                    heap,
                    state,
                    intrinsics::create_global_function_binding(heap, &scope, name, value, true)
                );
            } else {
                rt_try!(
                    vm,
                    heap,
                    state,
                    intrinsics::create_global_var_binding(heap, &scope, name, true)
                );
            }
            return vm.known().undefined.get(heap).erase();
        }

        let extension = match ctx.extension.get(heap) {
            Some(extension) => extension.as_handle(&scope),
            None => {
                let map = vm.known().object_initial_map.get_handle(heap, &scope);
                let extension = heap
                    .new_object(&scope, map, HandleSlice::EMPTY)
                    .as_handle(&scope);
                let ctx = context.as_tagged(heap);
                ctx.extension
                    .set(heap, ctx.erase(), extension.as_tagged(heap));
                extension
            }
        };
        let target = rt_try!(
            vm,
            heap,
            state,
            scope
                .cast::<Object>(heap, extension.as_tagged(heap).erase())
                .ok_or(VmError::Type)
        );
        let key = scope.handle(name.as_tagged(heap).as_name());
        let value = match value {
            Some(value) => value,
            None => vm.known().undefined.get_handle(heap, &scope).erase(),
        };
        let defined = rt_try!(
            vm,
            heap,
            state,
            Object::define_own_property(heap, &scope, target, key, PropertyDescriptor::data(value),)
        );
        if !defined {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        vm.known().undefined.get(heap).erase()
    })
}

fn validate_eval_declarations<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let declarations = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let Some(meta) = scope.cast::<FixedArray>(heap, declarations.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let (vars, funcs, _) = rt_try!(
            vm,
            heap,
            state,
            intrinsics::read_global_declarations(heap, &scope, meta.as_tagged(heap))
        );
        for name in &vars {
            rt_try!(
                vm,
                heap,
                state,
                validate_eval_binding(heap, state, &scope, *name, false)
            );
        }
        for name in &funcs {
            rt_try!(
                vm,
                heap,
                state,
                validate_eval_binding(heap, state, &scope, *name, true)
            );
        }
        vm.known().undefined.get(heap).erase()
    })
}

fn declare_eval_var<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let name = args.get_handle(ctx.heap, 0);
    declare_eval_binding(ctx, name, None)
}

fn declare_eval_function<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let name = args.get_handle(ctx.heap, 0);
    let value = args.get_handle(ctx.heap, 1);
    declare_eval_binding(ctx, name, Some(value))
}

fn throw_const_assign_error<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    raise_runtime(
        vm,
        heap,
        state,
        VmError::Message(vm_core::Message::ConstAssign),
    )
}

// ---- rest parameters ---------------------------------------------------------

/// A fresh array of the frame's arguments from formal index `first`:
/// (first) -> array.
fn create_rest_parameter<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let first = args.get(heap, 0).to_i64().map(|v| v as usize).unwrap_or(0);
    state.handle_scope(|scope| {
        if !state.is_frame_active() {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let base = state.frame_base();
        let argc = state.stack().argc(base); // receiver included
        let count = argc.saturating_sub(1).saturating_sub(first);
        let values: Vec<Handle<'_, Value>> = (0..count)
            .map(|i| scope.handle(state.stack().reg(heap, base, (first + i + 1) as i32)))
            .collect();
        let elements = heap.allocate_handle::<FixedArray>(scope.stage_handles(&values), &scope);
        let map = vm.known().js_array_map.get_handle(heap, &scope);
        heap.allocate_object(
            &scope,
            ObjectSlotsInit {
                map,
                values: HandleSlice::EMPTY,
                elements,
                length: values.len(),
            },
        )
        .erase()
    })
}

// ---- super property access (ES 15.4.2 / 15.4.4) -------------------------------

/// super.x load: (home, recv, key) -> value. GetSuperBase of the home
/// object walked with the split receiver/lookup-start; key coercion runs
/// after the parent link is resolved (user toString must not change the
/// chain searched).
fn super_get_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let home = args.get_handle(heap, 0);
    let recv = args.get_handle(heap, 1);
    let raw_key = args.get_handle(heap, 2);
    if recv
        .as_tagged(heap)
        .ptr_eq(vm.known().the_hole.get(heap).erase())
    {
        // super.x before super() in a derived constructor
        return raise_runtime(vm, heap, state, VmError::Reference);
    }
    // the key coercion allocates: home/recv stay rooted in their
    // argument handles across it
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let proto = home
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| o.prototype(heap));
        let name = match rt_try!(
            vm,
            heap,
            state,
            key.as_tagged(heap).erase().classify_key(heap)
        ) {
            Key::Element(i) => Tagged::from(Smi::new(i as i64)),
            Key::Name(name) => name,
        };
        let Some(proto) = proto else {
            return vm.known().undefined.get(heap).erase();
        };
        let start = scope.handle(proto);
        let key = scope.handle(name.erase());
        let result = match Object::get_property(vm, heap, state, start, recv, key, false) {
            Ok(result) => result,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        match result {
            Coercion::Value(v) => {
                let v = scope.handle(v);
                v.as_tagged(heap).erase()
            }
            Coercion::Threw => vm.known().exception.get(heap).erase(),
        }
    })
}

/// super.x store: (home, recv, key, value, semantics) -> value. ES stores
/// shadow inherited data properties on `this` unless the write-through
/// semantics flag is set; the parent link is resolved before any user key
/// coercion runs.
fn super_set_property<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let home = args.get_handle(heap, 0);
    let recv = args.get_handle(heap, 1);
    let raw_key = args.get_handle(heap, 2);
    let value = args.get_handle(heap, 3);
    let semantics_flag = args.get(heap, 4).to_i64().map(|v| v as u32).unwrap_or(0);
    if recv
        .as_tagged(heap)
        .ptr_eq(vm.known().the_hole.get(heap).erase())
    {
        return raise_runtime(vm, heap, state, VmError::Reference);
    }
    // the key coercion allocates: home/recv/value stay rooted in their
    // argument handles across it
    state.handle_scope(|scope| {
        let Some(key) = rt_try!(
            vm,
            heap,
            state,
            Object::to_property_key(vm, heap, state, raw_key)
        ) else {
            return vm.known().exception.get(heap).erase();
        };
        // root the name: the tagged result anchors the `&mut` borrow
        let key = scope.handle(key);
        let semantics = if semantics_flag & bytecode::SUPER_STORE_WRITE_THROUGH != 0 {
            StoreSemantics::WriteThrough
        } else {
            StoreSemantics::Shadow
        };
        let proto = home
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| o.prototype(heap));
        let start = match proto {
            Some(proto) => scope.handle(proto),
            None => recv,
        };
        let applied = rt_try!(
            vm,
            heap,
            state,
            Object::set_property(
                vm,
                heap,
                state,
                start,
                recv,
                key.erase(),
                value,
                semantics,
                false,
                LanguageMode::Strict
            )
        );
        match applied {
            Flow::Threw => return vm.known().exception.get(heap).erase(),
            Flow::Value(_) => {}
        }
        value.as_tagged(heap)
    })
}
