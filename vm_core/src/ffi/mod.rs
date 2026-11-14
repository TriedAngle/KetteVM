//! C FFI: type identity, layout, foreign pointers, libraries, and calls.
//!
//! FFI is a core VM subsystem owned by [`crate::VmCore`]; it is always
//! compiled in and has no runtime id of its own.
//!
//! VM-owned C data lives in VM byte arrays; there is no external allocation
//! arena. Only library handles and memory owned by C live outside the heap.

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
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// FFI state owned by `VmCore`.
///
/// The type table is growable at runtime: `FFI.struct(...)` defines a new
/// type from guest code. Runtime state is handed out as a shared reference,
/// so the table sits behind a lock.
pub struct FfiState {
    types: RwLock<TypeTable>,
    plans: RwLock<HashMap<TypeId, Arc<CallPlan>>>,
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
}

impl Default for FfiState {
    fn default() -> Self {
        Self {
            types: RwLock::new(TypeTable::new()),
            plans: RwLock::new(HashMap::new()),
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
