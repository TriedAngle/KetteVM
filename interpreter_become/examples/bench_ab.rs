//! A/B the old (`BecomeInterpreter`) and new (`NewInterpreter`) shells.
//!
//! - `fannkuch` / `nbody`: walltime, ABBA blocks cancelling linear drift
//!   (thermal/DVFS). `cargo run --release --example bench_ab -- <name> [blocks] [amp]`
//! - `deltablue`: the official Octane score (higher = faster), one
//!   interpreter per process so the runner's output stays attributable.
//!   `cargo run --release --example bench_ab -- deltablue <old|new>`

use interpreter_become::{BecomeInterpreter, NewInterpreter};
use js_runtime::JSRuntime;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use std::time::Instant;
use vm_core::{Interpreter, VM, materialize::Materialize};

/// Octane's harness objects are not part of the VM; the benchmark only
/// needs the constructors so the suite registration doesn't throw.
const DELTA_PRELUDE: &str = r#"
function BenchmarkSuite() {}
function Benchmark() {}
function alert(m) { throw "ALERT: " + m; }
"#;

fn source(name: &str, amp: usize) -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../benchmarks");
    let (prelude, body) = match name {
        "fannkuch" => (
            "",
            std::fs::read_to_string(format!("{root}/fannkuch/fannkuch.js"))
                .expect("fannkuch source"),
        ),
        "nbody" => {
            ("", std::fs::read_to_string(format!("{root}/nbody/nbody.js")).expect("nbody source"))
        }
        "deltablue" => {
            let src = std::fs::read_to_string(format!("{root}/octane/deltablue/deltablue.js"))
                .expect("deltablue source");
            (DELTA_PRELUDE, format!("{src}\ndeltaBlue();\n"))
        }
        _ => panic!("unknown benchmark {name}"),
    };
    // One compiled script that runs the workload `amp` times: compile and
    // materialize stay outside the timed region.
    format!(
        "{prelude}\nfunction __bench_body() {{\n{body}\n}}\n\
         for (var __i = 0; __i < {amp}; __i++) __bench_body();\n"
    )
}

/// One timed execution in a fresh VM. Returns milliseconds.
fn run_once<I: Interpreter>(label: &str, program: &bytecode::Program) -> f64 {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default())
        .unwrap()
        .add::<JSRuntime>()
        .unwrap();
    let mut thread = vm.attach();
    let (elapsed, result) = thread.handle_scope(|thread, scope| {
        let closure = Materialize::script(thread, &scope, program).expect("materialize");
        let start = Instant::now();
        let result = thread.execute(closure, &[]);
        (start.elapsed(), result)
    });
    if let Some(ex) = thread.take_pending_exception() {
        let heap = thread.heap();
        let tagged = unsafe { ex.assume_valid(heap) };
        let msg = tagged
            .get_as::<vm_core::DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_else(|| format!("{tagged:?}"));
        panic!("{label} threw: {msg}");
    }
    result.expect("benchmark must not throw");
    elapsed.as_secs_f64() * 1e3
}

fn report(name: &str, times: &mut [f64]) -> (f64, f64) {
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let best = times[0];
    let median = times[times.len() / 2];
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "  {name:8} best={best:8.2}ms  median={median:8.2}ms  mean={mean:8.2}ms  n={}",
        times.len()
    );
    (best, median)
}

/// The official Octane Deltablue harness: base.js + the suite + run.js.
/// The runner prints `Running DeltaBlue ...`, `DeltaBlue: <score>` and
/// `Score: <score>`; the score is the canonical metric (higher = faster).
fn deltablue_source() -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../benchmarks/octane");
    let base = std::fs::read_to_string(format!("{root}/base.js")).expect("base.js");
    let suite =
        std::fs::read_to_string(format!("{root}/deltablue/deltablue.js")).expect("deltablue.js");
    let run = std::fs::read_to_string(format!("{root}/run.js")).expect("run.js");
    format!("{base}\n{suite}\n{run}\n")
}

fn run_octane<I: Interpreter>(label: &str, program: &bytecode::Program) {
    println!("[{label}] octane deltablue");
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default())
        .unwrap()
        .add::<JSRuntime>()
        .unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let closure = Materialize::script(thread, &scope, program).expect("materialize");
        thread.execute(closure, &[])
    });
    if let Some(ex) = thread.take_pending_exception() {
        let heap = thread.heap();
        let tagged = unsafe { ex.assume_valid(heap) };
        let msg = tagged
            .get_as::<vm_core::DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_else(|| format!("{tagged:?}"));
        panic!("{label} threw: {msg}");
    }
    result.expect("benchmark must not throw");
}

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "fannkuch".into());
    if name == "deltablue" {
        let interp = std::env::args().nth(2).unwrap_or_else(|| "old".into());
        let program = js_compiler::compile_js(&deltablue_source(), bytecode::SourceMode::Script)
            .expect("compile");
        match interp.as_str() {
            "old" => run_octane::<BecomeInterpreter>("become", &program),
            "new" => run_octane::<NewInterpreter>("new", &program),
            other => panic!("unknown interpreter {other} (expected old|new)"),
        }
        return;
    }

    let blocks: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let amp: usize = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let src = source(&name, amp);
    let program = js_compiler::compile_js(&src, bytecode::SourceMode::Script).expect("compile");

    // warm up both shells (page in code, branch predictors)
    let _ = run_once::<BecomeInterpreter>("become", &program);
    let _ = run_once::<NewInterpreter>("new", &program);

    let mut old_t = Vec::with_capacity(blocks * 2);
    let mut new_t = Vec::with_capacity(blocks * 2);
    for _ in 0..blocks {
        old_t.push(run_once::<BecomeInterpreter>("become", &program));
        new_t.push(run_once::<NewInterpreter>("new", &program));
        new_t.push(run_once::<NewInterpreter>("new", &program));
        old_t.push(run_once::<BecomeInterpreter>("become", &program));
    }

    println!("{name} (amp={amp})");
    let (old_best, old_med) = report("become", &mut old_t);
    let (new_best, new_med) = report("new", &mut new_t);
    println!(
        "  speedup best={:5.2}x median={:5.2}x (old/new)",
        old_best / new_best,
        old_med / new_med
    );
}
