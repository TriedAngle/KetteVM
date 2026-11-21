use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    DefaultInterpreter, JSRuntime, JavascriptCompiler, KetteCompiler, KetteRuntime, Thread, VM,
    VmCore,
};

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .add::<VmCore>()
        .unwrap()
        .add::<JSRuntime>()
        .unwrap()
        .add::<KetteRuntime>()
        .unwrap()
        .attach()
}

#[test]
fn kette_file_mode_does_not_leak_top_level_declarations() {
    let mut thread = test_vm();
    thread
        .eval_file::<KetteCompiler>("let file_local_canary = 1")
        .unwrap();
    thread
        .eval_file::<KetteCompiler>(
            "if globalThis.file_local_canary != undefined { throw \"leaked\" }",
        )
        .unwrap();
}

#[test]
fn js_file_mode_does_not_leak_top_level_declarations() {
    let mut thread = test_vm();
    thread
        .eval_file::<JavascriptCompiler>(
            "let file_local_canary = 1; var file_local_var = 2; function fileLocalFn() {}",
        )
        .unwrap();
    thread
        .eval_file::<JavascriptCompiler>(
            "if (globalThis.file_local_canary !== undefined \
             || globalThis.file_local_var !== undefined \
             || globalThis.fileLocalFn !== undefined) { throw new Error('leaked') }",
        )
        .unwrap();
}
