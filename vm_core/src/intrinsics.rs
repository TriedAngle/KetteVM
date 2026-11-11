//! Intrinsic natives the VM owns rather than a front end: global declaration
//! instantiation (ES 9.1.1.4.16, shared by the JS front end and Kette) and
//! the array exotic object's `length` accessor.

use crate::{
    AccessorInfo, Args, CallableInfoInit, CallableInfoObject, ContextObject, Convert,
    EdgeVisitable, FixedArray, FunctionKind, Handle, HandleScope, Heap, HostCtx, INFO_GLOBAL,
    MODE_CLASS, MODE_LET, Map, MapInit, MapKind, Object, PropertyDescriptor, Runtime, RuntimeId,
    ScriptContextTable, SetupError, SlotFlags, Smi, Tagged, VM, Value, Visitor, VmError,
    frame_context_value, info_is_lexical, info_mode, lookup::Hit, lookup::Lookup, mode_from_strict,
    raise, rt_try, tools::KetteTools,
};

// ---- array `length` ----
/// `length` getter of Array exotic objects
/// [ES 10.4.2](https://tc39.es/ecma262/#sec-array-exotic-objects)
/// Returns the receiver's `[[ArrayLength]]` (a data property in the spec).
pub fn array_length_get<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = args.get(heap, 0).as_heap_object() else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    if !object.as_ref().is_array_exotic(heap) {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    Smi::new(object.as_ref().length() as i64).into_tagged()
}

/// ArraySetLength ( array, propertyDesc )
/// [ES 10.4.2.4](https://tc39.es/ecma262/#sec-arraysetlength)
/// Applies a new array length; false when remaining elements block the shrink.
pub fn array_length_set<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let holder = args.get_handle(heap, 0);
        let value = args.get(heap, 1);
        let Some(obj) = holder.as_tagged(heap).as_heap_object() else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        if !obj.as_ref().is_array_exotic(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let obj = scope.handle(obj);
        let value = scope.handle(value);
        // ToUint32 and ToNumber each coerce the value (an object runs
        // valueOf twice); unequal results are an invalid length
        let new_len = rt_try!(
            heap,
            Object::anything_to_array_length(vm, heap, state, value)
        );
        // the descriptor is read after the coercions (re-entrant code may
        // have redefined it); a non-writable length refuses a different value
        let length_name = vm.known().strings.length.get(heap);
        let writable = obj
            .as_tagged(heap)
            .map(heap)
            .descriptors()
            .iter()
            .find(|d| d.name(heap).ptr_eq(length_name))
            .is_some_and(|d| d.flags().is_writable());
        if !writable && new_len as usize != obj.as_tagged(heap).length() {
            return Convert::boolean(heap, false);
        }
        // apply, then report whether every dropped element could be deleted
        let shrunk = Object::set_array_length(heap, &scope, &obj, new_len as usize);
        Convert::boolean(heap, shrunk)
    })
}

// ---- the VM's own runtime ----

/// The VM's own runtime: the natives that are not a front end's, so they are
/// available regardless of which languages a VM hosts. Added with
/// `vm.add::<VmCore>()` before the front end's runtime.
#[derive(Default)]
pub struct VmCore;

impl EdgeVisitable for VmCore {
    fn visit_edges(&self, _visitor: &mut dyn Visitor) {}
}

/// `%Function.prototype%` (ES 19.2.3): the canonical empty function.
fn function_prototype_call<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    ctx.vm.known().undefined.get(ctx.heap).erase()
}

/// The `%ThrowTypeError%` [[Call]] (ES 10.2.3).
fn throw_type_error<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    raise!(vm, heap, state, VmError::InvalidOperandType)
}

impl Runtime for VmCore {
    const ID: RuntimeId = RuntimeId(0);

    fn setup(vm: &mut VM, _state: &mut Self) -> Result<(), SetupError> {
        let call_index = vm.register_native(function_prototype_call);
        let throw_index = vm.register_native(throw_type_error);
        let getter = vm.register_native(array_length_get);
        let setter = vm.register_native(array_length_set);
        debug_assert_eq!(setter.0, getter.0 + 1);
        let tools = [
            vm.register_native(KetteTools::force_minor_gc),
            vm.register_native(KetteTools::force_major_gc),
            vm.register_native(KetteTools::shutdown),
        ];
        let mut thread = vm.attach();
        thread.handle_scope(|thread, scope| {
            let heap = thread.heap();
            // the canonical empty function object: callable, non-constructable,
            // [[Prototype]] = %Object.prototype%
            let object_prototype = heap
                .known()
                .object_prototype
                .get_handle(heap, &scope)
                .erase();
            let empty_context = heap.known().empty_context.get_handle(heap, &scope).erase();
            let empty = heap.known().strings.empty.get_handle(heap, &scope);
            let info = heap.allocate_handle::<CallableInfoObject>(
                CallableInfoInit::Native {
                    index: call_index,
                    mode: mode_from_strict(true),
                },
                &scope,
            );
            info.as_tagged(heap).set_metadata_full(
                heap,
                Some(empty.as_tagged(heap).erase()),
                0,
                0,
                FunctionKind::Normal,
            );
            let map = heap.allocate_handle::<Map>(
                MapInit {
                    kind: MapKind::OBJECT
                        .union(MapKind::CALLABLE)
                        .union(MapKind::EXTENDABLE),
                    value_slot_count: 2,
                    descriptors: &[],
                    prototype: object_prototype,
                },
                &scope,
            );
            let slots = scope.stage(&[
                info.as_tagged(heap).erase(),
                empty_context.as_tagged(heap).erase(),
            ]);
            let function_prototype = scope.handle(heap.new_object(&scope, map, slots));
            vm.known()
                .function_prototype
                .init(function_prototype.as_tagged(heap));
            // ordinary function objects' [[Prototype]] (ES 19.2.3.1)
            for map in [
                vm.known().function_map.get(heap),
                vm.known().non_constructor_function_map.get(heap),
                vm.known().class_constructor_map.get(heap),
            ] {
                map.prototype.set(
                    heap,
                    map.erase(),
                    function_prototype.as_tagged(heap).erase(),
                );
            }
            let length_key = heap.known().strings.length.get_handle(heap, &scope);
            let name_key = heap.known().strings.name.get_handle(heap, &scope);
            for (key, value) in [
                (length_key, scope.handle(Smi::new(0).into_tagged()).erase()),
                (name_key, empty.erase()),
            ] {
                Object::define_own_property_ordinary(
                    heap,
                    &scope,
                    function_prototype,
                    key,
                    PropertyDescriptor::Data {
                        value,
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                )
                .expect("Function.prototype metadata");
            }

            // %ThrowTypeError% (ES 10.2.3): frozen, non-constructable,
            // [[Prototype]] = %Function.prototype%.
            let throw_type_error = Object::native_function(heap, &scope, throw_index, 0);
            for (key, value) in [
                (length_key, scope.handle(Smi::new(0).into_tagged()).erase()),
                (name_key, empty.erase()),
            ] {
                Object::define_own_property_ordinary(
                    heap,
                    &scope,
                    throw_type_error,
                    key,
                    PropertyDescriptor::Data {
                        value,
                        writable: false,
                        enumerable: false,
                        configurable: false,
                    },
                )
                .expect("ThrowTypeError metadata");
            }
            if let Some(info) = throw_type_error.as_tagged(heap).callable_info(heap) {
                info.as_ref()
                    .set_name(heap, Some(empty.as_tagged(heap).erase()));
            }
            Object::make_non_extensible(heap, &scope, throw_type_error);
            vm.known()
                .throw_type_error
                .init(throw_type_error.as_tagged(heap));

            // `bootstrap` installs the `length` accessor on the array map
            // with no natives; point it at the ones registered above
            let map = heap.known().js_array_map.get_handle(heap, &scope);
            let name = heap.known().strings.length.get_handle(heap, &scope);
            if let Some(info) = map
                .as_tagged(heap)
                .as_ref()
                .descriptors()
                .iter()
                .find(|d| d.name(heap).ptr_eq(name.as_tagged(heap)))
                .and_then(|d| d.value.get(heap).get_as::<AccessorInfo>(heap))
            {
                info.as_ref().set_natives(heap, Some(getter), Some(setter));
            }
            KetteTools::install(thread.heap(), vm.known(), vm.interner(), &scope, tools)
        });
        Ok(())
    }
}

// ---- global declarations ----
pub type GlobalDeclarations<'s> = (
    Vec<Handle<'s, Value>>,
    Vec<Handle<'s, Value>>,
    Vec<Handle<'s, CallableInfoObject>>,
);

pub fn read_global_declarations<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    meta: Tagged<'_, FixedArray>,
) -> Option<GlobalDeclarations<'s>> {
    let var_count = meta.at(heap, 0).to_i64().unwrap_or(0).max(0) as usize;
    let mut pos = 1;
    let mut vars = Vec::with_capacity(var_count);
    for _ in 0..var_count {
        vars.push(scope.handle(meta.at(heap, pos)));
        pos += 1;
    }
    let func_count = meta.at(heap, pos).to_i64().unwrap_or(0).max(0) as usize;
    pos += 1;
    let mut funcs = Vec::with_capacity(func_count);
    for _ in 0..func_count {
        funcs.push(scope.handle(meta.at(heap, pos)));
        pos += 1;
    }
    let mut infos = Vec::with_capacity(func_count);
    for _ in 0..func_count {
        let info = scope.cast::<CallableInfoObject>(heap, meta.at(heap, pos))?;
        infos.push(info);
        pos += 1;
    }
    Some((vars, funcs, infos))
}

pub fn global_property_flags(
    heap: &Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
) -> Option<SlotFlags> {
    let global = heap.known().global_object.get_handle(heap, scope).erase();
    let mut lookup = Lookup::new(heap, scope, global, name, true);
    match lookup.next(heap, scope) {
        Hit::Data { flags, .. }
        | Hit::Accessor { flags, .. }
        | Hit::NativeAccessor { flags, .. } => Some(flags),
        _ => None,
    }
}

pub fn can_declare_global_var(
    heap: &Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
) -> bool {
    if global_property_flags(heap, scope, name).is_some() {
        return true;
    }
    heap.known()
        .global_object
        .get(heap)
        .as_ref()
        .is_extendable(heap)
}

pub fn can_declare_global_function(
    heap: &Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
) -> bool {
    let Some(flags) = global_property_flags(heap, scope, name) else {
        return heap
            .known()
            .global_object
            .get(heap)
            .as_ref()
            .is_extendable(heap);
    };
    flags.is_configurable()
        || (!flags.is_accessor() && flags.is_writable() && flags.is_enumerable())
}

pub fn create_global_var_binding(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
    deletable: bool,
) -> bool {
    if global_property_flags(heap, scope, name).is_some() {
        return true;
    }
    let Some(global) = scope.cast::<Object>(heap, heap.known().global_object.get(heap).erase())
    else {
        return false;
    };
    if !global.as_tagged(heap).is_extendable(heap) {
        return false;
    }
    let key = scope.handle(name.as_tagged(heap).as_name());
    let value = heap.known().undefined.get_handle(heap, scope).erase();
    Object::define_own_property_ordinary(
        heap,
        scope,
        global,
        key,
        PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: deletable,
        },
    )
    .expect("global binding define cannot throw")
}

pub fn create_global_function_binding(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
    value: Handle<'_, Value>,
    deletable: bool,
) -> bool {
    let Some(global) = scope.cast::<Object>(heap, heap.known().global_object.get(heap).erase())
    else {
        return false;
    };
    let flags = global_property_flags(heap, scope, name);
    let desc = match flags {
        Some(flags) if !flags.is_configurable() => PropertyDescriptor::Data {
            value,
            writable: flags.is_writable(),
            enumerable: flags.is_enumerable(),
            configurable: false,
        },
        _ => PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: deletable,
        },
    };
    let key = scope.handle(name.as_tagged(heap).as_name());
    Object::define_own_property_ordinary(heap, scope, global, key, desc)
        .expect("global function binding define cannot throw")
}

pub fn declare_globals<'a>(
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
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let Some((vars, func_names, func_infos)) =
            read_global_declarations(heap, &scope, meta.as_tagged(heap))
        else {
            raise!(vm, heap, state, VmError::InvalidGlobalDeclarations);
        };

        let Some(context_value) = frame_context_value(state, heap) else {
            unreachable!("no active frame");
        };
        let Some(context) = scope.cast::<ContextObject>(heap, context_value) else {
            unreachable!("frame context is not a context");
        };
        let lexical: Vec<(Handle<'_, Value>, i64)> = {
            let scope_info = context.as_tagged(heap).scope_info.get(heap);
            (0..scope_info.local_count())
                .filter(|&i| {
                    let info = scope_info.info(i);
                    info_is_lexical(info) && info & INFO_GLOBAL != 0
                })
                .map(|i| {
                    (
                        scope.handle(scope_info.name(heap, i).erase()),
                        scope_info.info(i),
                    )
                })
                .collect()
        };
        let table = heap.known().script_context_table.get_handle(heap, &scope);
        let repl_entry = context
            .as_tagged(heap)
            .scope_info
            .get(heap)
            .is_repl_context();

        for (name, mode) in &lexical {
            if let Some(found) = table.as_tagged(heap).lookup(heap, name.as_tagged(heap)) {
                let existing = table
                    .as_tagged(heap)
                    .context(heap, found.context_index)
                    .scope_info
                    .get(heap);
                let old_mode = info_mode(found.info);
                let new_mode = info_mode(*mode);
                let same_mode = old_mode == new_mode
                    || (old_mode == MODE_LET && new_mode == MODE_CLASS)
                    || (old_mode == MODE_CLASS && new_mode == MODE_LET);
                let same_repl_mode =
                    repl_entry && existing.is_repl_context() && same_mode && info_is_lexical(*mode);
                if !same_repl_mode {
                    raise!(vm, heap, state, VmError::InvalidDeclaration);
                }
            }
            if global_property_flags(heap, &scope, *name)
                .is_some_and(|flags| !flags.is_configurable())
            {
                raise!(vm, heap, state, VmError::InvalidDeclaration);
            }
        }
        for name in vars.iter().chain(func_names.iter()) {
            if table
                .as_tagged(heap)
                .lookup(heap, name.as_tagged(heap))
                .is_some()
            {
                raise!(vm, heap, state, VmError::InvalidDeclaration);
            }
        }
        for name in &func_names {
            if !can_declare_global_function(heap, &scope, *name) {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        for name in &vars {
            if !can_declare_global_var(heap, &scope, *name) {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }

        if !lexical.is_empty() {
            let table = ScriptContextTable::add(heap, &scope, table, context);
            vm.known().script_context_table.init(table.as_tagged(heap));
        }

        for (name, info) in func_names.iter().zip(func_infos.iter()) {
            let value = Object::create_closure(heap, &scope, *info, context);
            let value = scope.handle(value.erase());
            if !create_global_function_binding(heap, &scope, *name, value, false) {
                raise!(vm, heap, state, VmError::InvalidGlobalDeclaration);
            }
        }
        for name in &vars {
            if !create_global_var_binding(heap, &scope, *name, false) {
                raise!(vm, heap, state, VmError::InvalidGlobalDeclaration);
            }
        }
        vm.known().undefined.get(heap).erase()
    })
}
