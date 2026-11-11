use crate::{
    Args, ErasedRuntimeState, Handle, HandleScope, HandleSlice, Heap, Object, Result, Tagged,
    ThreadState, Thrown, VM, Value, VmError, api::Runtime,
};

/// Return `Err(Thrown)` after materializing `err`.
#[macro_export]
macro_rules! bail {
    ($vm:expr, $heap:expr, $state:expr, $err:expr) => {
        return Err($crate::Errors::throw($vm, $heap, $state, $err))
    };
    ($vm:expr, $heap:expr, $state:expr, $err:expr, $arg:expr) => {
        return Err($crate::Errors::throw_with($vm, $heap, $state, $err, $arg))
    };
}

/// Return the exception sentinel after materializing `err`.
#[macro_export]
macro_rules! raise {
    ($vm:expr, $heap:expr, $state:expr, $err:expr) => {
        return $crate::Errors::raise($vm, $heap, $state, $err)
    };
}

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
        let scope = self.state.scope();
        let HostCtx {
            vm, heap, state, ..
        } = self;
        f(vm, heap, state, scope)
    }

    /// Invoke `callable` ([[Call]]) when `new_target` is `None`, or
    /// [[Construct]] with `new.target` = that handle when `Some`
    /// (ES 9.2.2). The callable is a rooted handle; the result is anchored to
    /// the `heap` borrow and `heap` stays usable. `args` is a call window:
    /// element 0 is the receiver, the rest are the arguments.
    pub fn enter<'h, 'r>(
        vm: &'h VM,
        runtime: &'h dyn ErasedRuntimeState,
        heap: &'h mut Heap,
        state: &'h ThreadState,
        callable: Handle<'_, Value>,
        args: HandleSlice<'r>,
        new_target: Option<Handle<'_, Value>>,
    ) -> Result<Tagged<'h, Value>> {
        let scope = state.scope();
        let Some(callable) = scope.cast::<Object>(heap, callable.as_tagged(heap)) else {
            bail!(vm, heap, state, VmError::NotCallable);
        };
        let new_target = match new_target {
            None => None,
            Some(nt) => {
                // new.target may be exotic (a constructor proxy)
                let nt = nt.as_tagged(heap);
                if !nt.is_strong_ptr() {
                    bail!(vm, heap, state, VmError::NewTargetNotObject);
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
    ) -> Result<Tagged<'h, Value>> {
        state.handle_scope(|scope| {
            let Some(function) = bound.as_tagged(heap).as_heap_object() else {
                bail!(vm, heap, state, VmError::NotCallable);
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
                    Object::construct_forward(vm, heap, state, target, &combined, new_target)
                }
            }
        })
    }
}

/// Values a call supplies beyond its argument list.
///
/// `receiver` is always present (undefiened if not explicit)
/// `callee` is `None` for internal registry calls and native accessors
#[derive(Copy, Clone)]
pub struct CallInfo<'a> {
    /// function object whose `[[Call]]`/`[[Construct]]` is running.
    pub callee: Option<Handle<'a, Value>>,
    /// `this`, or the undefined root when the call has no receiver.
    pub receiver: Handle<'a, Value>,
    /// `new.target` for `[[Construct]]`.
    pub new_target: Option<Handle<'a, Value>>,
}
pub type NativeFn = for<'a, 'i> fn(HostCtx<'a>, CallInfo<'i>, Args) -> Tagged<'a, Value>;

/// Return the exception sentinel for an already-pending exception.
pub fn raise_runtime<'a>(heap: &'a Heap, _thrown: Thrown) -> Tagged<'a, Value> {
    heap.known().exception.get(heap).erase()
}

/// `?` for runtime bodies: fold a `Thrown` into the sentinel channel.
#[macro_export]
macro_rules! rt_try {
    ($heap:expr, $e:expr) => {
        match $e {
            Ok(v) => v,
            Err($crate::Thrown) => return $crate::raise_runtime($heap, $crate::Thrown),
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

/// The current (calling) frame's context: `CallNative` runs in place, so
/// the interpreter's cache still holds the frame executing the call.
pub fn frame_context_value<'a>(state: &ThreadState, heap: &'a Heap) -> Option<Tagged<'a, Value>> {
    state
        .is_frame_active()
        .then(|| state.stack.context(heap, state.frame_base()))
}
