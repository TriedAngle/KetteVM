//! C FFI: type identity, layout, foreign pointers, libraries, and calls.
//!
//! FFI is a core VM subsystem owned by [`crate::VmCore`]; it is always
//! compiled in and has no runtime id of its own.

pub mod bindings;
mod call;
mod error;
mod library;
mod memory;
mod plan;
mod platform;
mod pointer;
mod ty;
mod values;

pub use call::CallSlot;
pub use error::{FfiError, FfiResult};
pub use plan::CallPlan;
pub use ty::{
    Abi, ArrayType, CType, Field, FunctionType, StructType, TypeError, TypeId, TypeTable,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// FFI state owned by `VmCore`.
///
/// The type table is growable at runtime: `FFI.struct(...)` defines a new
/// type from guest code. Runtime state is handed out as a shared reference,
/// so the table sits behind a lock.
pub struct FfiState {
    types: RwLock<TypeTable>,
    plans: RwLock<HashMap<TypeId, Arc<CallPlan>>>,
    /// Stack of manual allocation scopes; each holds freed-at-scope-end
    /// external addresses. Single-threaded use is assumed.
    arena: Mutex<Vec<Vec<usize>>>,
}

impl FfiState {
    pub fn types(&self) -> RwLockReadGuard<'_, TypeTable> {
        self.types.read().expect("ffi type table poisoned")
    }

    pub fn types_mut(&self) -> RwLockWriteGuard<'_, TypeTable> {
        self.types.write().expect("ffi type table poisoned")
    }

    pub fn plans(&self) -> RwLockReadGuard<'_, HashMap<TypeId, Arc<CallPlan>>> {
        self.plans.read().expect("ffi plan cache poisoned")
    }

    pub fn plans_mut(&self) -> RwLockWriteGuard<'_, HashMap<TypeId, Arc<CallPlan>>> {
        self.plans.write().expect("ffi plan cache poisoned")
    }

    /// Record an external allocation in the innermost scope, if one is open.
    pub(super) fn track(&self, address: usize) {
        let mut arena = self.arena.lock().expect("ffi arena poisoned");
        if let Some(scope) = arena.last_mut() {
            scope.push(address);
        }
    }

    /// Forget a tracked allocation (it is being freed explicitly).
    pub(super) fn untrack(&self, address: usize) {
        let mut arena = self.arena.lock().expect("ffi arena poisoned");
        for scope in arena.iter_mut().rev() {
            if let Some(index) = scope.iter().position(|tracked| *tracked == address) {
                scope.swap_remove(index);
                return;
            }
        }
    }

    pub fn scope_begin(&self) {
        self.arena
            .lock()
            .expect("ffi arena poisoned")
            .push(Vec::new());
    }

    /// Free everything allocated since the matching `scope_begin`.
    pub fn scope_end(&self) {
        let scope = self
            .arena
            .lock()
            .expect("ffi arena poisoned")
            .pop()
            .expect("ffi scope_end without scope_begin");
        for address in scope {
            crate::ffi::memory::free_external(address);
        }
    }
}

impl Default for FfiState {
    fn default() -> Self {
        Self {
            types: RwLock::new(TypeTable::new()),
            plans: RwLock::new(HashMap::new()),
            arena: Mutex::new(Vec::new()),
        }
    }
}

impl std::fmt::Debug for FfiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiState")
            .field("types", &self.types().len())
            .field("plans", &self.plans().len())
            .finish()
    }
}
