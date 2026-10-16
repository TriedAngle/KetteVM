use interpreter_become::NewInterpreter;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm_core::VM;

/// The new `#[handler]`-based interpreter plugs into `VM::new` like
/// any other. The JS-runtime bootstrap already enters and dispatches
/// through it (the first unimplemented opcode, `LoadUndefined`, traps
/// in `op_trap`); running real programs waits on more handlers.
#[test]
fn new_interpreter_constructs() {
    let vm = VM::new::<MarkSweep, NewInterpreter>(MarkSweepConfig::default()).unwrap();
    drop(vm);
}
