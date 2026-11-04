//! Microbenchmark: reading a well-known root through `vm.known()` vs through
//! the executing runtime (`ctx.runtime::<JSRuntime>()`).
//!
//! Temporary harness for the `JsKnown` split; run with
//! `cargo run --release -p vm --example known_probe`.

use std::hint::black_box;
use std::time::Instant;

use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    Args, DefaultInterpreter, Handle, HostCtx, JSRuntime, NativeFn, Tagged, ThreadState, VM, Value,
    VmCore,
};

fn probe_core<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, .. } = ctx;
    vm.known().strings.length.get(heap).erase()
}

fn probe_js<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let HostCtx { heap, .. } = ctx;
    known.strings.done.get(heap).erase()
}

fn run(
    vm: &VM,
    heap: &mut vm::Heap,
    state: &ThreadState,
    runtime: &dyn vm::ErasedRuntimeState,
    f: NativeFn,
    n: usize,
) -> f64 {
    let f = black_box(f);
    let start = Instant::now();
    for _ in 0..n {
        let ctx = HostCtx::new(vm, runtime, heap, state);
        black_box(f(ctx, None, Args::EMPTY));
    }
    start.elapsed().as_secs_f64() * 1e9 / n as f64
}

fn main() {
    let vm = VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .expect("heap")
        .add::<VmCore>()
        .expect("vm core")
        .add::<JSRuntime>()
        .expect("js runtime");
    // the state outlives the thread: borrowed from the VM, not the thread
    let runtime: &dyn vm::ErasedRuntimeState = vm.runtime::<JSRuntime>();
    let mut thread = vm.attach();

    const N: usize = 20_000_000;
    for round in 0..5 {
        let core = {
            let (vm, heap, state) = thread.split();
            run(vm, heap, state, runtime, probe_core, N)
        };
        let js = {
            let (vm, heap, state) = thread.split();
            run(vm, heap, state, runtime, probe_js, N)
        };
        println!("round {round}: vm.known {core:6.2}  js known {js:6.2} ns/call");
    }
}
