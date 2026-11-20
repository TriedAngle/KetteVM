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
}

impl EdgeVisitable for KetteRuntime {
    fn visit_edges(&self, _visitor: &mut dyn Visitor) {}
}

impl Runtime for KetteRuntime {
    const ID: RuntimeId = RuntimeId(2);

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), SetupError> {
        state.declare_globals = vm.register_native(declare_globals);
        state.install_accessor = vm.register_native(install_accessor);
        let print = vm.register_native(console_print);
        let mut thread = vm.attach();
        thread.handle_scope(|thread, scope| {
            let object_prototype = {
                let heap = thread.heap();
                heap.known()
                    .object_prototype
                    .get_handle(thread.heap(), &scope)
            };
            let map = thread.heap().allocate_handle::<Map>(
                MapInit {
                    kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                    value_slot_count: 0,
                    descriptors: &[],
                    prototype: object_prototype.erase(),
                },
                &scope,
            );
            let console = scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
            Object::install_method(thread, &scope, console, "print", print, 0);
            let console_name = thread.intern(&scope, "Console");
            let console_name = scope.handle(console_name.as_tagged(thread.heap()));
            let global = {
                let heap = thread.heap();
                heap.known().global_object.get_handle(thread.heap(), &scope)
            };
            // a bare global `print(x)`, same native as `Console.print`
            Object::install_method(thread, &scope, global, "print", print, 0);
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                global,
                console_name,
                PropertyDescriptor::data(console.erase()),
            )
            .expect("install console global");

            // `os.name` reports the host OS for platform-specific paths.
            let os = scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
            let name = scope.handle(thread.intern(&scope, "name").as_tagged(thread.heap()));
            let os_name = scope.handle(thread.intern(&scope, "os").as_tagged(thread.heap()));
            let value = thread.intern(&scope, std::env::consts::OS);
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                os,
                name,
                PropertyDescriptor::data(value.erase()),
            )
            .expect("install os name");
            Object::define_own_property_ordinary(
                thread.heap(),
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
