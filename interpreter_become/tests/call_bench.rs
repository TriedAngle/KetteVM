//! Microbenchmarks for the calling paths of the two interpreters in
//! this crate: the shipping `BecomeInterpreter` (lib.rs) and the
//! `#[handler]` sketch (`NewInterpreter`, new.rs).
//!
//! The programs are hand-assembled (like tests/new_interpreter.rs)
//! because the sketch only implements a subset of opcodes; both
//! interpreters run the *same* bytes through the same VM plumbing, so
//! the numbers isolate interpreter dispatch + calling code. Jump
//! offsets are ip-relative (target = instruction start + offset).
//!
//! Timed benchmark (from this directory):
//!     cargo test --release --test call_bench -- --ignored --nocapture
//! Correctness parity (both interpreters, expected results) runs with
//! plain `cargo test --test call_bench`.

use bytecode::{Opcode, emit};
use interpreter_become::{BecomeInterpreter, NewInterpreter};
use mark_sweep::{MarkSweep, MarkSweepConfig};
use std::time::Instant;
use vm_core::{
    CallableInfoInit, CallableInfoObject, FixedArray, FixedByteArray, Handle, HandleScope,
    HandlerEntryInit, HandlerTable, HandlerTableInit, Heap, Interpreter, Object, ObjectSlotsInit,
    Smi, Tagged, Thread, VM, Value, new_feedback_vector,
};

/// Outer-loop iterations per timed run.
const ITERS: i64 = 2_000_000;
/// Timed runs per interpreter (interleaved; best is reported).
const REPS: usize = 7;

fn smi(v: i64) -> Value {
    Smi::new(v).encode()
}

/// A `Normal`-kind bytecode callable over a hand-assembled program;
/// `feedback_slots` attaches a vector so call sites can go monomorphic,
/// `handlers` attaches an exception-handler table so catch ranges work.
fn callable<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    program: &[u8],
    register_count: usize,
    feedback_slots: usize,
    handlers: &[HandlerEntryInit],
) -> Handle<'s, Object> {
    let bytecode = thread
        .heap()
        .allocate_handle::<FixedByteArray>(program, scope);
    let constants = {
        let empty: &[Tagged<'_, Value>] = &[];
        let staged = scope.stage(empty);
        thread.heap().allocate_handle::<FixedArray>(staged, scope)
    };
    let handlers = (!handlers.is_empty()).then(|| {
        thread
            .heap()
            .allocate_handle::<HandlerTable>(HandlerTableInit { entries: handlers }, scope)
    });
    let info = thread.heap().allocate_handle::<CallableInfoObject>(
        CallableInfoInit {
            expected_slots: 0,
            bytecode,
            constants,
            register_count,
            handlers,
        },
        scope,
    );
    if let Some(vector) = new_feedback_vector(thread.heap(), scope, feedback_slots) {
        let heap: &Heap = thread.heap();
        info.as_tagged(heap).set_feedback(heap, vector);
    }
    let map = thread.heap().known().function_map;
    let values = {
        let heap = thread.heap();
        scope.stage(&[
            info.as_tagged(heap).erase(),
            heap.known().empty_context.as_tagged(heap).erase(),
        ])
    };
    let empty_fixed_array = thread.heap().known().empty_fixed_array;
    let obj = thread.heap().allocate_object(
        scope,
        ObjectSlotsInit {
            map,
            values,
            elements: empty_fixed_array,
            length: 0,
        },
    );
    obj.as_handle(scope)
}

struct Scenario {
    name: &'static str,
    /// Callee bytecode (called every outer iteration; may be unused).
    inner: Vec<u8>,
    inner_regs: usize,
    /// Driver bytecode: a countdown loop around the call.
    outer: Vec<u8>,
    outer_regs: usize,
    /// Feedback-vector slots attached to the outer callable.
    feedback: usize,
    /// Exception-handler ranges attached to the outer callable
    /// (`HandlerEntryInit { try_start, try_end, handler_pc }`).
    handler_entries: Vec<HandlerEntryInit>,
    /// Whether the callee word is passed as outer argument 0.
    has_callee: bool,
    /// Fixed outer arguments between the (optional) callee and the
    /// loop counter/step that `args` appends.
    const_args: Vec<i64>,
    /// Expected smi result.
    expect: i64,
}

impl Scenario {
    fn args(&self, inner: Value, iters: i64) -> Vec<Value> {
        let mut v = Vec::with_capacity(self.const_args.len() + 3);
        if self.has_callee {
            v.push(inner);
        }
        v.extend(self.const_args.iter().map(|&n| smi(n)));
        v.push(smi(iters));
        v.push(smi(-1));
        v
    }
}

/// `inner(recv, a): return a`
fn trivial_inner() -> Vec<u8> {
    let mut inner = Vec::new();
    emit(&mut inner, Opcode::Load, &[1]);
    emit(&mut inner, Opcode::Return, &[]);
    inner
}

/// Countdown loop with no calls: `n += step; while (n) ...; return n`.
/// Measures raw dispatch/loop overhead as the no-call baseline.
fn loop_baseline(jump_loop: bool) -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::Load, &[0]); // 0
    emit(&mut outer, Opcode::Add, &[1]); // 2
    emit(&mut outer, Opcode::Store, &[0]); // 4
    emit(&mut outer, Opcode::Load, &[0]); // 6
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 8 -> 12
    if jump_loop {
        emit(&mut outer, Opcode::JumpLoop, &[(-10i32) as u32]); // 10 -> 0
    } else {
        emit(&mut outer, Opcode::Jump, &[(-10i32) as u32]); // 10 -> 0
    }
    emit(&mut outer, Opcode::Load, &[0]); // 12
    emit(&mut outer, Opcode::Return, &[]); // 14
    Scenario {
        name: if jump_loop {
            "loop_jumploop"
        } else {
            "loop_baseline"
        },
        inner: Vec::new(),
        inner_regs: 0,
        outer,
        outer_regs: 2,
        feedback: 0,
        handler_entries: Vec::new(),
        has_callee: false,
        const_args: vec![],
        expect: 0,
    }
}

/// `CallFunction1` (scattered args) behind a call IC: first execution
/// records the callee, the remaining `iters - 1` hit the monomorphic
/// entry. Same shape as tests/new_interpreter.rs.
fn call_function1_ic() -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::CallFunction1, &[0, 1, 0]); // 0
    emit(&mut outer, Opcode::Store, &[1]); // 4
    emit(&mut outer, Opcode::Load, &[2]); // 6
    emit(&mut outer, Opcode::Add, &[3]); // 8
    emit(&mut outer, Opcode::Store, &[2]); // 10
    emit(&mut outer, Opcode::Load, &[2]); // 12
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 14 -> 18
    emit(&mut outer, Opcode::Jump, &[(-16i32) as u32]); // 16 -> 0
    emit(&mut outer, Opcode::Load, &[1]); // 18
    emit(&mut outer, Opcode::Return, &[]); // 20
    Scenario {
        name: "call_function1_ic",
        inner: trivial_inner(),
        inner_regs: 2,
        outer,
        outer_regs: 4,
        feedback: 2,
        handler_entries: Vec::new(),
        has_callee: true,
        const_args: vec![41],
        expect: 41,
    }
}

/// `CallMethod1` (scattered receiver + arg) behind a call IC.
fn call_method1_ic() -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::CallMethod1, &[0, 1, 2, 0]); // 0
    emit(&mut outer, Opcode::Store, &[5]); // 5
    emit(&mut outer, Opcode::Load, &[3]); // 7
    emit(&mut outer, Opcode::Add, &[4]); // 9
    emit(&mut outer, Opcode::Store, &[3]); // 11
    emit(&mut outer, Opcode::Load, &[3]); // 13
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 15 -> 19
    emit(&mut outer, Opcode::Jump, &[(-17i32) as u32]); // 17 -> 0
    emit(&mut outer, Opcode::Load, &[5]); // 19
    emit(&mut outer, Opcode::Return, &[]); // 21
    Scenario {
        name: "call_method1_ic",
        inner: trivial_inner(),
        inner_regs: 2,
        outer,
        outer_regs: 6,
        feedback: 2,
        handler_entries: Vec::new(),
        has_callee: true,
        const_args: vec![7, 41],
        expect: 41,
    }
}

/// `Call` (contiguous `[receiver, args..]` window) behind a call IC:
/// the window covers r1..r2, so the counter lives in r3/r4.
fn call_window_ic() -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::Call, &[0, 2, 2, 0]); // 0
    emit(&mut outer, Opcode::Store, &[5]); // 5
    emit(&mut outer, Opcode::Load, &[3]); // 7
    emit(&mut outer, Opcode::Add, &[4]); // 9
    emit(&mut outer, Opcode::Store, &[3]); // 11
    emit(&mut outer, Opcode::Load, &[3]); // 13
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 15 -> 19
    emit(&mut outer, Opcode::Jump, &[(-17i32) as u32]); // 17 -> 0
    emit(&mut outer, Opcode::Load, &[5]); // 19
    emit(&mut outer, Opcode::Return, &[]); // 21
    Scenario {
        name: "call_window_ic",
        inner: trivial_inner(),
        inner_regs: 2,
        outer,
        outer_regs: 6,
        feedback: 2,
        handler_entries: Vec::new(),
        has_callee: true,
        const_args: vec![5, 41],
        expect: 41,
    }
}

/// `CallNoFeedback`: the uncached call path (call_target probe +
/// push_frame every iteration).
fn call_no_feedback() -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::CallNoFeedback, &[0, 2, 2]); // 0
    emit(&mut outer, Opcode::Store, &[5]); // 4
    emit(&mut outer, Opcode::Load, &[3]); // 6
    emit(&mut outer, Opcode::Add, &[4]); // 8
    emit(&mut outer, Opcode::Store, &[3]); // 10
    emit(&mut outer, Opcode::Load, &[3]); // 12
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 14 -> 18
    emit(&mut outer, Opcode::Jump, &[(-16i32) as u32]); // 16 -> 0
    emit(&mut outer, Opcode::Load, &[5]); // 18
    emit(&mut outer, Opcode::Return, &[]); // 20
    Scenario {
        name: "call_no_feedback",
        inner: trivial_inner(),
        inner_regs: 2,
        outer,
        outer_regs: 6,
        feedback: 0,
        handler_entries: Vec::new(),
        has_callee: true,
        const_args: vec![5, 41],
        expect: 41,
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        loop_baseline(false),
        loop_baseline(true),
        call_function1_ic(),
        call_method1_ic(),
        call_window_ic(),
        call_no_feedback(),
    ]
}

/// Raise/catch loop with no calls resolving: every iteration calls a
/// non-callable through `CallNoFeedback`, which raises a TypeError;
/// the handler range [0, 4) catches it in the same frame and the loop
/// continues. Exercises the full raise path: park the fault offset,
/// materialize the error (allocations — minors will fire mid-loop),
/// sentinel check, unwind, catch dispatch. The old interpreter carries
/// a known latent bug here (raw pre-raise ip vs reloaded origin under
/// a moving collection), so its result is checked leniently.
fn throw_catch_nofb() -> Scenario {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::CallNoFeedback, &[0, 1, 1]); // 0: callee = smi, raises
    emit(&mut outer, Opcode::Store, &[1]); // 4 (catch): r1 = exception
    emit(&mut outer, Opcode::Load, &[2]); // 6
    emit(&mut outer, Opcode::Add, &[3]); // 8
    emit(&mut outer, Opcode::Store, &[2]); // 10
    emit(&mut outer, Opcode::Load, &[2]); // 12
    emit(&mut outer, Opcode::JumpIfFalsy, &[4]); // 14 -> 18
    emit(&mut outer, Opcode::Jump, &[(-16i32) as u32]); // 16 -> 0
    emit(&mut outer, Opcode::Load, &[2]); // 18
    emit(&mut outer, Opcode::Return, &[]); // 20
    Scenario {
        name: "throw_catch_nofb",
        inner: Vec::new(),
        inner_regs: 0,
        outer,
        outer_regs: 4,
        feedback: 0,
        handler_entries: vec![HandlerEntryInit {
            try_start: 0,
            try_end: 4,
            handler_pc: 4,
        }],
        // r0 = smi(1): a non-callable callee (raises TypeError);
        // r1 = exception sink
        has_callee: false,
        const_args: vec![1, 0],
        expect: 0,
    }
}

/// Throw/catch comparison: the sketch must catch every iteration;
/// become may lose the catch on a mid-raise relocation (reported,
/// not asserted).
fn run_throw_pair(s: &Scenario, reps: usize) {
    // correctness probe on young code (relocations live)
    let mut become_lost = 0;
    for _ in 0..reps {
        let (_, b) = run_once::<NewInterpreter>(s, ITERS);
        assert_eq!(b, smi(s.expect), "sketch: catch must run on young code");
        let (_, a, pending, exception) = run_once_raw::<BecomeInterpreter>(s, ITERS);
        if pending {
            assert_eq!(a, exception, "become: unexpected result with pending exception");
            become_lost += 1;
        } else {
            assert_eq!(a, smi(s.expect), "become: unexpected result");
        }
    }
    // timing on aged (non-moving) code, for both sides equally
    let mut cur = Vec::with_capacity(reps);
    let mut sketch = Vec::with_capacity(reps);
    for _ in 0..reps {
        let (ms, a) = run_once_aged::<BecomeInterpreter>(s, ITERS);
        assert_eq!(a, smi(s.expect), "become: catch must run on aged code");
        cur.push(ms);
        let (ms2, b) = run_once_aged::<NewInterpreter>(s, ITERS);
        assert_eq!(b, smi(s.expect), "sketch: catch must run on aged code");
        sketch.push(ms2);
    }
    let (bb, mb) = stats(&cur);
    let (bn, mn) = stats(&sketch);
    let per_iter = |best: f64| best * 1e6 / ITERS as f64;
    println!("{}:", s.name);
    println!(
        "  young-code probe: sketch {}/{} clean, become lost the catch in {become_lost}/{reps}",
        reps, reps
    );
    println!(
        "  become  best={bb:8.2}ms  mean={mb:8.2}ms  ({:6.1} ns/iter, aged code)",
        per_iter(bb)
    );
    println!(
        "  sketch  best={bn:8.2}ms  mean={mn:8.2}ms  ({:6.1} ns/iter, aged code)",
        per_iter(bn)
    );
    println!("  delta(best): {:+6.1}% (new vs become)", (bn - bb) / bb * 100.0);
}

/// Like `run_once_checked`, but ages the program first (one minor
/// collection promotes the freshly assembled bytecode/handlers to the
/// non-moving old generation), so the raise path can be timed without
/// the young-code relocation that kills the old interpreter.

/// One timed execution on a fresh VM. Returns the elapsed ms and the
/// result (asserted against `expect`).
fn run_once_checked<I: Interpreter>(s: &Scenario, iters: i64) -> (f64, Value) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let (ms, result) = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &s.inner, s.inner_regs, 0, &[]);
        let outer =
            callable(thread, &scope, &s.outer, s.outer_regs, s.feedback, &s.handler_entries);
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        let args = s.args(inner_word, iters);
        let start = Instant::now();
        let result = thread.execute(outer, &args).expect("runs");
        (start.elapsed().as_secs_f64() * 1e3, result)
    });
    assert!(!thread.has_pending_exception(), "{}: pending exception", s.name);
    (ms, result)
}

fn run_once_aged<I: Interpreter>(s: &Scenario, iters: i64) -> (f64, Value) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let (ms, result) = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &s.inner, s.inner_regs, 0, &[]);
        let outer =
            callable(thread, &scope, &s.outer, s.outer_regs, s.feedback, &s.handler_entries);
        thread.heap().collect_minor();
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        let args = s.args(inner_word, iters);
        let start = Instant::now();
        let result = thread.execute(outer, &args).expect("runs");
        (start.elapsed().as_secs_f64() * 1e3, result)
    });
    assert!(!thread.has_pending_exception(), "{}: pending exception", s.name);
    (ms, result)
}

/// Like `run_once_checked`, but also reports a pending exception and
/// this VM's own sentinel word (it differs per VM).
fn run_once_raw<I: Interpreter>(s: &Scenario, iters: i64) -> (f64, Value, bool, Value) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let (ms, result) = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &s.inner, s.inner_regs, 0, &[]);
        let outer =
            callable(thread, &scope, &s.outer, s.outer_regs, s.feedback, &s.handler_entries);
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        let args = s.args(inner_word, iters);
        let start = Instant::now();
        let result = thread.execute(outer, &args).expect("runs");
        (start.elapsed().as_secs_f64() * 1e3, result)
    });
    let exception = thread
        .heap()
        .known()
        .exception
        .as_tagged(thread.heap())
        .raw();
    (ms, result, thread.has_pending_exception(), exception)
}

fn run_once<I: Interpreter>(s: &Scenario, iters: i64) -> (f64, Value) {
    let (ms, result) = run_once_checked::<I>(s, iters);
    assert_eq!(result, smi(s.expect), "{}: wrong result", s.name);
    (ms, result)
}

fn stats(times: &[f64]) -> (f64, f64) {
    let best = times.iter().cloned().fold(f64::INFINITY, f64::min);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    (best, mean)
}

/// Interleave the two interpreters rep by rep (thermal/frequency
/// fairness), then report best/mean and the best-vs-best delta.
fn run_pair(s: &Scenario) {
    let mut cur = Vec::with_capacity(REPS);
    let mut sketch = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let (ms, a) = run_once::<BecomeInterpreter>(s, ITERS);
        let (ms2, b) = run_once::<NewInterpreter>(s, ITERS);
        assert_eq!(a, b, "{}: interpreters disagree", s.name);
        cur.push(ms);
        sketch.push(ms2);
    }
    let (bb, mb) = stats(&cur);
    let (bn, mn) = stats(&sketch);
    let per_iter = |best: f64| best * 1e6 / ITERS as f64;
    let delta = (bn - bb) / bb * 100.0;
    println!("{}:", s.name);
    println!("  become  best={bb:8.2}ms  mean={mb:8.2}ms  ({:6.1} ns/iter)", per_iter(bb));
    println!("  sketch  best={bn:8.2}ms  mean={mn:8.2}ms  ({:6.1} ns/iter)", per_iter(bn));
    println!("  delta(best): {delta:+6.1}% (new vs become)");
}

/// Both interpreters agree on the calling programs (fast: tiny trip
/// counts). Runs with plain `cargo test`.
#[test]
fn calling_parity() {
    for s in scenarios() {
        let (_, a) = run_once::<BecomeInterpreter>(&s, 3);
        let (_, b) = run_once::<NewInterpreter>(&s, 3);
        assert_eq!(a, b, "{}: interpreters disagree", s.name);
        assert_eq!(a, smi(s.expect), "{}: wrong result", s.name);
    }
}

/// Small-trip-count throw/catch: both interpreters must agree when no
/// collection can fire (below the young-generation threshold).
#[test]
fn throw_catch_small() {
    let s = throw_catch_nofb();
    let (_, a) = run_once::<BecomeInterpreter>(&s, 3);
    let (_, b) = run_once::<NewInterpreter>(&s, 3);
    assert_eq!(a, b, "small: interpreters disagree");
    let (_, a) = run_once::<BecomeInterpreter>(&s, 5000);
    let (_, b) = run_once::<NewInterpreter>(&s, 5000);
    assert_eq!(a, b, "5000: interpreters disagree");
}

/// Timed comparison; opt in with
/// `cargo test --release --test call_bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_calls() {
    for s in scenarios() {
        run_pair(&s);
    }
    run_throw_pair(&throw_catch_nofb(), REPS);
}

/// Under `stress-minor-gc` every raise relocates the executing code;
/// the catch must still run every iteration.
#[test]
#[cfg(feature = "stress-minor-gc")]
fn throw_catch_survives_code_movement() {
    let s = throw_catch_nofb();
    for _ in 0..8 {
        let (_, v) = run_once::<NewInterpreter>(&s, 25);
        assert_eq!(v, smi(s.expect), "catch must run under per-allocation minors");
    }
}
