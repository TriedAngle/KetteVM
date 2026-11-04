//! Tiny JS runner for comparing the interpreter shells.
//!
//!     cargo run --release -p vm --example bench_js -- files...

use bytecode::SourceMode;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{Compiler, DefaultInterpreter, Interpreter, JSRuntime, JavascriptCompiler, Runtime, VM};

fn run<I: Interpreter>(src: &str) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default())
        .expect("heap")
        .add::<vm::VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .expect("js runtime");
    let program = JavascriptCompiler::compile(&vm, src, SourceMode::Script).expect("compile");
    let mut thread = vm.attach();
    thread
        .run_program(program, JSRuntime::ID)
        .expect("script runs");
    if let Some(ex) = thread.take_pending_exception() {
        eprintln!("uncaught exception: {ex:?}");
        std::process::exit(1);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().expect("script path");
    // Keep the old `match`/`become` spelling accepted so stored benchmark
    // wrappers still work; the backend is selected at build time.
    let files: Vec<String> = if matches!(
        first.as_str(),
        "match" | "become" | "become_match" | "default"
    ) {
        args.collect()
    } else {
        std::iter::once(first).chain(args).collect()
    };
    let mut src = String::new();
    for path in files {
        src.push_str(&std::fs::read_to_string(&path).expect("read script"));
        src.push('\n');
    }
    run::<DefaultInterpreter>(&src);
}
