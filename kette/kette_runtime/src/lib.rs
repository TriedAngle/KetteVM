use vm_core::intrinsics::declare_globals;
use vm_core::raise_runtime;
use vm_core::{
    Args, DenseString, EdgeVisitable, Handle, HandleSlice, HostCtx, Map, MapInit, MapKind,
    NativeIndex, Object, PropertyDescriptor, Runtime, RuntimeId, Tagged, VM, Value, Visitor,
    VmError,
};

/// The Kette runtime: its state is the runtime itself (one instance per VM).
#[derive(Default)]
pub struct KetteRuntime {
    /// Id of the native that installs the top-level declarations.
    pub declare_globals: NativeIndex,
}

impl EdgeVisitable for KetteRuntime {
    fn visit_edges(&self, _visitor: &mut dyn Visitor) {}
}

impl Runtime for KetteRuntime {
    const ID: RuntimeId = RuntimeId(2);

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), VmError> {
        state.declare_globals = vm.register_native(declare_globals);
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
            Object::install_method(thread, &scope, console, "print", print)?;
            let console_name = thread.intern(&scope, "Console");
            let console_name = scope.handle(console_name.as_tagged(thread.heap()));
            let global = {
                let heap = thread.heap();
                heap.known().global_object.get_handle(thread.heap(), &scope)
            };
            // a bare global `print(x)`, same native as `Console.print`
            Object::install_method(thread, &scope, global, "print", print)?;
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                global,
                console_name,
                PropertyDescriptor::data(console.erase()),
            )?;
            Ok(())
        })
    }
}

fn console_print<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
    if args.len() < 2 {
        println!();
        return vm.known().undefined.get(heap).erase();
    }
    let v = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let text = match Object::to_string(vm, heap, state, v) {
            Ok(Some(t)) => {
                let t = scope.handle(t);
                t.as_tagged(heap)
                    .get_as::<DenseString>(heap)
                    .map(|s| s.to_rust_string(heap))
                    .unwrap_or_default()
            }
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        println!("{text}");
        vm.known().undefined.get(heap).erase()
    })
}
