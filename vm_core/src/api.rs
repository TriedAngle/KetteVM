//! The extension API: choosing an interpreter and plugging in a language
//! runtime.

use core::any::Any;

use crate::{
    EdgeVisitable, Handle, HandleSlice, Heap, Object, Result, SetupError, Tagged, ThreadState, VM,
    Value,
};

/// The interpreter entry: [[Call]]/[[Construct]] on a bytecode callable.
/// Stored as a plain fn pointer in `SharedVM` (set once from the
/// `I: Interpreter` type parameter) so core re-entry points —
/// `Thread::execute`, `HostCtx::enter` — pay one indirect call,
/// never per opcode.
pub type EntryFn = for<'a, 'v, 's, 'c, 'r, 'n, 't> fn(
    vm: &'v VM,
    runtime: &'t dyn ErasedRuntimeState,
    heap: &'a mut Heap,
    state: &'s ThreadState,
    callable: Handle<'c, Object>,
    args: HandleSlice<'r>,
    new_target: Option<Handle<'n, Value>>,
) -> Result<Tagged<'a, Value>>;

pub trait Interpreter {
    const EXECUTE: EntryFn;
}

/// Index of an installed runtime in the VM's runtime list. Every callable
/// records the runtime its bytecode belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeId(pub u16);

/// A language runtime contributing native functions and globals to a VM
/// (installed once by `VM::add`, before any code runs). The runtime type is
/// its own per-VM state: it is rooted for the GC and fetched back by native
/// functions via `HostCtx::runtime`.
pub trait Runtime: EdgeVisitable + Default + Send + Sync + Any + 'static {
    /// This runtime's slot: runtimes are installed in id order, so the id is
    /// a direct index into the VM's runtime list.
    const ID: RuntimeId;

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), SetupError>;
}

pub trait ErasedRuntimeState: EdgeVisitable + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

impl<T: EdgeVisitable + Any + Send + Sync> ErasedRuntimeState for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
