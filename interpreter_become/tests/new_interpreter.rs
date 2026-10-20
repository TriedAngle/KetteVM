use bytecode::{Opcode, emit};
use interpreter_become::NewInterpreter;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm_core::{
    CallableInfoInit, CallableInfoObject, FixedArray, FixedByteArray, Handle, HandleScope, Heap,
    Object, ObjectSlotsInit, Smi, Tagged, Thread, VM, Value, new_feedback_vector,
};

/// The new `#[handler]`-based interpreter plugs into `VM::new` like
/// any other. The JS-runtime bootstrap already enters and dispatches
/// through it (the first unimplemented opcode, `LoadUndefined`, traps
/// in `op_trap`); running real programs waits on more handlers.
#[test]
fn new_interpreter_constructs() {
    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    drop(vm);
}

fn smi(v: i64) -> Value {
    Smi::new(v).encode()
}

/// A `Normal`-kind bytecode callable over a hand-assembled program;
/// `feedback_slots` attaches a vector so call sites can go monomorphic.
fn callable<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    program: &[u8],
    register_count: usize,
    feedback_slots: usize,
) -> Handle<'s, Object> {
    let bytecode = thread
        .heap()
        .allocate_handle::<FixedByteArray>(program, scope);
    let constants = {
        let empty: &[Tagged<'_, Value>] = &[];
        let staged = scope.stage(empty);
        thread.heap().allocate_handle::<FixedArray>(staged, scope)
    };
    let info = thread.heap().allocate_handle::<CallableInfoObject>(
        CallableInfoInit {
            expected_slots: 0,
            bytecode,
            constants,
            register_count,
            handlers: None,
        },
        scope,
    );
    if let Some(vector) = new_feedback_vector(thread.heap(), scope, feedback_slots) {
        let heap: &Heap = thread.heap();
        info.as_tagged(heap).set_feedback(heap, vector);
    }
    let map = thread.heap().known().function_map;
    let values = {
        let heap: &Heap = thread.heap();
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

/// `CallFunction1` through a call site with a feedback vector: the
/// first execution misses (records the callee), the loop's second and
/// third hit the monomorphic entry and push the frame directly.
#[test]
fn call_function1_ic_miss_then_hit() {
    // inner(recv, a): return a
    let mut inner = Vec::new();
    emit(&mut inner, Opcode::Load, &[1]);
    emit(&mut inner, Opcode::Return, &[]);
    // outer(callee, arg, n, minus_one):
    //   loop n times: r1 = callee(arg); n = n + minus_one
    //   return r1
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

    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &inner, 2, 0);
        let outer = callable(thread, &scope, &outer, 4, 2);
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        thread.execute(outer, &[inner_word, smi(41), smi(3), smi(-1)])
    });
    assert_eq!(result, Ok(smi(41)));
    assert!(!thread.has_pending_exception());
}

/// `CallMethod0` stages the receiver register at element 0.
#[test]
fn call_method0_stages_receiver() {
    // inner(recv): return recv
    let mut inner = Vec::new();
    emit(&mut inner, Opcode::Load, &[0]);
    emit(&mut inner, Opcode::Return, &[]);
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::CallMethod0, &[0, 1, 0]);
    emit(&mut outer, Opcode::Return, &[]);

    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &inner, 1, 0);
        let outer = callable(thread, &scope, &outer, 2, 0);
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        thread.execute(outer, &[inner_word, smi(7)])
    });
    assert_eq!(result, Ok(smi(7)));
}

/// `Call` passes the contiguous `[receiver, args...]` window.
#[test]
fn call_passes_window() {
    // inner(recv, a): return a
    let mut inner = Vec::new();
    emit(&mut inner, Opcode::Load, &[1]);
    emit(&mut inner, Opcode::Return, &[]);
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::Call, &[0, 2, 2, 0]);
    emit(&mut outer, Opcode::Return, &[]);

    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let inner = callable(thread, &scope, &inner, 2, 0);
        let outer = callable(thread, &scope, &outer, 3, 2);
        let inner_word = inner.as_tagged(&*thread.heap()).raw();
        thread.execute(outer, &[inner_word, smi(5), smi(9)])
    });
    assert_eq!(result, Ok(smi(9)));
}

/// A non-callable callee raises through the miss path and unwinds out.
#[test]
fn call_non_callable_throws() {
    let mut outer = Vec::new();
    emit(&mut outer, Opcode::Call, &[0, 0, 1, 0]);
    emit(&mut outer, Opcode::Return, &[]);

    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let outer = callable(thread, &scope, &outer, 1, 2);
        thread.execute(outer, &[smi(1)])
    });
    let exception = {
        let heap = thread.heap();
        heap.known().exception.as_tagged(heap).raw()
    };
    assert_eq!(result, Ok(exception));
    assert!(thread.has_pending_exception(), "the raise must surface");
}
