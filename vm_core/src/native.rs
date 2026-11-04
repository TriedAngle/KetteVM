use crate::api::Runtime;
use crate::errors::Errors;
use crate::{
    Args, Coercion, ErasedRuntimeState, Handle, HandleScope, HandleSlice, Heap, Object, Tagged,
    ThreadState, VM, Value, VmError,
};
use core::ptr::NonNull;

// -- native calling convention --

pub struct HostCtx<'a> {
    pub vm: &'a VM,
    /// The runtime whose code is executing: the frame's runtime, or
    /// `VmCore` when the call did not come from a front end's frame.
    pub runtime: &'a dyn ErasedRuntimeState,
    pub heap: &'a mut Heap,
    pub state: &'a ThreadState,
}

impl<'a> HostCtx<'a> {
    pub fn new(
        vm: &'a VM,
        runtime: &'a dyn ErasedRuntimeState,
        heap: &'a mut Heap,
        state: &'a ThreadState,
    ) -> Self {
        Self {
            vm,
            runtime,
            heap,
            state,
        }
    }

    /// The state runtime `R` stored during `VM::add`.
    #[inline(always)]
    pub fn runtime<R: Runtime>(&self) -> &'a R {
        debug_assert!(
            self.runtime.as_any().is::<R>(),
            "native read a foreign runtime's state"
        );
        // Safety: the executing runtime is `R` (asserted in debug builds).
        unsafe { &*(self.runtime as *const dyn ErasedRuntimeState as *const R) }
    }

    /// Root a handle scope and hand the closure the context parts at their
    /// real (heap) lifetime, so values produced inside are `Tagged<'a>`.
    #[inline(always)]
    pub fn handle_scope<R>(
        self,
        f: impl FnOnce(&'a VM, &'a mut Heap, &'a ThreadState, HandleScope<'_>) -> R,
    ) -> R {
        let scope = unsafe { HandleScope::from_raw(NonNull::from(&self.state.handles)) };
        let HostCtx {
            vm, heap, state, ..
        } = self;
        f(vm, heap, state, scope)
    }

    /// Invoke `callable` ([[Call]]) when `new_target` is `None`, or
    /// [[Construct]] with `new.target` = that handle when `Some`
    /// (ES 9.2.2). The callable is a rooted handle; the result is anchored to
    /// the `heap` borrow and `heap` stays usable.
    pub fn enter<'h, 'r>(
        vm: &'h VM,
        runtime: &'h dyn ErasedRuntimeState,
        heap: &'h mut Heap,
        state: &'h ThreadState,
        callable: Handle<'_, Value>,
        args: HandleSlice<'r>,
        new_target: Option<Handle<'_, Value>>,
    ) -> Result<Tagged<'h, Value>, VmError> {
        let scope = unsafe { HandleScope::from_raw(NonNull::from(&state.handles)) };
        let Some(callable) = scope.cast::<Object>(heap, callable.as_tagged(heap)) else {
            return Err(VmError::Type);
        };
        let new_target = match new_target {
            None => None,
            Some(nt) => {
                // new.target may be exotic (a constructor proxy)
                let nt = nt.as_tagged(heap);
                if !nt.is_strong_ptr() {
                    return Err(VmError::Type);
                }
                Some(scope.handle(nt))
            }
        };
        let is_bound = callable.as_tagged(heap).map(heap).kind().is_bound();
        if is_bound {
            let forwarded: Vec<Handle<'_, Value>> = (1..args.len())
                .filter_map(|index| args.get(index))
                .collect();
            return Self::enter_bound(
                vm,
                runtime,
                heap,
                state,
                callable.erase(),
                &forwarded,
                new_target,
            );
        }
        (vm.shared.execute)(vm, runtime, heap, state, callable, args, new_target)
    }

    /// A bound function's `[[Call]]`/`[[Construct]]` (ES 10.4.2): appends
    /// `args` (the call's arguments, without a receiver) to `[[BoundArguments]]`
    /// and forwards to `[[BoundTargetFunction]]`. A call uses `[[BoundThis]]`
    /// as the receiver; a construct replaces `new.target` with the target when
    /// the bound function itself is being constructed.
    pub fn enter_bound<'h>(
        vm: &'h VM,
        runtime: &'h dyn ErasedRuntimeState,
        heap: &'h mut Heap,
        state: &'h ThreadState,
        bound: Handle<'_, Value>,
        args: &[Handle<'_, Value>],
        new_target: Option<Handle<'_, Value>>,
    ) -> Result<Tagged<'h, Value>, VmError> {
        state.handle_scope(|scope| {
            let Some(function) = bound.as_tagged(heap).as_heap_object() else {
                return Err(VmError::Type);
            };
            let slots = function.slots(heap);
            let target = scope.handle(slots.at(heap, 0));
            let bound_this = scope.handle(slots.at(heap, 1));
            let bound_args = scope.handle(slots.at(heap, 2));
            let mut combined: Vec<Handle<'_, Value>> = Vec::new();
            if let Some(arguments) = bound_args.as_tagged(heap).as_heap_object() {
                for index in 0..arguments.as_ref().length() {
                    if let Some(argument) = arguments.as_ref().element_value(heap, index) {
                        combined.push(scope.handle(argument));
                    }
                }
            }
            for arg in args {
                combined.push(scope.handle(arg.as_tagged(heap)));
            }
            match new_target {
                None => {
                    combined.insert(0, bound_this);
                    let staged = scope.stage(
                        &combined
                            .iter()
                            .map(|arg| arg.as_tagged(heap))
                            .collect::<Vec<_>>(),
                    );
                    Self::enter(vm, runtime, heap, state, target, staged, None)
                }
                Some(new_target) => {
                    let new_target =
                        scope.handle(if new_target.as_tagged(heap) == bound.as_tagged(heap) {
                            target.as_tagged(heap)
                        } else {
                            new_target.as_tagged(heap)
                        });
                    let result = match Object::construct_forward(
                        vm, heap, state, target, &combined, new_target,
                    )? {
                        Coercion::Threw => heap.known().exception.get_handle(heap, &scope).erase(),
                        Coercion::Value(value) => scope.handle(value),
                    };
                    Ok(result.as_tagged(heap).erase())
                }
            }
        })
    }
}

/// The native-call ABI (tier 1)
pub type NativeFn =
    for<'a, 'nt> fn(HostCtx<'a>, Option<Handle<'nt, Value>>, Args) -> Tagged<'a, Value>;

/// Materialize `err` as the pending exception and return the sentinel
pub fn raise_runtime<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    err: VmError,
) -> Tagged<'a, Value> {
    let ex =
        Errors::from_vm_error(vm, heap, state, err).expect("error materialization must not fail");
    state.set_pending_exception(ex);
    state.set_last_error(err);
    vm.known().exception.get(heap).erase()
}

/// `?` for runtime bodies: fold an internal `Result` into the sentinel
/// channel via [`raise_runtime`].
#[macro_export]
macro_rules! rt_try {
    ($vm:expr, $heap:expr, $state:expr, $e:expr) => {
        match $e {
            Ok(v) => v,
            Err(err) => return $crate::raise_runtime($vm, $heap, $state, err),
        }
    };
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NativeIndex(pub usize);

impl From<NativeIndex> for u32 {
    fn from(index: NativeIndex) -> Self {
        index.0 as u32
    }
}

pub struct NativeRegistry {
    entries: Vec<NativeFn>,
}

impl NativeRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn insert(&mut self, f: NativeFn) -> NativeIndex {
        let index = NativeIndex(self.entries.len());
        self.entries.push(f);
        index
    }

    pub fn get(&self, index: NativeIndex) -> Option<NativeFn> {
        self.entries.get(index.0).copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for NativeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The running frame's context (CallNative runs in place, so the
/// interpreter's cache still holds the frame executing the call).
/// The current (calling) frame's context: `CallNative` runs in place, so
/// the interpreter's cache still holds the frame executing the call.
pub fn frame_context_value<'a>(
    state: &ThreadState,
    heap: &'a Heap,
) -> Result<Tagged<'a, Value>, VmError> {
    if !state.is_frame_active() {
        return Err(VmError::Type);
    }
    Ok(state.stack.context(heap, state.frame_base()))
}
