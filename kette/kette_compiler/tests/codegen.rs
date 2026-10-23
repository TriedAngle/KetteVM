//! Frontend-level regressions for kette lowering.

use bytecode::{FunctionId, SourceMode};
use kette_compiler::compile_kette;

fn script_register_count(src: &str) -> u32 {
    let program = compile_kette(src, SourceMode::Script).expect("compile");
    program.function(FunctionId::SCRIPT).register_count
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
