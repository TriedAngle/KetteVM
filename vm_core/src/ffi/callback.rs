use crate::ffi::call::encode_value;
use crate::ffi::{
    ClosureEntry, EntryPtr, FLAG_VARIADIC, FfiError, FfiResult, FfiState, TypeKind,
    cif_for_signature,
};

use crate::{
    CValue, CValueFlags, CValueInit, CallTarget, ErasedRuntimeState, GcSlot, Handle, HandleScope,
    Heap, Object, RuntimeId, ThreadState, TypeInfo, VM, Value,
};
use core::cell::Cell;
use libffi::middle::Closure;
use std::ffi::c_void;

pub struct CallbackCtx {
    previous: *const CallbackCtx,
    vm: *const VM,
    runtime: &'static dyn ErasedRuntimeState,
    heap: *mut Heap,
    state: *const ThreadState,
    ffi: *const FfiState,
}

thread_local! {
    static INNERMOST_CALL: Cell<*const CallbackCtx> = const { Cell::new(std::ptr::null()) };
}

impl CallbackCtx {
    pub fn new(
        vm: &VM,
        runtime: &dyn ErasedRuntimeState,
        heap: &mut Heap,
        state: &ThreadState,
        ffi: &FfiState,
    ) -> Self {
        CallbackCtx {
            previous: std::ptr::null(),
            vm,
            // Safety: the VM owns its runtimes and outlives every call.
            runtime: unsafe {
                core::mem::transmute::<&dyn ErasedRuntimeState, &'static dyn ErasedRuntimeState>(
                    runtime,
                )
            },
            heap,
            state,
            ffi,
        }
    }

    /// Publish this context as the innermost C call on this thread.
    pub fn enter(&mut self) {
        self.previous = INNERMOST_CALL.with(|slot| slot.replace(self));
    }

    /// # Safety
    /// Only valid while this context is the innermost C call on the thread;
    /// the borrows must not outlive it.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn parts(&self) -> (&FfiState, &VM, &ThreadState, &mut Heap) {
        // Safety: the pointers are live for the duration of the call.
        unsafe { (&*self.ffi, &*self.vm, &*self.state, &mut *self.heap) }
    }

    /// The runtime the C call was made from.
    pub fn runtime(&self) -> &'static dyn ErasedRuntimeState {
        self.runtime
    }

    /// The innermost C call on this thread, if the callback was invoked from
    /// one. C-owned threads have no context until they attach.
    pub fn try_current() -> Option<&'static CallbackCtx> {
        let innermost = INNERMOST_CALL.with(|slot| slot.get());
        if innermost.is_null() {
            return None;
        }
        // Safety: a published context lives until its call returns.
        Some(unsafe { &*innermost })
    }
}

impl Drop for CallbackCtx {
    fn drop(&mut self) {
        INNERMOST_CALL.with(|slot| slot.set(self.previous));
    }
}

/// Create a C function pointer that calls `guest` and has `signature`.
pub fn callback<'s>(
    ffi: &FfiState,
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    signature: &Handle<TypeInfo>,
    guest: &Handle<Value>,
) -> FfiResult<Handle<'s, CValue>> {
    let info = signature.as_tagged(heap);
    if info.kind() != TypeKind::Function {
        return Err(FfiError::NotAFunction(info.id()));
    }
    if info.flags() & FLAG_VARIADIC != 0 {
        return Err(FfiError::VariadicCallback);
    }
    let cif = cif_for_signature(heap, info)?;
    let params = info.params(heap).ok_or(FfiError::NotAFunction(info.id()))?;
    let result = info.result(heap).ok_or(FfiError::NotAFunction(info.id()))?;
    let callback = guest
        .as_tagged(heap)
        .get_as::<Object>(heap)
        .ok_or(FfiError::NotCallable)?;
    if callback.map(heap).kind().is_bound() {
        return Err(FfiError::InvalidArgument(
            "a bound function cannot be used as a callback",
        ));
    }
    // parameters the signature never supplies would always be `undefined`
    if let Some(CallTarget::Bytecode { info, .. }) = guest.as_tagged(heap).call_target(heap) {
        let declared = info.formal_parameter_count();
        if declared > params.len() {
            return Err(FfiError::ArgCount {
                expected: params.len(),
                found: declared,
            });
        }
    }
    let entry: &'static ClosureEntry = Box::leak(Box::new(ClosureEntry {
        // Safety: every value is live and the entry is not shared yet, so
        // there is no old-to-young edge to record.
        params: unsafe { GcSlot::from_value(params.raw()) },
        result: unsafe { GcSlot::from_value(result.raw()) },
        result_size: if result.kind() == TypeKind::Void {
            0
        } else {
            result.size()
        },
        callback: unsafe { GcSlot::from_value(callback.raw()) },
    }));
    let userdata: &'static EntryPtr = Box::leak(Box::new(EntryPtr {
        entry,
        vm: vm.downgrade(),
    }));
    let closure: Closure<'static> = Closure::new(cif, closure_trampoline, userdata);
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

/// Zero the result buffer; libffi does not initialize it, and early returns
/// (a guest throw or a panic) leave it untouched.
fn zero_result(entry: &ClosureEntry, result: &mut u8) {
    if entry.result_size > 0 {
        // Safety: libffi guarantees a result buffer of `result_size` bytes.
        unsafe { core::ptr::write_bytes(result as *mut u8, 0, entry.result_size) };
    }
}

/// Stage the C arguments, run the guest callback, and encode its result.
fn invoke(
    entry: &'static ClosureEntry,
    ffi: &FfiState,
    vm: &VM,
    runtime: &dyn ErasedRuntimeState,
    state: &ThreadState,
    heap: &mut Heap,
    args: *const *const c_void,
    result: &mut u8,
) {
    if state.callback_threw() {
        return;
    }
    state.handle_scope(|scope| {
        let params = entry.params.root_handle();
        let count = params.as_tagged(heap).len() + 1;
        let staged = scope.stage_values(heap, count, |index, heap| {
            if index == 0 {
                return heap.known().undefined.get(heap).erase();
            }
            let ty = params.as_tagged(heap).at(heap, index - 1);
            let pointer = unsafe { *args.add(index - 1) as *const u8 };
            // Safety: libffi passes a pointer to a `size`-byte argument.
            let bytes = unsafe { core::slice::from_raw_parts(pointer, ty.size()) };
            let ty = scope.handle(ty);
            ffi.read_value(heap, &scope, &ty, bytes, None)
        });
        let value = vm.execute(
            runtime,
            &mut *heap,
            state,
            entry.callback.root_handle(),
            staged,
            None,
        );
        let Ok(value) = value else {
            state.set_callback_threw();
            return;
        };
        let value = scope.handle(value);
        let result_ty = entry.result.get(heap);
        if result_ty.kind() == TypeKind::Void {
            return;
        }
        let out = unsafe { core::slice::from_raw_parts_mut(result as *mut u8, result_ty.size()) };
        if encode_value(heap, result_ty, value.as_tagged(heap), out).is_err() {
            state.set_callback_threw();
        }
    });
}

unsafe extern "C" fn closure_trampoline(
    _cif: &libffi::low::ffi_cif,
    result: &mut u8,
    args: *const *const c_void,
    userdata: &EntryPtr,
) {
    let entry = userdata.entry;
    zero_result(entry, result);
    // Safety: the pointers and the guest values stay valid for this call; a
    // panic must never unwind through the C frame.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(ctx) = CallbackCtx::try_current() {
            let (ffi, vm, state, heap) = unsafe { ctx.parts() };
            invoke(entry, ffi, vm, ctx.runtime(), state, heap, args, result);
            return;
        }
        // The callback came from a thread the VM does not know yet (a thread
        // created by C): attach it for the duration, like another runtime's
        // callback entry would.
        let Some(vm) = VM::from_shared_weak(&userdata.vm) else {
            eprintln!("kettevm: callback invoked after its VM was dropped");
            return;
        };
        let mut thread = vm.attach();
        let (vm, heap, state) = thread.split();
        let ffi = vm.ffi();
        let runtime = vm.runtime_by_id(RuntimeId(0));
        let mut ctx = CallbackCtx::new(vm, runtime, heap, state, ffi);
        ctx.enter();
        invoke(entry, ffi, vm, runtime, state, heap, args, result);
        if state.callback_threw() {
            eprintln!("kettevm: uncaught exception in a C-thread callback");
        }
        let _ = state.take_pending_exception();
        drop(ctx);
    }));
    if outcome.is_err() {
        eprintln!("kettevm: panic in a C callback");
    }
}
