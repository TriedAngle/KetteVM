use std::path::PathBuf;

use bytecode::{FunctionId, SourceMode, disassemble, disassemble_function};

use crate::materialize::Materialize;
use crate::{
    Args, CallInfo, Errors, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind,
    NativeIndex, Object, PropertyDescriptor, Smi, StringInterner, StringRef, Tagged, ThreadState,
    VM, Value, VmError, WeakFixedArray, WellKnown, eval_global_context, ic::handler_kind_name,
    raise_runtime,
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

    /// REPL eval: compile in `Repl` mode so top-level declarations become
    /// globals of the running image.
    pub fn eval_repl<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        if args.len() < 2 {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "evalRepl expects (language, source)",
            );
        }
        let Some(language) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "evalRepl expects a language string",
            );
        };
        let Some(source) = string_arg(heap, args, 1) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "evalRepl expects a source string",
            );
        };
        eval_source(
            vm,
            heap,
            state,
            &language,
            &source,
            bytecode::SourceMode::Repl,
        )
    }

    /// Compile `(language, source[, function])` and return its bytecode.
    /// Without a function index, prints every function in the program.
    pub fn bytecode<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        if args.len() < 2 {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "bytecode expects (language, source[, function])",
            );
        }
        let Some(language) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "bytecode expects a language string",
            );
        };
        let Some(source) = string_arg(heap, args, 1) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "bytecode expects a source string",
            );
        };
        let Some(info) = vm.eval_language(&language) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                &format!("unknown eval language '{language}'"),
            );
        };
        let program = match (info.compile)(vm, &source, SourceMode::Script) {
            Ok(program) => program,
            Err(error) => {
                return Errors::raise_with(vm, heap, state, VmError::EvalError, &error.to_string());
            }
        };
        let text = if args.len() > 2 {
            let Some(index) = args.get_handle(heap, 2).as_tagged(heap).to_i64() else {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    "bytecode function index must be an integer",
                );
            };
            if index < 0 || index as usize >= program.len() {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    &format!(
                        "bytecode function index {index} out of range (0..{})",
                        program.len()
                    ),
                );
            }
            disassemble_function(&program, FunctionId(index as u32))
        } else {
            disassemble(&program)
        };
        state.handle_scope(|scope| {
            let text = vm.interner().intern_str(heap, &scope, &text);
            text.as_tagged(heap).erase()
        })
    }

    /// Read `path` and run it as an isolated file unit, inferring the
    /// language from the `.ktt` / `.js` extension. Relative paths resolve
    /// against the current process directory.
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
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&path))
                .unwrap_or(path)
        };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    &format!("cannot read {}: {error}", path.display()),
                );
            }
        };
        let language = match path.extension().and_then(|ext| ext.to_str()) {
            Some("ktt") => "kette",
            Some("js") => "js",
            _ => {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::EvalError,
                    &format!("evalFile expects a .ktt or .js file: {}", path.display()),
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

    /// Snapshot a function's inline-cache state into a normal array.
    ///
    /// One entry per feedback site: `[stateKind, state, handlerKind, handler]`.
    /// `state` is the cached map (mono), a copied normal array of
    /// `[map, handler, ...]` (poly), or null. `handler` is the decoded
    /// directive payload or an object identity. Internal objects (maps and
    /// handler objects) are reported by identity as a Smi, so the snapshot is
    /// always printable and never aliases the live feedback vector.
    pub fn read_feedback_vector<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        if args.is_empty() {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::InvalidOperandType,
                "readFeedbackVector expects a function",
            );
        }
        state.handle_scope(|scope| {
            let null = scope.handle(heap.known().null.get(heap).erase());
            let Some(object) = args
                .get_handle(heap, 0)
                .as_tagged(heap)
                .get_as::<Object>(heap)
            else {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::InvalidOperandType,
                    "readFeedbackVector expects a function",
                );
            };
            let Some(info) = object.callable_info(heap) else {
                return Errors::raise_with(
                    vm,
                    heap,
                    state,
                    VmError::InvalidOperandType,
                    "readFeedbackVector expects a function",
                );
            };
            let Some(feedback) = info.feedback(heap) else {
                return heap.new_array(&scope, HandleSlice::EMPTY).erase();
            };
            let vector = scope.handle(feedback);
            let sites = vector.as_tagged(heap).len() / 2;
            let mut entries = Vec::with_capacity(sites);
            for site in 0..sites {
                let pair = vector.as_tagged(heap).read_pair(heap, site * 2);
                let handler = pair.handler.as_strong();
                let (handler_kind, handler_handle) = if pair.handler.is_cleared()
                    || handler.is_some_and(|value| value.ptr_eq(heap.known().the_hole.get(heap)))
                {
                    ("none", null)
                } else if let Some(word) = pair.handler.to_i64() {
                    (
                        handler_kind_name(word & 0xff),
                        scope.handle(Smi::new(word >> 8).into_tagged()),
                    )
                } else if let Some(strong) = handler {
                    ("object", scope.handle(identity(strong)))
                } else {
                    ("dead", null)
                };
                let (state_kind, state_handle) = if pair.state.is_cleared() {
                    ("cleared", null)
                } else if pair.state.is_weak_ptr() {
                    match pair.state.as_strong() {
                        Some(map) => ("mono", scope.handle(identity(map.erase()))),
                        None => ("cleared", null),
                    }
                } else if let Some(strong) = pair.state.as_strong() {
                    if strong.ptr_eq(heap.known().the_hole.get(heap)) {
                        ("unset", null)
                    } else if strong.ptr_eq(heap.known().megamorphic_symbol.get(heap)) {
                        ("megamorphic", null)
                    } else if let Some(pairs) = strong.get_as::<WeakFixedArray>(heap) {
                        let staged = {
                            let mut values = Vec::with_capacity(pairs.len());
                            for i in 0..pairs.len() {
                                let entry = pairs.get(heap, i);
                                let value = if let Some(word) = entry.to_i64() {
                                    Smi::new(word >> 8).into_tagged()
                                } else if let Some(value) = entry.as_strong() {
                                    if value.ptr_eq(heap.known().the_hole.get(heap)) {
                                        null.as_tagged(heap)
                                    } else {
                                        identity(value)
                                    }
                                } else {
                                    null.as_tagged(heap)
                                };
                                values.push(value);
                            }
                            scope.stage(&values)
                        };
                        ("poly", scope.handle(heap.new_array(&scope, staged).erase()))
                    } else {
                        ("strong", scope.handle(identity(strong)))
                    }
                } else {
                    ("unset", null)
                };
                let state_kind = vm.interner().intern_str(heap, &scope, state_kind);
                let handler_kind = vm.interner().intern_str(heap, &scope, handler_kind);
                let words = [
                    state_kind.as_tagged(heap).erase(),
                    state_handle.as_tagged(heap).erase(),
                    handler_kind.as_tagged(heap).erase(),
                    handler_handle.as_tagged(heap).erase(),
                ];
                let staged = scope.stage(&words);
                entries.push(scope.handle(heap.new_array(&scope, staged)));
            }
            let staged = scope.stage_handles(&entries);
            heap.new_array(&scope, staged).erase()
        })
    }

    pub fn install(
        heap: &mut Heap,
        roots: &WellKnown,
        interner: &StringInterner,
        scope: &HandleScope<'_>,
        natives: [NativeIndex; 8],
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

        let methods: [(&str, NativeIndex); 8] = [
            ("forceMinorGC", natives[0]),
            ("forceMajorGC", natives[1]),
            ("shutdown", natives[2]),
            ("eval", natives[3]),
            ("evalFile", natives[4]),
            ("bytecode", natives[5]),
            ("readFeedbackVector", natives[6]),
            ("evalRepl", natives[7]),
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

fn identity(value: Tagged<'_, Value>) -> Tagged<'static, Value> {
    Smi::new(value.raw_addr() as i64).into_tagged()
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
        let result = HostCtx::enter(
            vm,
            vm.runtime_by_id(runtime),
            &mut *heap,
            state,
            closure.erase(),
            HandleSlice::EMPTY,
            None,
        );
        match result {
            Ok(value) => scope.handle(value),
            Err(thrown) => return raise_runtime(heap, thrown),
        }
        .as_tagged(heap)
    })
}
