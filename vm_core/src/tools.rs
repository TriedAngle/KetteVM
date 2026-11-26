use std::path::PathBuf;

use bytecode::{FunctionId, SourceMode, disassemble, disassemble_function};

use crate::materialize::Materialize;
use crate::{
    Args, CallInfo, CallableInfoObject, Errors, FixedArray, Handle, HandleScope, HandleSlice, Heap,
    HostCtx, Map, MapInit, MapKind, NativeIndex, Object, PropertyDescriptor, Smi, SourceFunction,
    SourceUnit, StringInterner, StringRef, Tagged, ThreadState, VM, Value, VmError, WeakFixedArray,
    WellKnown, eval_global_context, ic::handler_kind_name, raise_runtime,
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
            "<eval>",
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
        let name = if args.len() > 2 {
            string_arg(heap, args, 2).unwrap_or_else(|| "<repl>".to_string())
        } else {
            "<repl>".to_string()
        };
        eval_source(
            vm,
            heap,
            state,
            &language,
            &source,
            bytecode::SourceMode::Repl,
            &name,
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
        let program = match (info.compile)(vm, &source, SourceMode::Script, vm.source_info()) {
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
            &path.display().to_string(),
        )
    }

    /// `KetteTools.sendTarget(path, start, end)` — resolve a method send
    /// from the live inline caches. Returns `[state, unit, hash, targets]`,
    /// where each target is `[name, unit, start, end, kind]`.
    pub fn send_target<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(path) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "sendTarget expects a path string",
            );
        };
        let Some(start) = args.get_handle(heap, 1).as_tagged(heap).to_i64() else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "sendTarget expects a byte start",
            );
        };
        let Some(end) = args.get_handle(heap, 2).as_tagged(heap).to_i64() else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "sendTarget expects a byte end",
            );
        };
        let start = start.max(0) as u32;
        let end = (end.max(start as i64)) as u32;
        let heap_ref: &Heap = heap;
        let result = vm.with_units(|units| query_send(units, heap_ref, &path, start, end));
        state.handle_scope(|scope| match result {
            Some(result) => {
                let state_text = vm.interner().intern_str(heap, &scope, &result.state);
                let unit_text = vm.interner().intern_str(heap, &scope, &result.unit);
                let hash_text =
                    vm.interner()
                        .intern_str(heap, &scope, &format!("{:016x}", result.hash));
                let mut targets = Vec::with_capacity(result.targets.len());
                for target in &result.targets {
                    let name = vm.interner().intern_str(heap, &scope, &target.name);
                    let unit = vm.interner().intern_str(heap, &scope, &target.unit);
                    let kind = vm.interner().intern_str(heap, &scope, &target.kind);
                    let values = [
                        name.as_tagged(heap).erase(),
                        unit.as_tagged(heap).erase(),
                        Smi::new(target.start as i64).into_tagged(),
                        Smi::new(target.end as i64).into_tagged(),
                        kind.as_tagged(heap).erase(),
                    ];
                    let staged = scope.stage(&values);
                    targets.push(scope.handle(heap.new_array(&scope, staged)));
                }
                let staged = scope.stage_handles(&targets);
                let targets = scope.handle(heap.new_array(&scope, staged));
                let values = [
                    state_text.as_tagged(heap).erase(),
                    unit_text.as_tagged(heap).erase(),
                    hash_text.as_tagged(heap).erase(),
                    targets.as_tagged(heap).erase(),
                ];
                let staged = scope.stage(&values);
                heap.new_array(&scope, staged).erase()
            }
            None => {
                let state_text = vm.interner().intern_str(heap, &scope, "missing");
                let empty = vm.interner().intern_str(heap, &scope, "");
                let targets = scope.handle(heap.new_array(&scope, HandleSlice::EMPTY));
                let values = [
                    state_text.as_tagged(heap).erase(),
                    empty.as_tagged(heap).erase(),
                    empty.as_tagged(heap).erase(),
                    targets.as_tagged(heap).erase(),
                ];
                let staged = scope.stage(&values);
                heap.new_array(&scope, staged).erase()
            }
        })
    }

    /// `KetteTools.unitText(name)` — latest source text for a compile unit.
    pub fn unit_text<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(name) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "unitText expects a unit name",
            );
        };
        let text = vm
            .with_units(|units| {
                units
                    .iter()
                    .rev()
                    .find(|unit| unit.name == name)
                    .map(|unit| unit.source.to_string())
            })
            .unwrap_or_default();
        state.handle_scope(|scope| {
            let text = vm.interner().intern_str(heap, &scope, &text);
            text.as_tagged(heap).erase()
        })
    }

    /// `KetteTools.unitHash(name)` — source hash of the latest unit.
    pub fn unit_hash<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(name) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "unitHash expects a unit name",
            );
        };
        let hash = vm
            .with_units(|units| {
                units
                    .iter()
                    .rev()
                    .find(|unit| unit.name == name)
                    .map(|unit| unit.hash)
            })
            .unwrap_or(0);
        state.handle_scope(|scope| {
            let text = vm
                .interner()
                .intern_str(heap, &scope, &format!("{hash:016x}"));
            text.as_tagged(heap).erase()
        })
    }

    /// `KetteTools.unitNames()` — names of the live compile units.
    pub fn unit_names<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let names = vm.with_units(|units| {
            units
                .iter()
                .map(|unit| unit.name.clone())
                .collect::<Vec<_>>()
        });
        state.handle_scope(|scope| {
            let mut entries = Vec::with_capacity(names.len());
            for name in &names {
                let text = vm.interner().intern_str(heap, &scope, name);
                entries.push(scope.handle(text.as_tagged(heap).erase()));
            }
            let staged = scope.stage_handles(&entries);
            heap.new_array(&scope, staged).erase()
        })
    }

    /// `KetteTools.moduleSource(path)` — all `mod path` declarations.
    pub fn module_source<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(path) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "moduleSource expects a module path",
            );
        };
        let targets = vm.with_units(|units| {
            let mut targets = Vec::new();
            for unit in units {
                for module in &unit.modules {
                    if module.path == path {
                        targets.push((unit.name.clone(), module.span.start, module.span.end));
                    }
                }
            }
            targets
        });
        state.handle_scope(|scope| source_targets(heap, &scope, vm, &targets))
    }

    /// `KetteTools.exportSource(path, name)` — all matching export declarations.
    pub fn export_source<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(path) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "exportSource expects a module path",
            );
        };
        let Some(name) = string_arg(heap, args, 1) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "exportSource expects an export name",
            );
        };
        let targets = vm.with_units(|units| {
            let mut targets = Vec::new();
            for unit in units {
                for export in &unit.exports {
                    if export.module == path && export.name == name {
                        targets.push((unit.name.clone(), export.span.start, export.span.end));
                    }
                }
            }
            targets
        });
        state.handle_scope(|scope| source_targets(heap, &scope, vm, &targets))
    }

    /// `KetteTools.importSource(unit, alias)` — module/name imported by a
    /// `use` alias. Returns `[module, name]`, or `null`.
    pub fn import_source<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(unit_name) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "importSource expects a unit name",
            );
        };
        let Some(alias) = string_arg(heap, args, 1) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "importSource expects an alias",
            );
        };
        let found = vm.with_units(|units| {
            units
                .iter()
                .rev()
                .find(|unit| unit.name == unit_name)
                .and_then(|unit| {
                    unit.imports
                        .iter()
                        .rev()
                        .find(|import| import.alias == alias)
                        .map(|import| (import.module.clone(), import.name.clone()))
                })
        });
        state.handle_scope(|scope| match found {
            Some((module, name)) => {
                let module = vm.interner().intern_str(heap, &scope, &module);
                let name = vm
                    .interner()
                    .intern_str(heap, &scope, name.as_deref().unwrap_or(""));
                let values = [module.as_tagged(heap).erase(), name.as_tagged(heap).erase()];
                let staged = scope.stage(&values);
                heap.new_array(&scope, staged).erase()
            }
            None => heap.known().null.get(heap).erase(),
        })
    }

    /// `KetteTools.setSourceInfo(mode)` — set the source policy for units
    /// compiled later. Accepted modes are "omit", "lazy", and "complete".
    pub fn set_source_info<'a>(
        ctx: HostCtx<'a>,
        _call: CallInfo<'_>,
        args: Args,
    ) -> Tagged<'a, Value> {
        let HostCtx {
            vm, heap, state, ..
        } = ctx;
        let Some(text) = string_arg(heap, args, 0) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                "setSourceInfo expects a mode string",
            );
        };
        let Some(info) = bytecode::SourceInfo::parse(&text) else {
            return Errors::raise_with(
                vm,
                heap,
                state,
                VmError::EvalError,
                &format!("unknown source info mode '{text}'"),
            );
        };
        vm.set_source_info(info);
        heap.known().undefined.get(heap).erase()
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
        natives: [NativeIndex; 16],
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

        let methods: [(&str, NativeIndex); 16] = [
            ("forceMinorGC", natives[0]),
            ("forceMajorGC", natives[1]),
            ("shutdown", natives[2]),
            ("eval", natives[3]),
            ("evalFile", natives[4]),
            ("bytecode", natives[5]),
            ("readFeedbackVector", natives[6]),
            ("evalRepl", natives[7]),
            ("setSourceInfo", natives[8]),
            ("sendTarget", natives[9]),
            ("unitText", natives[10]),
            ("unitNames", natives[11]),
            ("moduleSource", natives[12]),
            ("exportSource", natives[13]),
            ("unitHash", natives[14]),
            ("importSource", natives[15]),
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

struct QueryTarget {
    name: String,
    unit: String,
    start: u32,
    end: u32,
    kind: String,
}

struct QueryResult {
    state: &'static str,
    unit: String,
    hash: u64,
    targets: Vec<QueryTarget>,
}

fn query_send(
    units: &[SourceUnit],
    heap: &Heap,
    path: &str,
    start: u32,
    end: u32,
) -> Option<QueryResult> {
    let unit = units.iter().rev().find(|unit| unit.name == path)?;
    let mut function: Option<(usize, &SourceFunction)> = None;
    for (index, candidate) in unit.functions.iter().enumerate() {
        if candidate.span.contains(start, end)
            && function.map_or(true, |(_, best)| candidate.span.len() < best.span.len())
        {
            function = Some((index, candidate));
        }
    }
    let mut result = QueryResult {
        state: "unset",
        unit: unit.name.clone(),
        hash: unit.hash,
        targets: Vec::new(),
    };
    let Some((function_id, function)) = function else {
        return Some(result);
    };
    let Some(entry) = function
        .entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.kind,
                bytecode::SourceEntryKind::Send { .. }
                    | bytecode::SourceEntryKind::Get { .. }
                    | bytecode::SourceEntryKind::Call { .. }
            ) && entry.span.contains(start, end)
        })
        .min_by_key(|entry| entry.span.len())
    else {
        return Some(result);
    };
    let Some(callables) = &unit.callables else {
        return Some(result);
    };
    let word = callables.load();
    if word == 0 {
        return Some(result);
    }
    let value = unsafe { Tagged::<Value>::from_value_unchecked(Value::from_bits(word)) };
    let Some(callables) = value.get_as::<FixedArray>(heap) else {
        return Some(result);
    };
    let Some(info) = callables
        .at(heap, function_id)
        .get_as::<CallableInfoObject>(heap)
    else {
        return Some(result);
    };
    let Some(vector) = info.feedback(heap) else {
        return Some(result);
    };
    let (load, call) = match entry.kind {
        bytecode::SourceEntryKind::Send { load, call } => (Some(load), Some(call)),
        bytecode::SourceEntryKind::Get { load } => (Some(load), None),
        bytecode::SourceEntryKind::Call { call } => (None, Some(call)),
        _ => (None, None),
    };
    if let Some(call) = call.filter(|slot| *slot != u32::MAX) {
        let site = vector.read_pair(heap, call as usize);
        if site.state.is_weak_ptr() {
            if let Some(callee) = site.state.as_strong()
                && let Some(target) = query_target(units, heap, callee)
            {
                result.state = "mono";
                result.targets.push(target);
            }
        } else if site.state.is_strong_ptr()
            && !site.state.ptr_eq(heap.known().the_hole.get(heap).erase())
        {
            result.state = "mega";
        }
    }
    if result.targets.is_empty()
        && let Some(load) = load
        && let Some(value) = crate::ic::load_site_value(heap, vector, load as usize)
        && let Some(target) = query_target(units, heap, value)
    {
        result.state = "load";
        result.targets.push(target);
    }
    Some(result)
}

fn query_target(
    units: &[SourceUnit],
    heap: &Heap,
    value: Tagged<'_, Value>,
) -> Option<QueryTarget> {
    let object = value.as_heap_object()?;
    let info = object.callable_info(heap)?;
    let unit_id = info.source_unit.to_smi_unchecked().value();
    let start = info.source_start.to_smi_unchecked().value();
    let end = info.source_end.to_smi_unchecked().value();
    let name = StringRef::try_from(heap, info.name.get(heap))
        .map(|name| name.to_rust_string(heap))
        .unwrap_or_default();
    if unit_id <= 0 {
        return Some(QueryTarget {
            name,
            unit: String::new(),
            start: 0,
            end: 0,
            kind: "native".to_string(),
        });
    }
    let unit = units
        .iter()
        .find(|unit| unit.id == unit_id as u32)
        .map(|unit| unit.name.clone())
        .unwrap_or_default();
    Some(QueryTarget {
        name,
        unit,
        start: start.max(0) as u32,
        end: end.max(start) as u32,
        kind: "bytecode".to_string(),
    })
}

fn source_targets<'a>(
    heap: &'a mut Heap,
    scope: &HandleScope<'_>,
    vm: &VM,
    targets: &[(String, u32, u32)],
) -> Tagged<'a, Value> {
    let mut entries = Vec::with_capacity(targets.len());
    for (unit, start, end) in targets {
        let unit = vm.interner().intern_str(heap, scope, unit);
        let values = [
            unit.as_tagged(heap).erase(),
            Smi::new(*start as i64).into_tagged(),
            Smi::new(*end as i64).into_tagged(),
        ];
        let staged = scope.stage(&values);
        entries.push(scope.handle(heap.new_array(scope, staged)));
    }
    let staged = scope.stage_handles(&entries);
    heap.new_array(scope, staged).erase()
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
    name: &str,
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
    let source_info = vm.source_info();
    let program = match compile(vm, source, mode, source_info) {
        Ok(program) => program,
        Err(error) => {
            return Errors::raise_with(vm, heap, state, VmError::EvalError, &error.to_string());
        }
    };
    let unit_id = vm.next_unit_id();
    let mut unit =
        SourceUnit::from_program(unit_id, name.to_string(), source, source_info, &program);
    let result = state.handle_scope(|scope| {
        let context = eval_global_context(vm, heap, &scope);
        let (closure, callables) = Materialize::closure_vm(
            vm,
            heap,
            state,
            &scope,
            &program,
            runtime,
            context,
            Some(&unit),
        );
        let result = HostCtx::enter(
            vm,
            vm.runtime_by_id(runtime),
            &mut *heap,
            state,
            closure.erase(),
            HandleSlice::EMPTY,
            None,
        );
        if let Some(callables) = callables {
            unit.callables = Some(Box::new(unsafe {
                crate::RawCell::from_word(callables.raw().to_bits())
            }));
        }
        match result {
            Ok(value) => scope.handle(value),
            Err(thrown) => return raise_runtime(heap, thrown),
        }
        .as_tagged(heap)
    });
    vm.register_unit(unit);
    result
}
