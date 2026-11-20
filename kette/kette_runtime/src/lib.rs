use vm_core::{
    Args, CallInfo, DenseString, EdgeVisitable, HandleSlice, HostCtx, Map, MapInit, MapKind,
    NativeIndex, Object, PartialDescriptor, PropertyDescriptor, Runtime, RuntimeId, SetupError,
    Tagged, VM, Value, Visitor, VmError, intrinsics::declare_globals, raise, raise_runtime, rt_try,
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
        let clone = state.clone;
        let print = vm.register_native(console_print);
        let is_type = vm.register_native(object_is_type);
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
                }
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
