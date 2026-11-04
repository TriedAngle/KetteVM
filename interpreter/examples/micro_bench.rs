//! Microbenchmark for individual bytecode ops on the match and become backends.

use std::hint::black_box;
use std::time::Instant;
use vm_core::Runtime;

use bytecode::{Opcode, emit};
use interpreter::DefaultInterpreter;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm_core::{
    CallableInfoInit, CallableInfoObject, Code, CodeInit, FixedArray, Handle, HandleScope, Heap,
    Interpreter, MODE_SLOPPY, Object, ObjectSlotsInit, Smi, Tagged, Thread, VM, Value, VmCore,
    new_feedback_vector,
};

fn smi(v: i64) -> Value {
    Smi::new(v).encode()
}

fn callable<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    program: &[u8],
    register_count: usize,
) -> Handle<'s, Object> {
    let runtime = thread.vm().runtime_id::<VmCore>();
    let code = thread.heap().allocate_handle::<Code>(
        CodeInit {
            runtime,
            bytes: program,
        },
        scope,
    );
    let constants = {
        let empty: &[Tagged<'_, Value>] = &[];
        let staged = scope.stage(empty);
        thread.heap().allocate_handle::<FixedArray>(staged, scope)
    };
    let info = thread.heap().allocate_handle::<CallableInfoObject>(
        CallableInfoInit {
            expected_slots: 0,
            code,
            constants,
            register_count,
            handlers: None,
            mode: MODE_SLOPPY,
        },
        scope,
    );
    if let Some(vector) = new_feedback_vector(thread.heap(), scope, 0) {
        let heap: &Heap = thread.heap();
        info.as_tagged(heap).set_feedback(heap, vector);
    }
    let map = thread.heap().known().function_map.as_handle();
    let values = {
        let heap: &Heap = thread.heap();
        scope.stage(&[
            info.as_tagged(heap).erase(),
            heap.known().empty_context.get(heap).erase(),
        ])
    };
    let empty_fixed_array = thread.heap().known().empty_fixed_array.as_handle();
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

fn load_program(n: usize) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 0..n {
        emit(&mut code, Opcode::Load, &[1]);
    }
    emit(&mut code, Opcode::Load, &[1]);
    emit(&mut code, Opcode::Return, &[]);
    code
}

fn store_program(n: usize) -> Vec<u8> {
    let mut code = Vec::new();
    emit(&mut code, Opcode::Load, &[1]);
    for _ in 0..n {
        emit(&mut code, Opcode::Store, &[3]);
    }
    emit(&mut code, Opcode::Load, &[3]);
    emit(&mut code, Opcode::Return, &[]);
    code
}

fn add_program(n: usize) -> Vec<u8> {
    let mut code = Vec::new();
    emit(&mut code, Opcode::Load, &[1]);
    for _ in 0..n {
        emit(&mut code, Opcode::Add, &[2]);
    }
    emit(&mut code, Opcode::Return, &[]);
    code
}

fn bench<I: Interpreter>(name: &str, op: &str, program: &[u8], reps: usize) {
    let vm = VM::new::<MarkSweep, I>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let args = [smi(0), smi(1), smi(0)];

    let elapsed = thread.handle_scope(|thread, scope| {
        let f = callable(thread, &scope, program, 4);
        // Warm up code/IC state and make sure the program returns.
        let result = thread
            .execute(f, &args, vm_core::intrinsics::VmCore::ID)
            .expect("program runs");
        black_box(result);
        let start = Instant::now();
        for _ in 0..reps {
            black_box(
                thread
                    .execute(f, &args, vm_core::intrinsics::VmCore::ID)
                    .expect("program runs"),
            );
        }
        start.elapsed()
    });

    println!(
        "{op:6} {name:12} {:8.3} ms/run",
        elapsed.as_secs_f64() * 1e3 / reps as f64
    );
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000);
    let reps: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let load = load_program(n);
    let store = store_program(n);
    let add = add_program(n);

    for (op, program) in [("load", &load), ("store", &store), ("add", &add)] {
        bench::<DefaultInterpreter>("default", op, program, reps);
    }
}
