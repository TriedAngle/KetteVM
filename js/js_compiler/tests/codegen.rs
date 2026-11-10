//! Frontend tests at the IR level: compile snippets through `compile_js`
//! and assert on function metadata and the emitted instruction stream.
//! (Runtime behavior is covered by the `vm` test suite.)

use bytecode::Opcode;
use bytecode::{FrontendErrorKind, Function, FunctionId, Program, SourceMode};
use js_compiler::compile_js;
use js_compiler::native::NativeIds;
use vm_core::NativeIndex;

/// Ids are placeholders: this tool never executes the program.
fn native_ids() -> NativeIds {
    NativeIds {
        get_iterator: NativeIndex(0),
        iterator_next: NativeIndex(0),
        iterator_done: NativeIndex(0),
        iterator_value: NativeIndex(0),
        has_property: NativeIndex(0),
        copy_data_properties: NativeIndex(0),
        create_private_name: NativeIndex(0),
        private_get: NativeIndex(0),
        private_set: NativeIndex(0),
        private_in: NativeIndex(0),
        set_class_fields: NativeIndex(0),
        init_instance_fields: NativeIndex(0),
        require_object_coercible: NativeIndex(0),
        delete_property_operator: NativeIndex(0),
        delete_identifier_sloppy: NativeIndex(0),
        delete_super_property: NativeIndex(0),
        for_in_enumerate: NativeIndex(0),
        for_in_next: NativeIndex(0),
        set_function_name: NativeIndex(0),
        install_accessor: NativeIndex(0),
        define_own_property: NativeIndex(0),
        set_prototype: NativeIndex(0),
        throw_if_not_constructor_or_null: NativeIndex(0),
        throw_if_not_object_or_null: NativeIndex(0),
        throw_super_not_called_if_hole: NativeIndex(0),
        throw_super_already_called_if_not_hole: NativeIndex(0),
        construct_super: NativeIndex(0),
        construct_super_all_args: NativeIndex(0),
        construct_super_via: NativeIndex(0),
        load_dynamic_name: NativeIndex(0),
        store_dynamic_name: NativeIndex(0),
        declare_eval_var: NativeIndex(0),
        declare_eval_function: NativeIndex(0),
        resolve_direct_eval: NativeIndex(0),
        store_repl_global_init: NativeIndex(0),
        validate_eval_declarations: NativeIndex(0),
        declare_globals: NativeIndex(0),
        throw_const_assign: NativeIndex(0),
        create_rest_parameter: NativeIndex(0),
        create_arguments: NativeIndex(0),
        super_get_property: NativeIndex(0),
        super_set_property: NativeIndex(0),
    }
}

fn compile(src: &str, mode: SourceMode) -> Program {
    compile_js(src, mode, &native_ids()).expect("compile")
}

fn script_fn(program: &Program) -> &Function {
    program.function(FunctionId::SCRIPT)
}

fn ops(program: &Program, f: &Function) -> Vec<Opcode> {
    let code = program.code(f);
    let mut out = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let (op, _, next) = bytecode::decode(code, pc);
        out.push(op);
        pc = next;
    }
    out
}

fn contains(program: &Program, f: &Function, op: Opcode) -> bool {
    ops(program, f).contains(&op)
}

#[test]
fn function_metadata() {
    let p = compile(
        "function add(a, b) { return a + b; }
         function rest(a, b = 1, ...c) {}",
        SourceMode::Script,
    );
    let add = p.function(FunctionId(1));
    assert_eq!(add.arity, 2);
    assert_eq!(add.length, 2);
    assert_eq!(p.name(add), Some(&b"add"[..]));
    let rest = p.function(FunctionId(2));
    assert_eq!(rest.arity, 3, "patterns and rest count one slot each");
    assert_eq!(rest.length, 1, "length stops at the first non-simple param");
}

#[test]
fn script_carries_a_completion_register() {
    let p = compile("1 + 2;", SourceMode::Script);
    let f = script_fn(&p);
    // locals(0) + ctx save + completion = 3 fixed slots below the temps
    assert!(f.register_count >= 3);
    assert!(contains(&p, f, Opcode::Store));
}

#[test]
fn unsupported_constructs_are_compile_errors() {
    for (src, feature) in [
        ("1n + 1n;", "BigInt literals"),
        ("try {} finally {}", "finally blocks"),
        ("a ?? b;", "nullish coalescing"),
    ] {
        let err = compile_js(src, SourceMode::Script, &native_ids()).expect_err(src);
        assert_eq!(err.kind, FrontendErrorKind::Compile, "{src}");
        assert!(err.message.contains(feature), "{src} -> {err}");
    }
}

#[test]
fn syntax_errors_are_syntax_errors() {
    let err = compile_js("function {", SourceMode::Script, &native_ids()).expect_err("must fail");
    assert_eq!(err.kind, FrontendErrorKind::Syntax);
}

#[test]
fn script_mode_top_level_vars_are_global_properties() {
    let p = compile("var x = 1; x = 2;", SourceMode::Script);
    let f = script_fn(&p);
    assert!(
        contains(&p, f, Opcode::StoreGlobal),
        "script var bindings live on the global object"
    );
}

#[test]
fn repl_mode_top_level_vars_are_global_properties() {
    let p = compile("var x = 1; let y = 2; function f() {}", SourceMode::Repl);
    let f = script_fn(&p);
    assert!(
        contains(&p, f, Opcode::CallNative),
        "global declarations use the runtime declaration helpers"
    );
    assert!(
        contains(&p, f, Opcode::LoadGlobal) || contains(&p, f, Opcode::StoreGlobal),
        "REPL root bindings resolve through the global object"
    );
}

// TODO(oxc): Oxc hoists the sloppy block function's synthesized Annex B
// var before checking the enclosing lexical declaration, so this valid
// source is rejected. Remove `ignore` once Oxc handles the ordering.
#[test]
#[ignore = "Oxc rejects valid Annex B when a block function precedes an enclosing let"]
fn annex_b_block_function_before_enclosing_lexical() {
    compile(
        "function f() { { function g() {} } let g = 2; return g; }",
        SourceMode::Script,
    );
    compile("{ function g() {} } let g = 2;", SourceMode::Script);
}

#[test]
fn eval_mode_free_names_are_dynamic_lookups() {
    let p = compile("undeclared_name;", SourceMode::Eval);
    let f = script_fn(&p);
    assert!(contains(&p, f, Opcode::CallNative));
}

#[test]
fn eval_mode_same_code_resolves_normally() {
    // in Script mode the same free name is a plain global load, not a
    // runtime chain walk
    let p = compile("undeclared_name;", SourceMode::Script);
    let f = script_fn(&p);
    assert!(contains(&p, f, Opcode::LoadGlobal));
}

#[test]
fn strict_directive_propagates() {
    let p = compile("\"use strict\"; function f() {}", SourceMode::Script);
    assert!(script_fn(&p).strict);
    assert!(p.function(FunctionId(1)).strict);
    let p = compile("function f() {}", SourceMode::Script);
    assert!(!script_fn(&p).strict);
}

#[test]
fn hoisted_function_declarations_are_declared_in_the_prologue() {
    // the closure creation must precede any body code
    let p = compile(
        "var r = f(); function f() { return 1; }",
        SourceMode::Script,
    );
    let f = script_fn(&p);
    let declare = position(&p, f, Opcode::CallNative);
    let first_call = position(&p, f, Opcode::CallFunction0);
    assert!(declare < first_call, "globals declared before the call");
}

#[test]
fn class_declarations_compile_with_synthesized_members() {
    let p = compile(
        "class A { #x = 1; get v() { return this.#x; } }
         new A().v;",
        SourceMode::Script,
    );
    // script + ctor + field initializer + getter = 4
    assert_eq!(p.len(), 4);
}

fn position(program: &Program, f: &Function, op: Opcode) -> usize {
    let code = program.code(f);
    let mut pc = 0;
    while pc < code.len() {
        let (found, _, next) = bytecode::decode(code, pc);
        if found == op {
            return pc;
        }
        pc = next;
    }
    panic!("opcode {op:?} not found");
}
