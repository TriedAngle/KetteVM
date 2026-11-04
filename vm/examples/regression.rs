//! Run the local regression suite in `tests/regression`.
//!
//! Usage:
//!   cargo run -p vm --example regression
//!   cargo run -p vm --example regression -- tests/regression/issue-1-*.js
//!   KETTEVM_GC_STRESS=1 cargo run -p vm --example regression
//!   KETTEVM_JOBS=4 cargo run -p vm --example regression
//!
//! Every `.js` file under the given paths (except `harness/`) runs in a
//! fresh VM with `tests/regression/harness/assert.js` prepended. Tests are
//! sharded across `KETTEVM_JOBS` workers (default: available parallelism).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{DefaultInterpreter, JSRuntime, JavascriptCompiler, VM, VmCore};

enum Outcome {
    Pass,
    Fail(String),
    Panic,
}

fn main() {
    let cwd = std::env::current_dir().unwrap_or_default();
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("vm crate has a parent")
        .to_path_buf();
    let cwd_regression = cwd.join("tests").join("regression");
    let regression = if cwd_regression.is_dir() {
        cwd_regression
    } else {
        manifest_root.join("tests").join("regression")
    };
    let root = regression
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest_root)
        .to_path_buf();
    let harness = std::fs::read_to_string(regression.join("harness").join("assert.js"))
        .expect("tests/regression/harness/assert.js");

    let mut roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        roots.push(regression);
    }

    let mut tests = Vec::new();
    for root in &roots {
        if root.is_dir() {
            collect(root, &mut tests);
        } else if root.extension().is_some_and(|e| e == "js") {
            tests.push(root.clone());
        }
    }
    tests.sort();

    if tests.is_empty() {
        eprintln!("regression: no tests found under {roots:?}");
        std::process::exit(1);
    }

    let jobs = std::env::var("KETTEVM_JOBS")
        .ok()
        .and_then(|jobs| jobs.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|jobs| jobs.get())
                .unwrap_or(1)
        });
    let jobs = jobs.max(1).min(tests.len());

    println!(
        "running {} regression test(s) with {} job(s)",
        tests.len(),
        jobs
    );

    let mut results: Vec<Option<Outcome>> = (0..tests.len()).map(|_| None).collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(jobs);
        for shard in 0..jobs {
            let tests = &tests;
            let harness = &harness;
            handles.push(scope.spawn(move || {
                let mut local = Vec::new();
                for index in (shard..tests.len()).step_by(jobs) {
                    let result = catch_unwind(AssertUnwindSafe(|| run_one(harness, &tests[index])));
                    let outcome = match result {
                        Ok(Ok(())) => Outcome::Pass,
                        Ok(Err(error)) => Outcome::Fail(error),
                        Err(_) => Outcome::Panic,
                    };
                    local.push((index, outcome));
                }
                local
            }));
        }
        for handle in handles {
            for (index, outcome) in handle.join().expect("regression worker") {
                results[index] = Some(outcome);
            }
        }
    });

    let mut failed = 0;
    for (test, result) in tests.iter().zip(results) {
        let name = test
            .strip_prefix(&root)
            .unwrap_or(test)
            .display()
            .to_string();
        match result.expect("every test has a result") {
            Outcome::Pass => println!("PASS {name}"),
            Outcome::Fail(error) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
            Outcome::Panic => {
                failed += 1;
                println!("PANIC {name}");
            }
        }
    }

    println!(
        "\n{} passed, {} failed, {} total",
        tests.len() - failed,
        failed,
        tests.len()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "harness") {
                continue;
            }
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "js") {
            out.push(path);
        }
    }
}

fn run_one(harness: &str, path: &Path) -> Result<(), String> {
    let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let program = format!("{harness}\n{source}");

    let vm = VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .map_err(|e| format!("{e:?}"))?
        .add::<VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .map_err(|e| format!("{e:?}"))?;
    if std::env::var_os("KETTEVM_GC_STRESS").is_some() {
        vm.arm_gc_stress();
    }
    let mut thread = vm.attach();
    let value = thread
        .eval::<JavascriptCompiler>(&program)
        .map_err(|e| e.to_string())?;

    let threw = {
        let heap = thread.heap();
        value == heap.known().exception.get(heap).raw()
    };
    if threw {
        let exception = thread
            .take_pending_exception()
            .map(|e| format!("{e:?}"))
            .unwrap_or_else(|| "uncaught exception".into());
        return Err(exception);
    }
    Ok(())
}
