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

/// Build a callable, run it, and return the result value; panics on a
/// throw. `args[0]` is the receiver, `args[1..]` the parameters.
fn run(program: &[u8], register_count: usize, args: &[Value]) -> Value {
    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let f = callable(thread, &scope, program, register_count, 0);
        thread.execute(f, args)
    });
    assert!(!thread.has_pending_exception(), "program threw");
    result.expect("program must not throw")
}

/// `Mul`/`Div`/`ModImmediate`/`Negate` inline paths and the operations
/// after them. Folded-in pipeline: `b * a`, then `(b*a) / b`, then
/// `% 5`, then negate → `-4` for (a, b) = (14, 3).
#[test]
fn mul_div_mod_negate_handlers() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::Load, &[1]); // acc = a
    emit(&mut p, Opcode::Mul, &[2]); // acc = b * a = 42
    emit(&mut p, Opcode::Store, &[3]); // r3 = 42
    emit(&mut p, Opcode::Load, &[2]); // acc = b
    emit(&mut p, Opcode::Div, &[3]); // acc = r3 / b = 14
    emit(&mut p, Opcode::Store, &[3]); // r3 = 14
    emit(&mut p, Opcode::ModImmediate, &[3, 5]); // acc = 14 % 5 = 4
    emit(&mut p, Opcode::Negate, &[]); // acc = -4
    emit(&mut p, Opcode::Return, &[]);
    assert_eq!(run(&p, 4, &[smi(0), smi(14), smi(3)]), smi(-4));
}

/// `MulImmediate`/`DivImmediate`/`ModImmediate`/`ExpImmediate`, each
/// reading r1 as the left operand.
#[test]
fn arithmetic_immediate_handlers() {
    let mut mul = Vec::new();
    emit(&mut mul, Opcode::MulImmediate, &[1, 3]);
    emit(&mut mul, Opcode::Return, &[]);
    assert_eq!(run(&mul, 2, &[smi(0), smi(12)]), smi(36));

    let mut div = Vec::new();
    emit(&mut div, Opcode::DivImmediate, &[1, 6]);
    emit(&mut div, Opcode::Return, &[]);
    assert_eq!(run(&div, 2, &[smi(0), smi(12)]), smi(2));

    let mut modulo = Vec::new();
    emit(&mut modulo, Opcode::ModImmediate, &[1, 5]);
    emit(&mut modulo, Opcode::Return, &[]);
    assert_eq!(run(&modulo, 2, &[smi(0), smi(12)]), smi(2));

    let mut exp = Vec::new();
    emit(&mut exp, Opcode::ExpImmediate, &[1, 3]);
    emit(&mut exp, Opcode::Return, &[]);
    assert_eq!(run(&exp, 2, &[smi(0), smi(12)]), smi(1728));
}

/// `Exp` with a register exponent (`r1 ^ acc`).
#[test]
fn exp_register_handler() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::Load, &[2]); // acc = 3
    emit(&mut p, Opcode::Exp, &[1]); // acc = 12 ** 3
    emit(&mut p, Opcode::Return, &[]);
    assert_eq!(run(&p, 3, &[smi(0), smi(12), smi(3)]), smi(1728));
}

/// `BitwiseOr`/`BitwiseXor`/`BitwiseAnd`, register operand on the left.
#[test]
fn bitwise_register_handlers() {
    let mut or_xor = Vec::new();
    emit(&mut or_xor, Opcode::Load, &[1]); // acc = 12
    emit(&mut or_xor, Opcode::BitwiseOr, &[2]); // acc = 10 | 12 = 14
    emit(&mut or_xor, Opcode::BitwiseXor, &[2]); // acc = 10 ^ 14 = 4
    emit(&mut or_xor, Opcode::Return, &[]);
    assert_eq!(run(&or_xor, 3, &[smi(0), smi(12), smi(10)]), smi(4));

    let mut and = Vec::new();
    emit(&mut and, Opcode::Load, &[1]);
    emit(&mut and, Opcode::BitwiseAnd, &[2]); // 10 & 12 = 8
    emit(&mut and, Opcode::Return, &[]);
    assert_eq!(run(&and, 3, &[smi(0), smi(12), smi(10)]), smi(8));
}

/// `ShiftLeft`/`ShiftRight`/`ShiftRightLogical`, register operand on the
/// left; the logical shift keeps a negative source unsigned.
#[test]
fn shift_register_handlers() {
    let mut left = Vec::new();
    emit(&mut left, Opcode::Load, &[2]); // acc = 3
    emit(&mut left, Opcode::ShiftLeft, &[1]); // 12 << 3 = 96
    emit(&mut left, Opcode::Return, &[]);
    assert_eq!(run(&left, 3, &[smi(0), smi(12), smi(3)]), smi(96));

    let mut right = Vec::new();
    emit(&mut right, Opcode::Load, &[2]); // acc = 2
    emit(&mut right, Opcode::ShiftRight, &[1]); // -8 >> 2 = -2
    emit(&mut right, Opcode::Return, &[]);
    assert_eq!(run(&right, 3, &[smi(0), smi(-8), smi(2)]), smi(-2));

    let mut logical = Vec::new();
    emit(&mut logical, Opcode::Load, &[2]);
    emit(&mut logical, Opcode::ShiftRightLogical, &[1]); // -8 >>> 2
    emit(&mut logical, Opcode::Return, &[]);
    assert_eq!(run(&logical, 3, &[smi(0), smi(-8), smi(2)]), smi(1073741822));
}

/// The immediate bitwise/shift forms.
#[test]
fn bitwise_immediate_handlers() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::BitwiseOrImmediate, &[1, 9]); // 12 | 9 = 13
    emit(&mut p, Opcode::BitwiseXorImmediate, &[1, 5]); // 12 ^ 5 = 9
    emit(&mut p, Opcode::BitwiseAndImmediate, &[1, 6]); // 12 & 6 = 4
    emit(&mut p, Opcode::Return, &[]);
    assert_eq!(run(&p, 2, &[smi(0), smi(12)]), smi(4));

    let mut left = Vec::new();
    emit(&mut left, Opcode::ShiftLeftImmediate, &[1, 3]);
    emit(&mut left, Opcode::Return, &[]);
    assert_eq!(run(&left, 2, &[smi(0), smi(12)]), smi(96));

    let mut right = Vec::new();
    emit(&mut right, Opcode::ShiftRightImmediate, &[1, 2]);
    emit(&mut right, Opcode::Return, &[]);
    assert_eq!(run(&right, 2, &[smi(0), smi(-8)]), smi(-2));

    let mut logical = Vec::new();
    emit(&mut logical, Opcode::ShiftRightLogicalImmediate, &[1, 2]);
    emit(&mut logical, Opcode::Return, &[]);
    assert_eq!(run(&logical, 2, &[smi(0), smi(-8)]), smi(1073741822));
}

/// `LoadZero`/`Move`/`Load` plumbing.
#[test]
fn move_and_load_zero_handlers() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::LoadSmi, &[42]);
    emit(&mut p, Opcode::Store, &[1]);
    emit(&mut p, Opcode::Move, &[2, 1]); // r2 = r1
    emit(&mut p, Opcode::LoadZero, &[]);
    emit(&mut p, Opcode::Load, &[2]);
    emit(&mut p, Opcode::Return, &[]);
    assert_eq!(run(&p, 3, &[smi(0)]), smi(42));
}

/// `LoadNull` yields a falsy value: the branch skips the `LoadSmi 1`.
#[test]
fn load_null_handler() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::LoadNull, &[]);
    emit(&mut p, Opcode::JumpIfFalsy, &[4]); // -> Return
    emit(&mut p, Opcode::LoadSmi, &[1]);
    emit(&mut p, Opcode::Return, &[]);

    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    let mut thread = vm.attach();
    let result = thread.handle_scope(|thread, scope| {
        let f = callable(thread, &scope, &p, 1, 0);
        thread.execute(f, &[smi(0)])
    });
    let result = result.expect("program must not throw");
    let null = {
        let heap = thread.heap();
        heap.known().null.as_tagged(heap).raw()
    };
    assert_eq!(result, null);
}

/// `AddLoc`/`SubLoc` write the destination register and leave the
/// result in the accumulator: 10 + 4 = 14, then 14 - 4 = 10.
#[test]
fn add_sub_loc_handlers() {
    let mut p = Vec::new();
    emit(&mut p, Opcode::LoadSmi, &[10]);
    emit(&mut p, Opcode::Store, &[1]);
    emit(&mut p, Opcode::LoadSmi, &[4]);
    emit(&mut p, Opcode::Store, &[2]);
    emit(&mut p, Opcode::AddLoc, &[1, 2]); // r1 = 14
    emit(&mut p, Opcode::SubLoc, &[1, 2]); // r1 = 10
    emit(&mut p, Opcode::Return, &[]);
    assert_eq!(run(&p, 3, &[smi(0)]), smi(10));
}

/// Float operands take the full ToNumeric/ToInt32 slow paths for every
/// bitwise/shift kind (`bitwise_body`), and the non-exact division that
/// produces the float exercises `slow_div_immediate`.
#[test]
fn bitwise_slow_paths() {
    // r1 = 21 / 2 = 10.5 (a boxed float), acc = the argument.
    fn with_float(imm: u32) -> Vec<u8> {
        let mut p = Vec::new();
        emit(&mut p, Opcode::LoadSmi, &[21]);
        emit(&mut p, Opcode::Store, &[1]);
        emit(&mut p, Opcode::DivImmediate, &[1, 2]);
        emit(&mut p, Opcode::Store, &[1]);
        emit(&mut p, Opcode::LoadSmi, &[imm]);
        p
    }
    fn case(op: Opcode, imm: u32, expected: Value) {
        let mut p = with_float(imm);
        emit(&mut p, op, &[1]);
        emit(&mut p, Opcode::Return, &[]);
        assert_eq!(run(&p, 2, &[smi(0)]), expected, "{op:?}");
    }

    // ToInt32(10.5) = 10.
    case(Opcode::BitwiseOr, 3, smi(10 | 3));
    case(Opcode::BitwiseXor, 3, smi(10 ^ 3));
    case(Opcode::BitwiseAnd, 3, smi(10 & 3));
    case(Opcode::ShiftLeft, 2, smi(10 << 2));
    case(Opcode::ShiftRight, 2, smi(10 >> 2));
    case(Opcode::ShiftRightLogical, 2, smi(10 >> 2));
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
