//! Materialization: `bytecode::Program` → VM heap objects.
//!
//! Each compiled function becomes a `Code` (instruction bytes) wrapped in
//! a `CallableInfoObject` (language mode, constants, handler table);
//! closures reference them from the constants table. Constants are converted to heap values: interned strings,
//! `Float`s, oddball singletons, and child callable infos.
//!
//! The materializer is frontend-agnostic: it consumes the compiled program
//! function table and knows nothing about the language that produced it.

use crate::{
    CallableInfoInit, CallableInfoObject, Code, CodeInit, ContextObject, FeedbackVector,
    FeedbackVectorInit, FixedArray, FunctionKind, Handle, HandleScope, HandleSlice,
    HandlerEntryInit, HandlerTable, HandlerTableInit, Heap, Object, RuntimeId, ScopeInfo,
    ScopeInfoInit, Value, decode_wtf8, mode_from_strict,
};

use bytecode::{CallableKind, Constant, FunctionId, Program};

use crate::DenseString;
use crate::Smi;
use crate::StringData;
use crate::{Thread, ThreadState, VM};

/// Namespace for turning compiled programs into VM heap objects.
pub struct Materialize;

impl Materialize {
    /// Materialize a compiled program into a closure object (function map,
    /// empty context) ready for `Thread::execute`.
    pub fn script<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        program: &Program,
        runtime: RuntimeId,
    ) -> Handle<'s, Object> {
        let empty = {
            let (_vm, heap, _state) = thread.split();
            heap.known().empty_context.get_handle(heap, scope)
        };
        Self::closure(thread, scope, program, runtime, empty)
    }

    pub fn closure<'s, 'c>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        program: &Program,
        runtime: RuntimeId,
        context: Handle<'c, ContextObject>,
    ) -> Handle<'s, Object>
    where
        'c: 's,
    {
        let (vm, heap, state) = thread.split();
        Self::closure_vm(vm, heap, state, scope, program, runtime, context)
    }

    pub fn closure_vm<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        program: &Program,
        runtime: RuntimeId,
        context: Handle<'s, ContextObject>,
    ) -> Handle<'s, Object> {
        let _span = trace::info_span!("vm::materialize").entered();
        let mut infos: Vec<Option<Handle<'s, CallableInfoObject>>> =
            (0..program.len()).map(|_| None).collect();
        let info = materialize_function(
            vm,
            heap,
            state,
            scope,
            program,
            runtime,
            &mut infos,
            FunctionId::SCRIPT,
        );

        let map = heap.known().function_map.get_handle(heap, scope);
        let slots = scope.stage(&[
            info.as_tagged(heap).erase(),
            context.as_tagged(heap).erase(),
        ]);
        let object = heap.new_object(scope, map, slots).as_handle(scope);
        object
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_function<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    program: &Program,
    runtime: RuntimeId,
    infos: &mut [Option<Handle<'s, CallableInfoObject>>],
    fid: FunctionId,
) -> Handle<'s, CallableInfoObject> {
    if let Some(info) = infos[fid.index()] {
        return info;
    }
    let _span = trace::debug_span!("vm::materialize_function", fid = fid.0).entered();
    let function = program.function(fid);

    let constants = program.constants(function);
    let mut materialized: Vec<Handle<'s, Value>> = Vec::with_capacity(constants.len());
    for constant in constants {
        let value = match constant {
            Constant::String(bytes) => intern(heap, state, scope, vm, bytes).erase(),
            Constant::Smi(v) => scope.handle(Smi::new(*v)),
            Constant::Float(f) => scope.handle(heap.new_number(*f)),
            Constant::Callable(child) => {
                let info =
                    materialize_function(vm, heap, state, scope, program, runtime, infos, *child);
                info.erase()
            }
            Constant::ScopeInfo(layout) => {
                let names: Vec<Handle<'s, Value>> = layout
                    .locals
                    .iter()
                    .map(|local| intern(heap, state, scope, vm, &local.name).erase())
                    .collect();
                let names: HandleSlice<'s> = scope.stage_handles(&names);
                let infos: Vec<i64> = layout.locals.iter().map(|l| l.info as i64).collect();
                heap.allocate_handle::<ScopeInfo>(
                    ScopeInfoInit {
                        names,
                        infos: &infos,
                        flags: layout.flags,
                    },
                    scope,
                )
                .erase()
            }
            Constant::GlobalDeclarations(decls) => {
                let mut values: Vec<Handle<'s, Value>> = Vec::new();
                values.push(scope.handle(Smi::new(decls.vars.len() as i64)));
                for name in &decls.vars {
                    values.push(intern(heap, state, scope, vm, name).erase());
                }
                values.push(scope.handle(Smi::new(decls.funcs.len() as i64)));
                for decl in &decls.funcs {
                    values.push(intern(heap, state, scope, vm, &decl.name).erase());
                }
                for decl in &decls.funcs {
                    let info = materialize_function(
                        vm,
                        heap,
                        state,
                        scope,
                        program,
                        runtime,
                        infos,
                        decl.function,
                    );
                    values.push(info.erase());
                }
                heap.allocate_handle::<FixedArray>(scope.stage_handles(&values), scope)
                    .erase()
            }
            Constant::ObjectPrototype => heap
                .known()
                .object_prototype
                .get_handle(heap, scope)
                .erase(),
            Constant::FunctionPrototype => heap
                .known()
                .function_prototype
                .get_handle(heap, scope)
                .erase(),
            Constant::GlobalObject => heap.known().global_object.get_handle(heap, scope).erase(),
        };
        materialized.push(value);
    }

    let code = heap.allocate_handle::<Code>(
        CodeInit {
            runtime,
            bytes: program.code(function),
        },
        scope,
    );
    let constants = heap.allocate_handle::<FixedArray>(scope.stage_handles(&materialized), scope);
    let handlers = if program.handlers(function).is_empty() {
        None
    } else {
        let entries: Vec<HandlerEntryInit> = program
            .handlers(function)
            .iter()
            .map(|h| HandlerEntryInit::new(h.try_start, h.try_end, h.handler_pc))
            .collect();
        Some(heap.allocate_handle::<HandlerTable>(HandlerTableInit { entries: &entries }, scope))
    };

    let info = heap.allocate_handle::<CallableInfoObject>(
        CallableInfoInit::Bytecode {
            code,
            constants,
            register_count: function.register_count as usize,
            handlers,
            expected_slots: function.expected_slots as usize,
            mode: mode_from_strict(function.strict),
        },
        scope,
    );
    if function.feedback_count > 0 {
        let feedback = heap.allocate_handle::<FeedbackVector>(
            FeedbackVectorInit {
                length: function.feedback_count as usize,
            },
            scope,
        );
        info.as_tagged(heap).set_feedback(heap, feedback);
    }
    let name: Option<Handle<'s, DenseString>> = program
        .name(function)
        .map(|name| intern(heap, state, scope, vm, name));
    let kind = match function.kind {
        CallableKind::Normal => FunctionKind::Normal,
        CallableKind::Generator => FunctionKind::Generator,
        CallableKind::Arrow => FunctionKind::Arrow,
        CallableKind::Method => FunctionKind::Method,
        CallableKind::Getter => FunctionKind::Getter,
        CallableKind::Setter => FunctionKind::Setter,
        CallableKind::BaseClassConstructor => FunctionKind::BaseClassConstructor,
        CallableKind::DerivedClassConstructor => FunctionKind::DerivedClassConstructor,
        CallableKind::DefaultDerivedConstructor => FunctionKind::DefaultDerivedConstructor,
    };
    info.as_tagged(heap).set_metadata_full(
        heap,
        name.map(|h| h.as_tagged(heap).erase()),
        function.arity as usize,
        function.length as usize,
        kind,
    );
    infos[fid.index()] = Some(info);
    info
}

fn intern<'s>(
    heap: &mut Heap,
    _state: &ThreadState,
    scope: &'s HandleScope<'_>,
    vm: &VM,
    s: &[u8],
) -> Handle<'s, DenseString> {
    let units = decode_wtf8(s).expect("frontends produce valid WTF-8 string constants");
    vm.interner().intern(heap, scope, StringData::Utf16(&units))
}
