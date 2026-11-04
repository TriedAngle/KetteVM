use interpreter::BecomeInterpreter;
use js_runtime::JSRuntime;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm_core::Runtime;
use vm_core::{VM, VmCore};

fn main() {
    let vm = VM::new::<MarkSweep, BecomeInterpreter>(MarkSweepConfig::default())
        .expect("failed to create heap")
        .add::<VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .expect("failed to install js runtime");
    let mut thread = vm.attach();

    let mut parts = Vec::new();
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("kettevm: cannot read {path}: {e}");
            std::process::exit(1);
        });
        parts.push(src);
    }
    let joined = parts.join("\n");
    let program = match js_compiler::compile_js(
        &joined,
        js_compiler::SourceMode::Script,
        &vm.runtime::<JSRuntime>().ids,
    ) {
        Ok(program) => program,
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    };
    match thread.run_program(program, JSRuntime::ID) {
        Ok(_) => {
            if let Some(ex) = thread.take_pending_exception() {
                eprintln!("uncaught exception: {ex:?}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
