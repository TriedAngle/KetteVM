//! Guest callbacks invoked by C through libffi closures.
//!
//! A closure runs on the thread that is inside the corresponding C call. The
//! call path pushes a [`CallbackCtx`] on a thread-local stack for the duration
//! of that call, so the trampoline can re-enter the VM even though C, not Rust,
//! owns the stack. Contexts are matched by VM, which keeps several VMs running
//! on one thread apart. A guest exception cannot unwind through C, so it is
//! recorded on the VM and re-raised once the C call returns.

use crate::bootstrap::smi_slot;
use crate::ffi::call::encode_value;
use crate::ffi::{ClosureEntry, FfiError, FfiResult, FfiState, TypeKind, cif_for_signature};
use crate::{
    CValue, CValueFlags, CValueInit, ErasedRuntimeState, Handle, HandleScope, Heap, HostCtx,
    ThreadState, TypeInfo, VM, Value,
};
use core::cell::RefCell;
use libffi::middle::Closure;
use std::ffi::c_void;

/// The re-entry context of one C call on this thread.
#[derive(Clone, Copy)]
pub struct CallbackCtx {
    pub vm: *const VM,
    pub runtime: &'static dyn ErasedRuntimeState,
    pub heap: *mut Heap,
    pub state: *const ThreadState,
    pub ffi: *const FfiState,
}

thread_local! {
    /// C calls currently on this thread's stack, innermost last.
    static CALLS: RefCell<Vec<CallbackCtx>> = const { RefCell::new(Vec::new()) };
}

/// Publish the re-entry context of a C call.
pub fn enter_call(ctx: CallbackCtx) {
    CALLS.with(|calls| calls.borrow_mut().push(ctx));
}

/// Drop the innermost C call's context.
pub fn leave_call() {
    CALLS.with(|calls| {
        calls.borrow_mut().pop();
    });
}

/// The innermost context belonging to `ffi` on this thread.
fn find_call(ffi: *const FfiState) -> Option<CallbackCtx> {
    CALLS.with(|calls| {
        calls
            .borrow()
            .iter()
            .rev()
            .find(|ctx| ctx.ffi == ffi)
            .copied()
    })
}

/// Create a C function pointer that calls `guest` and has `signature`.
pub fn callback<'s>(
    ffi: &FfiState,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    signature: &Handle<TypeInfo>,
    guest: &Handle<Value>,
) -> FfiResult<Handle<'s, CValue>> {
    if signature.as_tagged(heap).kind() != TypeKind::Function {
        return Err(FfiError::NotAFunction(signature.as_tagged(heap).id()));
    }
    let cif = cif_for_signature(heap, signature.as_tagged(heap))?;
    let entry = Box::into_raw(Box::new(ClosureEntry {
        signature: smi_slot::<TypeInfo>(),
        callback: smi_slot::<Value>(),
        ffi: ffi as *const FfiState,
    }));
    {
        // Safety: `entry` was just boxed and is initialized exactly once.
        let entry = unsafe { &*entry };
        entry.signature.init_tagged(signature.as_tagged(heap));
        entry.callback.init_tagged(guest.as_tagged(heap));
    }
    // Safety: the entry is leaked below and never moved, so the userdata
    // reference stays valid for the lifetime of the closure.
    let closure: Closure<'static> = Closure::new(cif, closure_trampoline, unsafe { &*entry });
    let address = *closure.code_ptr() as *const () as *mut u8;
    ffi.register_closure(entry, Box::new(closure));
    Ok(heap.allocate_handle::<CValue>(
        CValueInit {
            ty: signature.clone(),
            map: None,
            address,
            flags: CValueFlags::NONE,
            storage: None,
            library: None,
            callable: None,
        },
        scope,
    ))
}

unsafe extern "C" fn closure_trampoline(
    _cif: &libffi::low::ffi_cif,
    result: &mut u8,
    args: *const *const c_void,
    userdata: &ClosureEntry,
) {
    // Safety: the entry is leaked and lives as long as the closure.
    let ffi = unsafe { &*userdata.ffi };
    if ffi.callback_threw() {
        return;
    }
    // The closure may be invoked with an unrelated VM's C call innermost, so
    // pick the context that belongs to this callback's VM.
    let Some(ctx) = find_call(userdata.ffi) else {
        return;
    };
    let vm: &VM = unsafe { &*ctx.vm };
    let state: &ThreadState = unsafe { &*ctx.state };
    let heap_pointer = ctx.heap;
    state.handle_scope(|scope| {
        // Types become handles first, so the guest call can hold `&mut Heap`.
        let (params, sizes, result_ty, callback) = {
            let heap = unsafe { &*heap_pointer };
            let signature = userdata.signature.get(heap);
            let callback = scope.handle(userdata.callback.get(heap));
            let mut params = Vec::new();
            let mut sizes = Vec::new();
            if let Some(array) = signature.params(heap) {
                for index in 0..array.len() {
                    if let Some(ty) = array.at(heap, index).get_as::<TypeInfo>(heap) {
                        sizes.push(ty.size());
                        params.push(scope.handle(ty));
                    }
                }
            }
            let result_ty = signature.result(heap).map(|ty| scope.handle(ty));
            (params, sizes, result_ty, callback)
        };

        let heap: &mut Heap = unsafe { &mut *heap_pointer };
        let mut values: Vec<Handle<Value>> = Vec::with_capacity(params.len() + 1);
        values.push(heap.known().undefined.get_handle(heap, &scope).erase());
        for (index, (ty, size)) in params.iter().zip(&sizes).enumerate() {
            let pointer = unsafe { *args.add(index) as *const u8 };
            // Safety: libffi passes a pointer to a `size`-byte argument.
            let bytes = unsafe { core::slice::from_raw_parts(pointer, *size) };
            let value = ffi.read_value(heap, &scope, ty, bytes, None);
            values.push(scope.handle(value));
        }
        let staged = scope.stage_handles(&values);
        let value = HostCtx::enter(vm, ctx.runtime, heap, state, callback, staged, None);
        let Some(result_ty) = result_ty else {
            if value.is_err() {
                ffi.set_callback_threw();
            }
            return;
        };
        let Ok(value) = value else {
            ffi.set_callback_threw();
            return;
        };
        let value = scope.handle(value);
        let result_ty = result_ty.as_tagged(heap);
        if result_ty.kind() == TypeKind::Void {
            return;
        }
        let size = result_ty.size();
        // Safety: libffi provides a `size`-byte result area.
        let out = unsafe { core::slice::from_raw_parts_mut(result as *mut u8, size) };
        if encode_value(heap, result_ty, value.as_tagged(heap), out).is_err() {
            ffi.set_callback_threw();
        }
    });
}
