use bytecode::{Constant, FnBuilder, Function, FunctionId, Program, Reg, RegList};
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    DefaultInterpreter, Handle, HandleScope, HandlerEntryInit, HandlerTable, HandlerTableInit,
    Runtime, ScriptError, Smi, Thread, VM, Value, VmCore,
};

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .add::<VmCore>()
        .unwrap()
        .attach()
}

fn run(thread: &mut Thread, functions: Vec<Function>) -> Result<Value, ScriptError> {
    let mut program = Program::new();
    for function in functions {
        program.add_function(function);
    }
    thread.run_program(program, VmCore::ID)
}

fn table<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    entries: &[HandlerEntryInit],
) -> Handle<'s, HandlerTable> {
    thread
        .heap()
        .allocate_handle::<HandlerTable>(HandlerTableInit { entries }, scope)
}

#[test]
fn roundtrip_entries() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let t = table(
            thread,
            &scope,
            &[
                HandlerEntryInit::new(0, 10, 40),
                HandlerEntryInit::new(12, 20, 55),
            ],
        );
        let heap = thread.heap();
        let table = t.as_tagged(heap);
        assert_eq!(table.len(), 2);
        assert_eq!(table.entry(0), HandlerEntryInit::new(0, 10, 40));
        assert_eq!(table.entry(1), HandlerEntryInit::new(12, 20, 55));
    });
}

#[test]
fn lookup_finds_handler_inside_range() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let t = table(thread, &scope, &[HandlerEntryInit::new(5, 15, 100)]);
        let heap = thread.heap();
        let table = t.as_tagged(heap);
        // try_start is inside (inclusive) ...
        assert_eq!(table.lookup(5), Some(100));
        // ... as is any offset before try_end
        assert_eq!(table.lookup(14), Some(100));
    });
}

#[test]
fn lookup_returns_none_outside_range() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let t = table(thread, &scope, &[HandlerEntryInit::new(5, 15, 100)]);
        let heap = thread.heap();
        let table = t.as_tagged(heap);
        // before the region ...
        assert_eq!(table.lookup(4), None);
        // ... try_end is exclusive ...
        assert_eq!(table.lookup(15), None);
        // ... and beyond it
        assert_eq!(table.lookup(16), None);
    });
}

#[test]
fn lookup_returns_innermost_of_nested_ranges() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        // inner entry emitted first: lookup must be independent of
        // emission order (properly-nested try regions)
        let t = table(
            thread,
            &scope,
            &[
                HandlerEntryInit::new(4, 12, 300),
                HandlerEntryInit::new(0, 20, 100),
            ],
        );
        let heap = thread.heap();
        let table = t.as_tagged(heap);
        // inside both ranges: the innermost (largest try_start) wins
        assert_eq!(table.lookup(5), Some(300));
        // inside the outer range only
        assert_eq!(table.lookup(1), Some(100));
        assert_eq!(table.lookup(15), Some(100));
    });
}

#[test]
fn lookup_on_empty_table_returns_none() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let t = table(thread, &scope, &[]);
        let heap = thread.heap();
        let table = t.as_tagged(heap);
        assert_eq!(table.len(), 0);
        assert_eq!(table.lookup(0), None);
    });
}

#[test]
fn throw_is_caught_in_same_function() {
    let mut thread = test_vm();

    let mut f = FnBuilder::new(0);
    let t = f.begin_try();
    f.load_smi(99);
    f.throw();
    f.end_try(t);
    f.handler_entry(t);
    f.store(Reg::new(0));
    f.load(Reg::new(0));
    f.ret();
    let f = f.finish(Default::default()).unwrap();

    let result = run(&mut thread, vec![f]).unwrap();
    assert_eq!(Smi::decode(result).unwrap().value(), 99);
    // catching consumes the pending exception
    assert!(!thread.has_pending_exception());
}

#[test]
fn throw_any_value_escapes_as_sentinel() {
    let mut thread = test_vm();

    // throw 42: arbitrary values are throwable (ES 14.18), no handlers
    let mut f = FnBuilder::new(0);
    f.load_smi(42);
    f.throw();
    let f = f.finish(Default::default()).unwrap();

    let result = run(&mut thread, vec![f]).unwrap();
    let exception_word = {
        let heap = thread.heap();
        heap.known().exception.get(heap).raw()
    };
    assert_eq!(result, exception_word);
    assert_eq!(
        thread.take_pending_exception(),
        Some(Smi::new(42).encode()),
        "the thrown value itself is the pending exception"
    );
}

#[test]
fn innermost_handler_wins() {
    let mut thread = test_vm();

    // the inner region covers only the throw, so its try_start is larger
    let mut f = FnBuilder::new(0);
    let outer = f.begin_try();
    f.load_smi(1);
    f.load_smi(2);
    let inner = f.begin_try();
    f.throw();
    f.end_try(inner);
    f.end_try(outer);
    f.handler_entry(inner);
    f.load_smi(100);
    f.ret();
    f.handler_entry(outer);
    f.load_smi(200);
    f.ret();
    let f = f.finish(Default::default()).unwrap();

    let result = run(&mut thread, vec![f]).unwrap();
    assert_eq!(Smi::decode(result).unwrap().value(), 100);
}

#[test]
fn exception_unwinds_to_caller() {
    let mut thread = test_vm();

    // callee: throws 7, no handlers
    let mut callee = FnBuilder::new(0);
    callee.load_smi(7);
    callee.throw();
    let callee = callee.finish(Default::default()).unwrap();

    // caller: the try region covers the call site, so the handler binds the
    // exception raised by the suspended callee frame
    let mut caller = FnBuilder::new(0);
    let t = caller.begin_try();
    let template = caller.constant(Constant::Callable(FunctionId(1)));
    caller.create_closure(template);
    let r0 = Reg::new(0);
    caller.store(r0);
    caller.call_no_feedback(r0, RegList::new(r0, 1));
    caller.end_try(t);
    caller.handler_entry(t);
    caller.store(r0);
    caller.load(r0);
    caller.ret();
    let caller = caller.finish(Default::default()).unwrap();

    let result = run(&mut thread, vec![caller, callee]).unwrap();
    assert_eq!(Smi::decode(result).unwrap().value(), 7);
}

#[test]
fn throw_from_finally_escapes_past_its_own_handler() {
    let mut thread = test_vm();

    // the handler rethrows the exception; its throw is outside the try
    // region, so no handler matches and the run ends
    let mut f = FnBuilder::new(0);
    let t = f.begin_try();
    f.load_smi(3);
    f.throw();
    f.end_try(t);
    f.handler_entry(t);
    f.throw();
    let f = f.finish(Default::default()).unwrap();

    let result = run(&mut thread, vec![f]).unwrap();
    let exception_word = {
        let heap = thread.heap();
        heap.known().exception.get(heap).raw()
    };
    assert_eq!(result, exception_word);
    assert_eq!(thread.take_pending_exception(), Some(Smi::new(3).encode()));
}
