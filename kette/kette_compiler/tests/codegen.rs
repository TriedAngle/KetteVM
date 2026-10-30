//! Frontend-level regressions for kette lowering.

use bytecode::{Constant, Function, FunctionId, Opcode, Program, SourceMode};
use kette_compiler::compile_kette;

fn compile(src: &str, mode: SourceMode) -> Program {
    compile_kette(src, mode).expect("compile")
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

fn script_register_count(src: &str) -> u32 {
    compile(src, SourceMode::Script)
        .function(FunctionId::SCRIPT)
        .register_count
}

#[test]
fn array_literals_recycle_the_element_temp() {
    // ctx save + completion + array + one index temp
    assert_eq!(
        script_register_count("[1, 2, 3, 4, 5, 6, 7, 8];"),
        4,
        "array elements reuse one index register"
    );
}

#[test]
fn element_slots_recycle_the_key_temp() {
    assert_eq!(
        script_register_count("{ [0]: 1, [1]: 2, [2]: 3 };"),
        4,
        "element slots reuse one key register"
    );
}

#[test]
fn top_level_lets_are_global_properties() {
    let p = compile("let x = 1; x; x = 2;", SourceMode::Script);
    let f = script_fn(&p);
    let declarations = p.constants(f).iter().find_map(|constant| match constant {
        Constant::GlobalDeclarations(declarations) => Some(declarations),
        _ => None,
    });
    let declarations = declarations.expect("script declares its global bindings");
    assert_eq!(declarations.vars.len(), 1);
    assert_eq!(&*declarations.vars[0], b"x");
    assert!(contains(&p, f, Opcode::StoreGlobal));
    assert!(contains(&p, f, Opcode::LoadGlobal));
}

#[test]
fn repl_mode_shares_the_same_global_lowering() {
    let p = compile("let x = 1; x; x = 2;", SourceMode::Repl);
    let f = script_fn(&p);
    assert!(contains(&p, f, Opcode::StoreGlobal));
    assert!(contains(&p, f, Opcode::LoadGlobal));
}

#[test]
fn nested_lets_stay_local() {
    let p = compile("let f = { || let x = 1; x };", SourceMode::Script);
    let body = p.function(FunctionId(1));
    assert!(!contains(&p, body, Opcode::LoadGlobal));
    assert!(!contains(&p, body, Opcode::StoreGlobal));
}
