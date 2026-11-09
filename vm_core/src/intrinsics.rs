//! Intrinsic natives the VM owns rather than a front end: global declaration
//! instantiation (ES 9.1.1.4.16, shared by the JS front end and Kette) and
//! the array exotic object's `length` accessor.

use crate::lookup::{Hit, Lookup};
use crate::tools::KetteTools;
use crate::{
    AccessorInfo, Args, CallableInfoObject, ContextObject, Convert, EdgeVisitable, FixedArray,
    Handle, HandleScope, Heap, HostCtx, INFO_GLOBAL, MODE_CLASS, MODE_LET, Map, MapInit, MapKind,
    Object, PropertyDescriptor, Runtime, RuntimeId, ScriptContextTable, SlotFlags, Smi, Tagged, VM,
    Value, Visitor, VmError, frame_context_value, info_is_lexical, info_mode, raise_runtime,
    rt_try,
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
        return raise_runtime(vm, heap, state, VmError::Type);
    };
    if !object.as_ref().is_array(heap) {
        return raise_runtime(vm, heap, state, VmError::Type);
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
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        if !obj.as_ref().is_array(heap) {
            return raise_runtime(vm, heap, state, VmError::Type);
        }
        let obj = scope.handle(obj);
        let value = scope.handle(value);
        // ToUint32 and ToNumber each coerce the value (an object runs
        // valueOf twice); unequal results are an invalid length
        let new_len = match Object::anything_to_array_length(vm, heap, state, value) {
            Ok(Some(n)) => n,
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
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
        match Object::set_array_length(heap, &scope, &obj, new_len as usize) {
            Ok(applied) => Convert::boolean(heap, applied),
            Err(err) => raise_runtime(vm, heap, state, err),
        }
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

impl Runtime for VmCore {
    const ID: RuntimeId = RuntimeId(0);

    fn setup(vm: &mut VM, _state: &mut Self) -> Result<(), VmError> {
        let call_index = vm.register_native(function_prototype_call);
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
            let map = heap.allocate_handle::<Map>(
                MapInit {
                    kind: MapKind::OBJECT
                        .union(MapKind::CALLABLE)
                        .union(MapKind::NATIVE)
                        .union(MapKind::EXTENDABLE),
                    value_slot_count: 2,
                    descriptors: &[],
                    prototype: object_prototype,
                },
                &scope,
            );
            let slots = scope.stage(&[
                Smi::new(call_index.0 as i64).into_tagged(),
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
        })
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
) -> Result<GlobalDeclarations<'s>, VmError> {
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
        let Some(info) = scope.cast::<CallableInfoObject>(heap, meta.at(heap, pos)) else {
            return Err(VmError::Type);
        };
        infos.push(info);
        pos += 1;
    }
    Ok((vars, funcs, infos))
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
) -> Result<(), VmError> {
    if global_property_flags(heap, scope, name).is_some() {
        return Ok(());
    }
    let global = scope
        .cast::<Object>(heap, heap.known().global_object.get(heap).erase())
        .ok_or(VmError::Type)?;
    if !global.as_tagged(heap).is_extendable(heap) {
        return Err(VmError::Type);
    }
    let key = scope.handle(name.as_tagged(heap).as_name());
    let value = heap.known().undefined.get_handle(heap, scope).erase();
    if Object::define_own_property_ordinary(
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
    )? {
        Ok(())
    } else {
        Err(VmError::Type)
    }
}

pub fn create_global_function_binding(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    name: Handle<'_, Value>,
    value: Handle<'_, Value>,
    deletable: bool,
) -> Result<(), VmError> {
    let global = scope
        .cast::<Object>(heap, heap.known().global_object.get(heap).erase())
        .ok_or(VmError::Type)?;
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
    if Object::define_own_property_ordinary(heap, scope, global, key, desc)? {
        Ok(())
    } else {
        Err(VmError::Type)
    }
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
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        let (vars, func_names, func_infos) = rt_try!(
            vm,
            heap,
            state,
            read_global_declarations(heap, &scope, meta.as_tagged(heap))
        );

        let context_value = rt_try!(vm, heap, state, frame_context_value(state, heap));
        let context = rt_try!(
            vm,
            heap,
            state,
            scope
                .cast::<ContextObject>(heap, context_value)
                .ok_or(VmError::Type)
        );
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
                    return raise_runtime(vm, heap, state, VmError::Syntax);
                }
            }
            if global_property_flags(heap, &scope, *name)
                .is_some_and(|flags| !flags.is_configurable())
            {
                return raise_runtime(vm, heap, state, VmError::Syntax);
            }
        }
        for name in vars.iter().chain(func_names.iter()) {
            if table
                .as_tagged(heap)
                .lookup(heap, name.as_tagged(heap))
                .is_some()
            {
                return raise_runtime(vm, heap, state, VmError::Syntax);
            }
        }
        for name in &func_names {
            if !can_declare_global_function(heap, &scope, *name) {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }
        for name in &vars {
            if !can_declare_global_var(heap, &scope, *name) {
                return raise_runtime(vm, heap, state, VmError::Type);
            }
        }

        if !lexical.is_empty() {
            let table = rt_try!(
                vm,
                heap,
                state,
                ScriptContextTable::add(heap, &scope, table, context)
            );
            vm.known().script_context_table.init(table.as_tagged(heap));
        }

        for (name, info) in func_names.iter().zip(func_infos.iter()) {
            let value = rt_try!(
                vm,
                heap,
                state,
                Object::create_closure(heap, &scope, *info, context)
            );
            let value = scope.handle(value.erase());
            rt_try!(
                vm,
                heap,
                state,
                create_global_function_binding(heap, &scope, *name, value, false)
            );
        }
        for name in &vars {
            rt_try!(
                vm,
                heap,
                state,
                create_global_var_binding(heap, &scope, *name, false)
            );
        }
        vm.known().undefined.get(heap).erase()
    })
}
