use interpreter_become::BecomeInterpreter;
use js_runtime::JSRuntime;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use std::time::Instant;
use vm_core::{Smi, VM};

fn bench(name: &str, src: &str, iters: usize) {
    let mut times = Vec::new();
    for _ in 0..iters {
        let vm = VM::new::<MarkSweep, BecomeInterpreter>(MarkSweepConfig::default())
            .unwrap()
            .add::<JSRuntime>()
            .unwrap();
        let mut thread = vm.attach();
        let t = Instant::now();
        let _result = thread
            .run_source(src, js_compiler::compile_js, js_compiler::SourceMode::Script)
            .expect("runs");
        let dt = t.elapsed();
        if let Some(ex) = thread.take_pending_exception() {
            let heap = thread.heap();
            let tagged = unsafe { ex.assume_valid(heap) };
            let msg = tagged
                .get_as::<vm_core::DenseString>(heap)
                .map(|s| s.to_rust_string(heap))
                .unwrap_or_else(|| format!("{:?}", tagged));
            panic!("{name} threw: {msg}");
        }
        times.push(dt.as_secs_f64() * 1e3);
    }
    let best = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let all = times
        .iter()
        .map(|t| format!("{t:.0}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{name:10} best={best:8.1}ms  mean={mean:8.1}ms  [{all}]");
}

/// The official Octane Deltablue harness with a silent runner; the
/// script's completion value is the rounded Octane score (higher = faster).
const OCTANE_RUNNER: &str = r#"
var __runner = {
  NotifyStart: function (name) {},
  NotifyStep: function (name) {},
  NotifyError: function (name, error) { print("ERROR: " + name + ": " + error); },
  NotifyResult: function (name, score) {},
  NotifyScore: function (score) {}
};
BenchmarkSuite.RunSuites(__runner);
Math.round(100 * BenchmarkSuite.GeometricMean(BenchmarkSuite.scores));
"#;

fn deltablue_source() -> String {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../benchmarks/octane");
    let base = std::fs::read_to_string(format!("{root}/base.js")).expect("base.js");
    let suite =
        std::fs::read_to_string(format!("{root}/deltablue/deltablue.js")).expect("deltablue.js");
    format!("{base}\n{suite}\n{OCTANE_RUNNER}")
}

fn score_bench(name: &str, src: &str, iters: usize) {
    let mut scores = Vec::with_capacity(iters);
    for _ in 0..iters {
        let vm = VM::new::<MarkSweep, BecomeInterpreter>(MarkSweepConfig::default())
            .unwrap()
            .add::<JSRuntime>()
            .unwrap();
        let mut thread = vm.attach();
        let result = thread
            .run_source(src, js_compiler::compile_js, js_compiler::SourceMode::Script)
            .expect("runs");
        if let Some(ex) = thread.take_pending_exception() {
            let heap = thread.heap();
            let tagged = unsafe { ex.assume_valid(heap) };
            let msg = tagged
                .get_as::<vm_core::DenseString>(heap)
                .map(|s| s.to_rust_string(heap))
                .unwrap_or_else(|| format!("{:?}", tagged));
            panic!("{name} threw: {msg}");
        }
        let score = Smi::decode(result).expect("octane score is an integer");
        scores.push(score.value() as f64);
    }
    let best = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mean = scores.iter().sum::<f64>() / scores.len() as f64;
    let all = scores
        .iter()
        .map(|s| format!("{s:.0}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{name:10} best={best:8.1}  mean={mean:8.1}  [{all}]  (higher = faster)");
}

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../benchmarks");

    // mirror benchmarks/bench.sh: the SunSpider files are far too short
    // to time directly, so wrap and amplify; deltablue reports the
    // official Octane score instead (walltime is not meaningful there).
    let amp: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let wrap = |src: String| {
        format!(
            "function __bench_body() {{\n{src}\n}}\nfor (var __i = 0; __i < {amp}; __i++) __bench_body();\n"
        )
    };
    let fannkuch =
        wrap(std::fs::read_to_string(format!("{root}/fannkuch/fannkuch.js")).expect("src"));
    let nbody = wrap(std::fs::read_to_string(format!("{root}/nbody/nbody.js")).expect("src"));

    bench("fannkuch", &fannkuch, iters);
    score_bench("deltablue", &deltablue_source(), iters);
    bench("nbody", &nbody, iters);
}
