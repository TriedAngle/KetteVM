use interpreter_become::{BecomeInterpreter, NewInterpreter};
use js_runtime::JSRuntime;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use std::time::Instant;
use vm_core::{Interpreter, VM};

/// One timed run of `src` on interpreter `I`.
fn run_once<I: Interpreter>(src: &str) -> f64 {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default())
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
        panic!("threw: {msg}");
    }
    dt.as_secs_f64() * 1e3
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn report(name: &str, times: &mut Vec<f64>) {
    let best = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let med = median(times);
    println!(
        "{name:10} best={best:7.1}ms  median={med:7.1}ms  mean={mean:7.1}ms  n={}",
        times.len()
    );
}

fn main() {
    let blocks: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../benchmarks");
    let amp: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let src = std::fs::read_to_string(format!("{root}/fannkuch/fannkuch.js")).expect("src");
    let fannkuch = format!(
        "function __bench_body() {{\n{src}\n}}\nfor (var __i = 0; __i < {amp}; __i++) __bench_body();\n"
    );

    // warmup both shells (page in code, spin the core to steady state)
    let _ = run_once::<BecomeInterpreter>(&fannkuch);
    let _ = run_once::<NewInterpreter>(&fannkuch);

    // ABBA blocks: each block runs become,new,new,become — linear drift
    // (thermal, DVFS) cancels between the A and B halves of a block
    let mut old_t = Vec::new();
    let mut new_t = Vec::new();
    for _ in 0..blocks {
        old_t.push(run_once::<BecomeInterpreter>(&fannkuch));
        new_t.push(run_once::<NewInterpreter>(&fannkuch));
        new_t.push(run_once::<NewInterpreter>(&fannkuch));
        old_t.push(run_once::<BecomeInterpreter>(&fannkuch));
    }
    report("become", &mut old_t);
    report("new", &mut new_t);
}
