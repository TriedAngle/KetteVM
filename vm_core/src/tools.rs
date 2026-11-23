use std::path::Path;

use crate::materialize::Materialize;
use crate::{
    Args, CallInfo, Errors, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind,
    NativeIndex, Object, PropertyDescriptor, StringInterner, StringRef, Tagged, ThreadState, VM,
    Value, VmError, WellKnown, eval_global_context, raise_runtime,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Termination {
    Shutdown,
}

pub struct KetteTools;

/// Stop the current execution: cancel live executions and mark the VM as
/// shut down. `os.exit` layers its exit status on top of this.
pub fn request_shutdown<'a>(vm: &VM, heap: &'a mut Heap, state: &ThreadState) -> Tagged<'a, Value> {
    heap.cancel_executions(&|| {});
    vm.note_shutdown();
    state.set_termination(Termination::Shutdown);
    state.set_pending_exception(vm.known().undefined.get(heap));
    vm.known().exception.get(heap).erase()
}

impl KetteTools {
    pub fn force_minor_gc<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        _args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx { vm, heap, .. } = ctx;
        heap.collect_minor();
        vm.known().undefined.get(heap).erase()
    }

    pub fn force_major_gc<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        _args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx { vm, heap, .. } = ctx;
        heap.collect();
        vm.known().undefined.get(heap).erase()
    }

    pub fn shutdown<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        request_shutdown(vm, heap, state)
    }

    /// Indirect eval: compile `source` as `language` in a fresh lexical
    /// scope. Free names resolve through the global environment; only
    /// explicit `globalThis` writes persist.
    pub fn eval<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        if args.len() < 2 {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "eval expects (language, source)",
            );
        }
        let Some(language) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "eval expects a language string",
            );
        };
        let Some(source) = string_arg(heap, args, 1) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "eval expects a source string",
            );
        };
        eval_source(
            vm,
            heap,
            state,
            &language,
            &source,
            bytecode::SourceMode::EvalGlobal,
        )
    }

    /// Read `path` and run it as an isolated file unit, inferring the
    /// language from the `.ktt` / `.js` extension.
    pub fn eval_file<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(path) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "evalFile expects a path string",
            );
        };
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    &format!("cannot read {path}: {error}"),
                );
            }
        };
        let language = match Path::new(&path).extension().and_then(|ext| ext.to_str()) {
            Some("ktt") => "kette",
            Some("js") => "js",
            _ => {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    &format!("evalFile expects a .ktt or .js file: {path}"),
                );
            }
        };
        eval_source(
            vm,
            heap,
            state,
            language,
            &source,
            bytecode::SourceMode::File,
        )
    }

    pub fn install(
        heap: &mut Heap,
        roots: &WellKnown,
        interner: &StringInterner,
        scope: &HandleScope<'_>,
        natives: [NativeIndex; 5],
    ) {
        let global = roots.global_object.get_handle(heap, scope);
        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: roots.object_prototype.get_handle(heap, scope).erase(),
            },
            scope,
        );
        let tools = scope.handle(heap.new_object(scope, map, HandleSlice::EMPTY));
        let primitives = scope.handle(heap.new_object(scope, map, HandleSlice::EMPTY));

        let methods: [(&str, NativeIndex); 5] = [
            ("forceMinorGC", natives[0]),
            ("forceMajorGC", natives[1]),
            ("shutdown", natives[2]),
            ("eval", natives[3]),
            ("evalFile", natives[4]),
        ];
        for (name, index) in methods {
            let method = Object::native_function(heap, scope, index, 0);
            let name_str = interner.intern_str(heap, scope, name);
            let method_name = scope.handle(name_str.as_tagged(heap));
            Object::define_own_property_ordinary(
                heap,
                scope,
                tools,
                method_name,
                PropertyDescriptor::method(method.erase()),
            )
            .expect("install tools method");
        }

        let primitives_name = interner.intern_str(heap, scope, "primitives");
        let primitives_name = scope.handle(primitives_name.as_tagged(heap));
        Object::define_own_property_ordinary(
            heap,
            scope,
            tools,
            primitives_name,
            PropertyDescriptor::data(primitives.erase()),
        )
        .expect("install KetteTools primitives");

        let name_str = interner.intern_str(heap, scope, "KetteTools");
        let name = scope.handle(name_str.as_tagged(heap));
        Object::define_own_property_ordinary(
            heap,
            scope,
            global,
            name,
            PropertyDescriptor::data(tools.erase()),
        )
        .expect("install KetteTools global");
    }

    /// The object holding VM/runtime primitive namespaces.
    pub fn primitives<'s>(
        heap: &mut Heap,
        interner: &StringInterner,
        scope: &'s HandleScope<'_>,
    ) -> Option<Handle<'s, Object>> {
        let global = heap.known().global_object.get_handle(heap, scope);
        let tools_name = interner.intern_str(heap, scope, "KetteTools");
        let tools_name = scope.handle(tools_name.as_tagged(heap).erase());
        let tools = Object::get_data_property(heap, scope, global.erase(), tools_name);
        let tools = scope.handle(tools.as_heap_object()?);
        let primitives_name = interner.intern_str(heap, scope, "primitives");
        let primitives_name = scope.handle(primitives_name.as_tagged(heap).erase());
        let primitives = Object::get_data_property(heap, scope, tools.erase(), primitives_name);
        Some(scope.handle(primitives.as_heap_object()?))
    }
}

fn string_arg(heap: &Heap, args: Args, index: usize) -> Option<String> {
    StringRef::try_from(heap, args.get_handle(heap, index).as_tagged(heap))
        .map(|text| text.to_rust_string(heap))
}

fn eval_source<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    language: &str,
    source: &str,
    mode: bytecode::SourceMode,
) -> Tagged<'a, Value> {
    let Some(info) = vm.eval_language(language) else {
        return Errors::raise_with(
            vm,
            heap,
            state,
            VmError::EvalError,
            &format!("unknown eval language '{language}'"),
        );
    };
    let compile = info.compile;
    let runtime = info.runtime;
    let program = match compile(vm, source, mode) {
        Ok(program) => program,
        Err(error) => {
            return Errors::raise_with(vm, heap, state, VmError::EvalError, &error.to_string());
        }
    };
    state.handle_scope(|scope| {
        let context = eval_global_context(vm, heap, &scope);
        let closure = Materialize::closure_vm(vm, heap, state, &scope, &program, runtime, context);
        match HostCtx::enter(
            vm,
            vm.runtime_by_id(runtime),
            heap,
            state,
            closure.erase(),
            HandleSlice::EMPTY,
            None,
        ) {
            Ok(value) => scope.handle(value),
            Err(thrown) => return raise_runtime(heap, thrown),
        }
        .as_tagged(heap)
    })
}
