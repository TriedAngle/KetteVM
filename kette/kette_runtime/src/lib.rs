use vm_core::{
    Args, CallInfo, CallableInfoObject, DenseString, EdgeVisitable, Handle, HandleScope,
    HandleSlice, Heap, HostCtx, Key, Map, MapInit, MapKind, NativeIndex, Object, ObjectKind,
    OwnProperty, OwnPropertyLocation, PartialDescriptor, PropertyDescriptor, Prototype, Runtime,
    RuntimeId, SetupError, ShapeChange, ShouldThrow, Smi, Symbol, Tagged, ThreadState, VM, Value,
    Visitor, VmError, intrinsics::declare_globals, raise, raise_runtime, rt_try,
};

/// The Kette runtime: its state is the runtime itself (one instance per VM).
#[derive(Default)]
pub struct KetteRuntime {
    /// Id of the native that installs the top-level declarations.
    pub declare_globals: NativeIndex,
    /// Id of the native that installs one accessor half.
    pub install_accessor: NativeIndex,
    /// Id of the native that shallow-clones an object.
    pub clone: NativeIndex,
    pub add_parent_uncached: NativeIndex,
    pub delete_property: NativeIndex,
    pub define_in_place: NativeIndex,
    pub define_in_place_from: NativeIndex,
    pub publish_flat_module: NativeIndex,
}

impl EdgeVisitable for KetteRuntime {
    fn visit_edges(&self, _visitor: &mut dyn Visitor) {}
}

impl Runtime for KetteRuntime {
    const ID: RuntimeId = RuntimeId(2);

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), SetupError> {
        state.declare_globals = vm.register_native(declare_globals);
        state.install_accessor = vm.register_native(install_accessor);
        state.clone = vm.register_native(object_clone);
        state.add_parent_uncached = vm.register_native(object_add_parent_uncached);
        state.delete_property = vm.register_native(object_delete_property);
        state.define_in_place = vm.register_native(object_define_in_place);
        state.define_in_place_from = vm.register_native(object_define_in_place_from);
        state.publish_flat_module = vm.register_native(tools_publish_flat_module);
        let clone = state.clone;
        let print = vm.register_native(console_print);
        let is_type = vm.register_native(object_is_type);
        let add_parent = vm.register_native(object_add_parent);
        let add_parent_uncached = state.add_parent_uncached;
        let delete_property = state.delete_property;
        let define_in_place = state.define_in_place;
        let define_in_place_from = state.define_in_place_from;
        let publish_flat_module = state.publish_flat_module;
        let mut thread = vm.attach();
        thread.handle_scope(|thread, scope| {
            let (thread_vm, heap, thread_state) = thread.split();
            let object_prototype = heap.known().object_prototype.get_handle(heap, &scope);
            let map = heap.allocate_handle::<Map>(
                MapInit {
                    kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                    value_slot_count: 0,
                    descriptors: &[],
                    prototype: object_prototype.erase(),
                },
                &scope,
            );
            let console = scope.handle(heap.new_object(&scope, map, HandleSlice::EMPTY));
            Object::add_method(thread_vm, heap, &scope, console, "print", print, 0);
            let console_name = thread_vm.interner().intern_str(heap, &scope, "Console");
            let console_name = scope.handle(console_name.as_tagged(heap));
            let global = heap.known().global_object.get_handle(heap, &scope);
            Object::add_method(thread_vm, heap, &scope, global, "print", print, 0);
            Object::define_own_property_ordinary(
                heap,
                &scope,
                global,
                console_name,
                PropertyDescriptor::data(console.erase()),
            )
            .expect("install console global");
            Object::add_method(thread_vm, heap, &scope, object_prototype, "clone", clone, 0);

            let object_key = thread_vm.interner().intern_str(heap, &scope, "Object");
            let object_key = scope.handle(object_key.as_tagged(heap).erase());
            let object_ctor = Object::get_property(
                thread_vm,
                heap,
                thread_state,
                global.erase(),
                global.erase(),
                object_key.erase(),
                false,
            );
            if let Ok(object_ctor) = object_ctor {
                let object_ctor = scope.handle(object_ctor);
                if let Some(object_ctor) = object_ctor.as_tagged(heap).as_heap_object() {
                    let object_ctor = scope.handle(object_ctor);
                    Object::add_method(thread_vm, heap, &scope, object_ctor, "isType", is_type, 2);
                    Object::add_method(
                        thread_vm,
                        heap,
                        &scope,
                        object_ctor,
                        "addParent",
                        add_parent,
                        3,
                    );
                    Object::add_method(
                        thread_vm,
                        heap,
                        &scope,
                        object_ctor,
                        "addParentUncached",
                        add_parent_uncached,
                        3,
                    );
                    Object::add_method(
                        thread_vm,
                        heap,
                        &scope,
                        object_ctor,
                        "deleteProperty",
                        delete_property,
                        2,
                    );
                    Object::add_method(
                        thread_vm,
                        heap,
                        &scope,
                        object_ctor,
                        "defineInPlace",
                        define_in_place,
                        2,
                    );
                    Object::add_method(
                        thread_vm,
                        heap,
                        &scope,
                        object_ctor,
                        "defineInPlaceFrom",
                        define_in_place_from,
                        2,
                    );
                }
            }

            let tools_key = thread_vm.interner().intern_str(heap, &scope, "KetteTools");
            let tools_key = scope.handle(tools_key.as_tagged(heap).erase());
            let tools = Object::get_property(
                thread_vm,
                heap,
                thread_state,
                global.erase(),
                global.erase(),
                tools_key,
                false,
            );
            if let Ok(tools) = tools
                && let Some(tools) = tools.as_heap_object()
            {
                let tools = scope.handle(tools);
                Object::add_method(
                    thread_vm,
                    heap,
                    &scope,
                    tools,
                    "publishFlatModule",
                    publish_flat_module,
                    7,
                );
            }

            let none_name = scope.handle(
                thread_vm
                    .interner()
                    .intern_str(heap, &scope, "None")
                    .as_tagged(heap),
            );
            let none = heap.known().null.get_handle(heap, &scope).erase();
            Object::define_own_property_ordinary(
                heap,
                &scope,
                global,
                none_name,
                PropertyDescriptor::data(none),
            )
            .expect("install None global");

            // `os.name` reports the host OS for platform-specific paths.
            let os = scope.handle(heap.new_object(&scope, map, HandleSlice::EMPTY));
            let name = scope.handle(
                thread_vm
                    .interner()
                    .intern_str(heap, &scope, "name")
                    .as_tagged(heap),
            );
            let os_name = scope.handle(
                thread_vm
                    .interner()
                    .intern_str(heap, &scope, "os")
                    .as_tagged(heap),
            );
            let value = thread_vm
                .interner()
                .intern_str(heap, &scope, std::env::consts::OS);
            Object::define_own_property_ordinary(
                heap,
                &scope,
                os,
                name,
                PropertyDescriptor::data(value.erase()),
            )
            .expect("install os name");
            Object::define_own_property_ordinary(
                heap,
                &scope,
                global,
                os_name,
                PropertyDescriptor::data(os.erase()),
            )
            .expect("install os global");
            Ok(())
        })
    }
}

fn object_clone<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(call.receiver.as_tagged(heap).erase());
        let Some(clone) = Object::clone_shallow(heap, &scope, receiver) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        clone.as_tagged(heap).erase()
    })
}

/// `Object.isType(object, type)` — map identity, the VM's notion of type.
fn object_is_type<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    let object = args.get(heap, 0);
    let ty = args.get(heap, 1);
    let same = match (object.as_heap_object(), ty.as_heap_object()) {
        (Some(object), Some(ty)) => object.map(heap).ptr_eq(ty.map(heap)),
        _ => false,
    };
    let known = heap.known();
    if same {
        known.true_object.get(heap).erase()
    } else {
        known.false_object.get(heap).erase()
    }
}

/// `Object.addParent(object, name, parent)` — append or replace one
/// Self-style parent on an existing object.
fn object_add_parent<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let parent = args.get_handle(heap, 2);
    state.handle_scope(|scope| {
        let Some(target) = target.as_tagged(heap).as_heap_object() else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let target = scope.handle(target);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key);
        rt_try!(
            heap,
            Object::add_parent_preserving(vm, heap, state, &scope, target, key, parent)
        );
        target.as_tagged(heap).erase()
    })
}

/// `Object.addParentUncached(object, name, parent)`
/// Replaces a parent without caching a prototype transition.
fn object_add_parent_uncached<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let parent = args.get_handle(heap, 2);
    state.handle_scope(|scope| {
        let Some(target) = target.as_tagged(heap).as_heap_object() else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let target = scope.handle(target);
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key);
        rt_try!(
            heap,
            Object::add_parent_preserving_uncached(vm, heap, state, &scope, target, key, parent)
        );
        target.as_tagged(heap).erase()
    })
}

/// `Object.defineInPlace(object, template)`
/// Updates an ordinary object from `template` in place, recursively reusing
/// existing nested object identities and preserving unrelated properties.
fn object_define_in_place<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let old = args.get_handle(heap, 0);
    let new = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let old = scope.handle(old.as_tagged(heap).erase());
        let new = scope.handle(new.as_tagged(heap).erase());
        let Some(old_object) = definable_object(heap, old.as_tagged(heap).erase()) else {
            return heap.known().null.get(heap).erase();
        };
        let Some(new_object) = definable_object(heap, new.as_tagged(heap).erase()) else {
            return heap.known().null.get(heap).erase();
        };
        let old_object = scope.handle(old_object);
        let new_object = scope.handle(new_object);
        let mut plan = TransitionPlan::new();
        let Some(canonical) = plan_define_in_place(
            vm,
            heap,
            state,
            &scope,
            old_object.erase(),
            new_object.erase(),
            &mut plan,
        ) else {
            return heap.known().null.get(heap).erase();
        };
        heap.exclusive(|heap| apply_plan(heap, &plan));
        canonical.as_tagged(heap).erase()
    })
}

struct TransitionPlan<'s> {
    objects: Vec<(Handle<'s, Object>, Handle<'s, Object>)>,
    infos: Vec<(
        Handle<'s, CallableInfoObject>,
        Handle<'s, CallableInfoObject>,
    )>,
    writes: Vec<(Handle<'s, Object>, usize, Handle<'s, Value>)>,
}

impl<'s> TransitionPlan<'s> {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
            infos: Vec::new(),
            writes: Vec::new(),
        }
    }
}

/// `Object.defineInPlaceFrom(current, next)`
/// Plans in-place transitions for every value in `next` paired with the same
/// key in `current`, then applies the plan under one stop-the-world section.
fn object_define_in_place_from<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let current = args.get_handle(heap, 0);
    let next = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let current = scope.handle(current.as_tagged(heap).erase());
        let next = scope.handle(next.as_tagged(heap).erase());
        let mut plan = TransitionPlan::new();
        if plan_from(vm, heap, state, &scope, current, next, &mut plan).is_none() {
            return heap.known().false_object.get(heap).erase();
        }
        heap.exclusive(|heap| apply_plan(heap, &plan));
        next.as_tagged(heap).erase()
    })
}

fn plan_from<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    current: Handle<'s, Value>,
    next: Handle<'s, Value>,
    plan: &mut TransitionPlan<'s>,
) -> Option<()> {
    let current_object = scope.handle(definable_object(heap, current.as_tagged(heap).erase())?);
    let next_object = scope.handle(definable_object(heap, next.as_tagged(heap).erase())?);
    let undefined = heap.known().undefined.get_handle(heap, scope).erase();
    for key in Object::own_property_keys(vm, heap, state, scope, next_object.erase()).ok()? {
        let Some(new_property) =
            Object::own_property_ordinary(heap, scope, next_object.erase(), key)
        else {
            continue;
        };
        let PropertyDescriptor::Data {
            value: new_value, ..
        } = new_property.desc
        else {
            continue;
        };
        let old_value =
            match Object::own_property_ordinary(heap, scope, current_object.erase(), key) {
                Some(OwnProperty {
                    desc:
                        PropertyDescriptor::Data {
                            value: old_value, ..
                        },
                    ..
                }) => old_value,
                _ => undefined,
            };
        let canonical = plan_define_in_place(vm, heap, state, scope, old_value, new_value, plan)?;
        if canonical.as_tagged(heap).ptr_eq(new_value.as_tagged(heap)) {
            continue;
        }
        plan_write(heap, scope, next_object, key, canonical, plan)?;
    }
    Some(())
}

fn plan_write<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    target: Handle<'s, Object>,
    key: Handle<'s, Value>,
    value: Handle<'s, Value>,
    plan: &mut TransitionPlan<'s>,
) -> Option<()> {
    let property = Object::own_property_ordinary(heap, scope, target.erase(), key)?;
    let OwnPropertyLocation::MapSlot(index) = property.location? else {
        return None;
    };
    let offset = target.as_tagged(heap).map(heap).descriptor(index).offset();
    plan.writes.push((target, offset, value));
    Some(())
}

/// `KetteTools.publishFlatModule(module, staging, entry, oldOwners, owners,
/// contributions, preserveExtras)`
fn tools_publish_flat_module<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let module = args.get_handle(heap, 0);
    let staging = args.get_handle(heap, 1);
    let entry = args.get_handle(heap, 2);
    let old_owners = args.get_handle(heap, 3);
    let owners = args.get_handle(heap, 4);
    let contributions = args.get_handle(heap, 5);
    let preserve_extras = args.get_handle(heap, 6);
    state.handle_scope(|scope| {
        let module = scope.handle(module.as_tagged(heap).erase());
        let staging = scope.handle(staging.as_tagged(heap).erase());
        let entry = scope.handle(entry.as_tagged(heap).erase());
        let old_owners = scope.handle(old_owners.as_tagged(heap).erase());
        let owners = scope.handle(owners.as_tagged(heap).erase());
        let contributions = scope.handle(contributions.as_tagged(heap).erase());

        let Some(module_object) = definable_object(heap, module.as_tagged(heap).erase()) else {
            return heap.known().false_object.get(heap).erase();
        };
        let Some(staging_object) = definable_object(heap, staging.as_tagged(heap).erase()) else {
            return heap.known().false_object.get(heap).erase();
        };
        let Some(entry_object) = definable_object(heap, entry.as_tagged(heap).erase()) else {
            return heap.known().false_object.get(heap).erase();
        };
        if definable_object(heap, old_owners.as_tagged(heap).erase()).is_none() {
            return heap.known().false_object.get(heap).erase();
        }
        let module_object = scope.handle(module_object);
        let staging_object = scope.handle(staging_object);
        let entry_object = scope.handle(entry_object);

        let Some(storage) = Object::clone_shallow(heap, &scope, staging_object.erase()) else {
            return heap.known().false_object.get(heap).erase();
        };
        let prototype = module_object.as_tagged(heap).map(heap).prototype.get(heap);
        if !storage
            .as_tagged(heap)
            .map(heap)
            .prototype
            .get(heap)
            .ptr_eq(prototype)
        {
            let prototype = scope.handle(prototype);
            if Object::set_prototype(vm, heap, state, &scope, storage, prototype).is_err() {
                return heap.known().false_object.get(heap).erase();
            }
        }

        let mut plan = TransitionPlan::new();
        plan.objects.push((module_object, storage));
        let undefined = heap.known().undefined.get_handle(heap, &scope).erase();
        let Some(keys) =
            Object::own_property_keys(vm, heap, state, &scope, staging_object.erase()).ok()
        else {
            return heap.known().false_object.get(heap).erase();
        };
        for key in keys {
            let Some(new_property) =
                Object::own_property_ordinary(heap, &scope, staging_object.erase(), key)
            else {
                continue;
            };
            let PropertyDescriptor::Data {
                value: new_value,
                writable,
                enumerable,
                configurable,
            } = new_property.desc
            else {
                continue;
            };
            let old_value =
                match Object::own_property_ordinary(heap, &scope, module_object.erase(), key) {
                    Some(OwnProperty {
                        desc:
                            PropertyDescriptor::Data {
                                value: old_value, ..
                            },
                        ..
                    }) => old_value,
                    _ => undefined,
                };
            let Some(canonical) =
                plan_define_in_place(vm, heap, state, &scope, old_value, new_value, &mut plan)
            else {
                return heap.known().false_object.get(heap).erase();
            };
            if canonical.as_tagged(heap).ptr_eq(new_value.as_tagged(heap)) {
                continue;
            }
            let desc = PartialDescriptor {
                value: Some(canonical),
                writable: Some(writable),
                enumerable: Some(enumerable),
                configurable: Some(configurable),
                ..PartialDescriptor::default()
            };
            if !matches!(
                Object::define_own_property(
                    vm,
                    heap,
                    state,
                    &scope,
                    storage.erase(),
                    key,
                    desc,
                    ShouldThrow::DontThrow,
                ),
                Ok(true)
            ) {
                return heap.known().false_object.get(heap).erase();
            }
        }

        let preserve_extras = preserve_extras.as_tagged(heap).is_truthy(heap);
        let Some(module_keys) =
            Object::own_property_keys(vm, heap, state, &scope, module_object.erase()).ok()
        else {
            return heap.known().false_object.get(heap).erase();
        };
        for key in module_keys {
            if Object::own_property_ordinary(heap, &scope, staging_object.erase(), key).is_some() {
                continue;
            }
            if key.as_tagged(heap).get_as::<Symbol>(heap).is_none()
                && (!preserve_extras
                    || Object::own_property_ordinary(heap, &scope, old_owners, key).is_some())
            {
                continue;
            }
            let Some(property) =
                Object::own_property_ordinary(heap, &scope, module_object.erase(), key)
            else {
                continue;
            };
            if !matches!(
                Object::define_own_property(
                    vm,
                    heap,
                    state,
                    &scope,
                    storage.erase(),
                    key,
                    PartialDescriptor::from(&property.desc),
                    ShouldThrow::DontThrow,
                ),
                Ok(true)
            ) {
                return heap.known().false_object.get(heap).erase();
            }
        }

        let owners_name = vm.interner().intern_str(heap, &scope, "owners");
        let owners_name = scope.handle(owners_name.as_tagged(heap).erase());
        let contributions_name = vm.interner().intern_str(heap, &scope, "contributions");
        let contributions_name = scope.handle(contributions_name.as_tagged(heap).erase());
        for (key, value) in [(owners_name, owners), (contributions_name, contributions)] {
            if plan_write(heap, &scope, entry_object, key, value, &mut plan).is_none() {
                return heap.known().false_object.get(heap).erase();
            }
        }

        heap.exclusive(|heap| apply_plan(heap, &plan));
        module.as_tagged(heap).erase()
    })
}

fn apply_plan(heap: &Heap, plan: &TransitionPlan<'_>) {
    for (old_info, new_info) in &plan.infos {
        old_info
            .as_tagged(heap)
            .as_ref()
            .copy_from(heap, new_info.as_tagged(heap));
    }
    for (old, storage) in &plan.objects {
        let old_object = old.as_tagged(heap);
        let storage_object = storage.as_tagged(heap);
        let old_map = old_object.map(heap);
        let new_map = storage_object.map(heap);
        if !old_map.ptr_eq(new_map) {
            Prototype::shape_changed(heap, old_map, ShapeChange::INVALIDATE_ONLY);
        }
        let host = old_object.erase();
        old_object.header.set_map(heap, host, new_map);
        old_object.slots.set(heap, host, storage_object.slots(heap));
        old_object
            .elements
            .set(heap, host, storage_object.elements(heap));
        old_object
            .length
            .set(heap, host, Smi::new(storage_object.length() as i64));
    }
    for (target, offset, value) in &plan.writes {
        let target = target.as_tagged(heap);
        target
            .slot(heap, *offset)
            .set(heap, target.erase(), value.as_tagged(heap));
    }
}

fn plan_define_in_place<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    old: Handle<'s, Value>,
    new: Handle<'s, Value>,
    plan: &mut TransitionPlan<'s>,
) -> Option<Handle<'s, Value>> {
    let Some(old_object) = definable_object(heap, old.as_tagged(heap).erase()) else {
        return Some(new);
    };
    let Some(new_object) = definable_object(heap, new.as_tagged(heap).erase()) else {
        return Some(new);
    };
    if old_object.ptr_eq(new_object) {
        return Some(old);
    }
    let old_object = scope.handle(old_object);
    let new_object = scope.handle(new_object);
    if plan
        .objects
        .iter()
        .any(|(seen, _)| seen.as_tagged(heap).ptr_eq(old_object.as_tagged(heap)))
    {
        return Some(old);
    }

    let storage = Object::clone_shallow(heap, scope, new_object.erase())?;
    plan.objects.push((old_object, storage));

    let old_callable = old_object.as_tagged(heap).erase().is_callable(heap);
    let new_callable = new_object.as_tagged(heap).erase().is_callable(heap);
    if old_callable
        && new_callable
        && let (Some(old_info), Some(new_info)) = (
            old_object.as_tagged(heap).callable_info(heap),
            new_object.as_tagged(heap).callable_info(heap),
        )
    {
        let old_info = scope.handle(old_info);
        let new_info = scope.handle(new_info);
        if !old_info.as_tagged(heap).ptr_eq(new_info.as_tagged(heap))
            && !plan
                .infos
                .iter()
                .any(|(seen, _)| seen.as_tagged(heap).ptr_eq(old_info.as_tagged(heap)))
        {
            plan.infos.push((old_info, new_info));
        }
        storage.as_tagged(heap).slot(heap, 0).set(
            heap,
            storage.as_tagged(heap).erase(),
            old_info.as_tagged(heap).erase(),
        );
    }

    let keys = Object::own_property_keys(vm, heap, state, scope, new_object.erase()).ok()?;
    for key in keys {
        let Some(new_property) =
            Object::own_property_ordinary(heap, scope, new_object.erase(), key)
        else {
            continue;
        };
        if let PropertyDescriptor::Data {
            value: new_value,
            writable,
            enumerable,
            configurable,
        } = new_property.desc
            && let Some(OwnProperty {
                desc:
                    PropertyDescriptor::Data {
                        value: old_value, ..
                    },
                ..
            }) = Object::own_property_ordinary(heap, scope, old_object.erase(), key)
            && definable_object(heap, old_value.as_tagged(heap).erase()).is_some()
            && definable_object(heap, new_value.as_tagged(heap).erase()).is_some()
        {
            let canonical =
                plan_define_in_place(vm, heap, state, scope, old_value, new_value, plan)?;
            let desc = PartialDescriptor {
                value: Some(canonical),
                writable: Some(writable),
                enumerable: Some(enumerable),
                configurable: Some(configurable),
                ..PartialDescriptor::default()
            };
            if !Object::define_own_property(
                vm,
                heap,
                state,
                scope,
                storage.erase(),
                key,
                desc,
                ShouldThrow::DontThrow,
            )
            .ok()?
            {
                return None;
            }
        }
    }

    let old_kind = old_object.as_tagged(heap).map(heap).kind().kind();
    let new_kind = new_object.as_tagged(heap).map(heap).kind().kind();
    let old_keys = Object::own_property_keys(vm, heap, state, scope, old_object.erase()).ok()?;
    for key in old_keys {
        if Object::own_property_ordinary(heap, scope, new_object.erase(), key).is_some() {
            continue;
        }
        if (old_kind == ObjectKind::Array || new_kind == ObjectKind::Array)
            && matches!(
                key.as_tagged(heap).classify_key(heap),
                Some(Key::Element(_))
            )
        {
            continue;
        }
        let Some(property) = Object::own_property_ordinary(heap, scope, old_object.erase(), key)
        else {
            continue;
        };
        if !Object::define_own_property(
            vm,
            heap,
            state,
            scope,
            storage.erase(),
            key,
            PartialDescriptor::from(&property.desc),
            ShouldThrow::DontThrow,
        )
        .ok()?
        {
            return None;
        }
    }
    Some(old)
}

/// JS receivers with the shared object layout; proxies and strings use
/// different Rust layouts and cannot be traversed as ordinary objects.
fn definable_object<'a>(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<Tagged<'a, Object>> {
    let object = value.as_heap_object()?;
    let kind = object.map(heap).kind().kind();
    matches!(kind, ObjectKind::Object | ObjectKind::Array).then_some(object)
}

/// `Object.deleteProperty(object, name)`
/// Ordinary `[[Delete]]`.
fn object_delete_property<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let target = scope.handle(target.as_tagged(heap).erase());
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key);
        let ok = rt_try!(
            heap,
            Object::delete_property(vm, heap, state, target.erase(), key.erase())
        );
        let known = heap.known();
        if ok {
            known.true_object.get(heap).erase()
        } else {
            known.false_object.get(heap).erase()
        }
    })
}

fn install_accessor<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 0);
    let raw_key = args.get_handle(heap, 1);
    let closure = args.get_handle(heap, 2);
    let flags = args.get(heap, 3).to_i64().unwrap_or(0) as u32;
    state.handle_scope(|scope| {
        let key = rt_try!(heap, Object::to_property_key(vm, heap, state, raw_key));
        let key = scope.handle(key);
        if target.as_tagged(heap).is_primitive(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        if !closure.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::NotCallable);
        }
        let is_getter = flags & 1 != 0;
        let partial = PartialDescriptor {
            get: is_getter.then_some(closure),
            set: (!is_getter).then_some(closure),
            enumerable: Some(true),
            configurable: Some(true),
            ..PartialDescriptor::default()
        };
        rt_try!(
            heap,
            Object::define_property_or_throw(vm, heap, state, &scope, target, key.erase(), partial)
        );
        closure.as_tagged(heap)
    })
}

fn console_print<'a>(nctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    if args.is_empty() {
        println!();
        return vm.known().undefined.get(heap).erase();
    }
    let v = args.get_handle(heap, 0);
    state.handle_scope(|scope| {
        let text = match Object::to_string(vm, heap, state, v) {
            Ok(t) => {
                let t = scope.handle(t);
                t.as_tagged(heap)
                    .get_as::<DenseString>(heap)
                    .map(|s| s.to_rust_string(heap))
                    .unwrap_or_default()
            }
            Err(err) => return raise_runtime(heap, err),
        };
        println!("{text}");
        vm.known().undefined.get(heap).erase()
    })
}
