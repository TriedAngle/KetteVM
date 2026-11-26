use crate::{
    Args, AtomicI64Object, AtomicI64ObjectInit, CallInfo, Convert, DenseString, EdgeVisitable,
    Float, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, Object,
    PropertyDescriptor, RuntimeId, SetupError, Smi, Tagged, TaskStart, Termination, ThreadKind,
    ThreadObject, ThreadObjectInit, ThreadState, ThreadStatus, Thrown, VM, Value, Visitor, VmError,
    extend_lifetime, prepare_task, raise, raise_runtime, release_task_run, task_outcome,
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

static NANO_TIME_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Install the `Thread` and `Atomic` globals.
pub fn install(vm: &mut VM) -> Result<(), SetupError> {
    let thread_new_platform_index = vm.register_native(thread_new_platform);
    let thread_new_virtual_index = vm.register_native(thread_new_virtual);
    let thread_start_index = vm.register_native(thread_start);
    let thread_join_index = vm.register_native(thread_join);
    let thread_is_alive_index = vm.register_native(thread_is_alive);
    let thread_name_index = vm.register_native(thread_name);
    let thread_id_index = vm.register_native(thread_id);
    let thread_current_index = vm.register_native(thread_current);
    let thread_interrupt_index = vm.register_native(thread_interrupt);
    let thread_is_interrupted_index = vm.register_native(thread_is_interrupted);
    let thread_mount_index = vm.register_native(thread_mount);
    let thread_park_index = vm.register_native(thread_park);
    let thread_unpark_index = vm.register_native(thread_unpark);
    let thread_nano_time_index = vm.register_native(thread_nano_time);
    let thread_is_ready_index = vm.register_native(thread_is_ready);
    let thread_sleep_index = vm.register_native(thread_sleep);
    let thread_yield_index = vm.register_native(thread_yield);
    let thread_available_processors_index = vm.register_native(thread_available_processors);
    let atomic_i64_new_index = vm.register_native(atomic_i64_new);
    let atomic_i64_load_index = vm.register_native(atomic_i64_load);
    let atomic_i64_store_index = vm.register_native(atomic_i64_store);
    let atomic_i64_swap_index = vm.register_native(atomic_i64_swap);
    let atomic_i64_compare_exchange_index = vm.register_native(atomic_i64_compare_exchange);
    let atomic_i64_add_index = vm.register_native(atomic_i64_add);

    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let (thread_vm, heap, state) = thread.split();
        let object_prototype = heap
            .known()
            .object_prototype
            .get_handle(heap, &scope)
            .erase();
        let make_prototype = |heap: &mut Heap| {
            let map = heap.allocate_handle::<Map>(
                MapInit {
                    kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                    value_slot_count: 0,
                    descriptors: &[],
                    prototype: object_prototype,
                },
                &scope,
            );
            scope.handle(heap.new_object(&scope, map, HandleSlice::EMPTY))
        };

        let thread_prototype = make_prototype(heap);
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "start",
            thread_start_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "join",
            thread_join_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "isAlive",
            thread_is_alive_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "name",
            thread_name_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "threadId",
            thread_id_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "new",
            thread_new_platform_index,
            2,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "newPlatform",
            thread_new_platform_index,
            2,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "newVirtual",
            thread_new_virtual_index,
            2,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "current",
            thread_current_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "mount",
            thread_mount_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "park",
            thread_park_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "parkNanos",
            thread_park_index,
            1,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "unpark",
            thread_unpark_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "nanoTime",
            thread_nano_time_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "isReady",
            thread_is_ready_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "interrupt",
            thread_interrupt_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "isInterrupted",
            thread_is_interrupted_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "sleep",
            thread_sleep_index,
            1,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "yield",
            thread_yield_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            thread_prototype,
            "availableProcessors",
            thread_available_processors_index,
            0,
        );

        let map = heap.known().thread_object_map.get_handle(heap, &scope);
        map.as_tagged(heap).prototype.set(
            heap,
            map.as_tagged(heap).erase(),
            thread_prototype.as_tagged(heap).erase(),
        );

        let atomic_prototype = make_prototype(heap);
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_prototype,
            "load",
            atomic_i64_load_index,
            0,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_prototype,
            "store",
            atomic_i64_store_index,
            1,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_prototype,
            "swap",
            atomic_i64_swap_index,
            1,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_prototype,
            "compareExchange",
            atomic_i64_compare_exchange_index,
            2,
        );
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_prototype,
            "add",
            atomic_i64_add_index,
            1,
        );

        let map = heap.known().atomic_i64_map.get_handle(heap, &scope);
        map.as_tagged(heap).prototype.set(
            heap,
            map.as_tagged(heap).erase(),
            atomic_prototype.as_tagged(heap).erase(),
        );

        let main_name = thread_vm.interner().intern_str(heap, &scope, "main");
        let main_tid = state.id();
        let hole = heap.known().the_hole.get_handle(heap, &scope).erase();
        let main = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Platform,
                target: hole,
                name: main_name.erase(),
                state: ThreadStatus::Running,
                tid: main_tid as i64,
            },
            &scope,
        );
        init_thread_fields(heap, thread_vm, &scope, main);
        let main_object = scope
            .cast::<Object>(heap, main.as_tagged(heap).erase())
            .expect("main thread object");
        let main = main.as_tagged(heap);
        state.set_thread_object(main);
        let atomic_i64 = make_prototype(heap);
        Object::add_method(
            thread_vm,
            heap,
            &scope,
            atomic_i64,
            "new",
            atomic_i64_new_index,
            1,
        );
        let prototype_name = thread_vm.interner().intern_str(heap, &scope, "prototype");
        let prototype_name = scope.handle(prototype_name.as_tagged(heap));
        Object::define_own_property_ordinary(
            heap,
            &scope,
            atomic_i64,
            prototype_name,
            PropertyDescriptor::data(atomic_prototype.erase()),
        )
        .expect("install AtomicI64.prototype");
        let atomic_namespace = make_prototype(heap);
        let atomic_i64_name = thread_vm.interner().intern_str(heap, &scope, "AtomicI64");
        let atomic_i64_name = scope.handle(atomic_i64_name.as_tagged(heap));
        Object::define_own_property_ordinary(
            heap,
            &scope,
            atomic_namespace,
            atomic_i64_name,
            PropertyDescriptor::data(atomic_i64.erase()),
        )
        .expect("install atomic.AtomicI64");
        let global = heap.known().global_object.get_handle(heap, &scope);
        let thread_value = main_object.erase();
        let atomic_value = atomic_namespace.erase();
        for (name, value) in [("Thread", thread_value), ("atomic", atomic_value)] {
            let name = thread_vm.interner().intern_str(heap, &scope, name);
            let name = scope.handle(name.as_tagged(heap));
            Object::define_own_property_ordinary(
                heap,
                &scope,
                global,
                name,
                PropertyDescriptor::data(value),
            )
            .expect("install concurrency global");
        }
        Ok(())
    })
}

#[derive(Default)]
pub struct ThreadRegistry {
    states: Mutex<HashMap<u64, Arc<ThreadState>>>,
    next_id: AtomicUsize,
    attached: Mutex<HashMap<std::thread::ThreadId, Arc<ThreadState>>>,
}

impl ThreadRegistry {
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) as u64
    }

    pub fn register(&self, state: &Arc<ThreadState>) {
        self.states
            .lock()
            .unwrap()
            .insert(state.id(), Arc::clone(state));
    }

    pub fn get(&self, id: u64) -> Option<Arc<ThreadState>> {
        self.states.lock().unwrap().get(&id).cloned()
    }

    pub fn remove(&self, id: u64) -> Option<Arc<ThreadState>> {
        self.states.lock().unwrap().remove(&id)
    }

    pub fn attached_state(&self) -> Option<Arc<ThreadState>> {
        self.attached
            .lock()
            .unwrap()
            .get(&std::thread::current().id())
            .cloned()
    }

    pub fn remember_attached(&self, state: &Arc<ThreadState>) {
        self.attached
            .lock()
            .unwrap()
            .insert(std::thread::current().id(), Arc::clone(state));
    }

    pub fn forget_attached(&self) {
        self.attached
            .lock()
            .unwrap()
            .remove(&std::thread::current().id());
    }

    pub fn visit_edges(&self, visitor: &mut dyn Visitor) {
        for state in self.states.lock().unwrap().values() {
            state.visit_edges(visitor);
        }
    }
}

fn state_for<'a>(
    vm: &VM,
    heap: &'a Heap,
    object: Tagged<'a, ThreadObject>,
) -> Option<Arc<ThreadState>> {
    let id = object.as_ref().tid(heap);
    if id < 0 {
        return None;
    }
    vm.thread_state(id as u64)
}

// Creates a not-yet-started platform thread for `target`.
fn init_thread_fields(
    heap: &mut Heap,
    vm: &VM,
    scope: &HandleScope<'_>,
    thread: Handle<'_, ThreadObject>,
) {
    let thread = scope
        .cast::<Object>(heap, thread.as_tagged(heap).erase())
        .expect("thread object");
    let null = heap.known().null.get_handle(heap, scope).erase();
    for name in ["scheduler", "queued", "waiter"] {
        let name = vm.interner().intern_str(heap, scope, name);
        Object::define_own_property_ordinary(
            heap,
            scope,
            thread,
            scope.handle(name.as_tagged(heap)),
            PropertyDescriptor::data(null),
        )
        .expect("thread field");
    }
}

fn thread_new_platform<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let name = args.get_handle(heap, 0);
        let target = args.get_handle(heap, 1);
        if !target.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::NotCallable);
        }
        let id = vm.next_thread_id();
        let name = if name.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
            name
        } else {
            vm.interner()
                .intern_str(heap, &scope, &format!("thread-{id}"))
                .erase()
        };
        let object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Platform,
                target,
                name,
                state: ThreadStatus::New,
                tid: id as i64,
            },
            &scope,
        );
        init_thread_fields(heap, vm, &scope, object);
        object.as_tagged(heap).erase()
    })
}

// Creates a not-yet-started virtual thread for `target`.
fn thread_new_virtual<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let name = args.get_handle(heap, 0);
        let target = args.get_handle(heap, 1);
        if !target.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::NotCallable);
        }
        let id = vm.next_thread_id();
        let name = if name.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
            name
        } else {
            vm.interner()
                .intern_str(heap, &scope, &format!("thread-{id}"))
                .erase()
        };
        let vm_state = Arc::new(ThreadState::new(heap, id as u64));
        vm.register_thread_state(&vm_state);
        let object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Virtual,
                target,
                name,
                state: ThreadStatus::New,
                tid: id as i64,
            },
            &scope,
        );
        init_thread_fields(heap, vm, &scope, object);
        vm_state.set_thread_object(object.as_tagged(heap));
        object.as_tagged(heap).erase()
    })
}

// Starts the target once; a second call throws.
fn thread_start<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(object) = scope.cast::<ThreadObject>(heap, call.receiver.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let started = if object.as_tagged(heap).is_virtual(heap) {
            ThreadStatus::Ready
        } else {
            ThreadStatus::Running
        };
        if object
            .as_tagged(heap)
            .state
            .compare_exchange_smi(Smi::new(ThreadStatus::New as i64), Smi::new(started as i64))
            .is_err()
        {
            raise!(vm, heap, state, VmError::ThreadAlreadyStarted);
        }
        if object.as_tagged(heap).is_virtual(heap) {
            let runtime = if state.is_frame_active() {
                state
                    .stack()
                    .frame_header(state.frame_base())
                    .code(heap)
                    .runtime()
            } else {
                RuntimeId(0)
            };
            object.as_tagged(heap).set_runtime(runtime);
            return vm.known().undefined.get(heap).erase();
        }
        let id = object.as_tagged(heap).tid(heap) as u64;
        let platform_state = Arc::new(ThreadState::new(heap, id));
        vm.register_thread_state(&platform_state);
        platform_state.set_thread_object(object.as_tagged(heap));
        let runtime = if state.is_frame_active() {
            state
                .stack()
                .frame_header(state.frame_base())
                .code(heap)
                .runtime()
        } else {
            RuntimeId(0)
        };
        let os_name = object
            .as_tagged(heap)
            .name(heap)
            .get_as::<DenseString>(heap)
            .map(|name| name.as_ref().to_rust_string(heap))
            .unwrap_or_else(|| format!("kette-{id}"));
        let vm_for_thread = vm.clone();
        let state_for_thread = Arc::clone(&platform_state);
        let spawned = std::thread::Builder::new().name(os_name).spawn(move || {
            run_thread(vm_for_thread, state_for_thread, runtime);
        });
        match spawned {
            Ok(handle) => {
                // Platform threads are detached; the state monitor is the join path.
                drop(handle);
            }
            Err(_) => {
                object.as_tagged(heap).set_state(ThreadStatus::New);
                vm.remove_thread_state(id);
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        vm.known().undefined.get(heap).erase()
    })
}

fn run_thread(vm: VM, task_state: Arc<ThreadState>, runtime: RuntimeId) {
    let mut thread = vm.attach_with_state(task_state);
    thread.handle_scope(|thread, scope| {
        let callable = {
            let (_, heap, state) = thread.split();
            state
                .thread_object(heap)
                .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
                .and_then(|object| {
                    let target = object.as_ref().target(heap);
                    scope.cast::<Object>(heap, target)
                })
        };
        if let Some(callable) = callable
            && thread.execute(callable, &[], runtime).is_err()
        {
            let (_, heap, state) = thread.split();
            if let Some(exception) = state.take_pending_exception_tagged(heap)
                && let Some(object) = state
                    .thread_object(heap)
                    .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
            {
                object.as_ref().set_exception(heap, exception);
            }
        }
        let (_, _, state) = thread.split();
        state.mark_finished();
        vm.remove_thread_state(state.id());
        vm.notify_thread_event();
    });
}

fn thread_join<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm,
        mut heap,
        state,
        ..
    } = ctx;
    state.handle_scope(move |scope| {
        let Some(object) = scope.cast::<ThreadObject>(heap, call.receiver.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        if object.as_tagged(heap).is_virtual(heap) {
            if object.as_tagged(heap).state(heap) != ThreadStatus::Terminated {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
            if let Some(exception) = object.as_tagged(heap).exception(heap) {
                let exception = scope.handle(exception);
                state.set_pending_exception(exception.as_tagged(heap).erase());
                return raise_runtime(heap, Thrown);
            }
            return vm.known().undefined.get(heap).erase();
        }
        if object.as_tagged(heap).state(heap) == ThreadStatus::New {
            return vm.known().undefined.get(heap).erase();
        }
        if object.as_tagged(heap).state(heap) != ThreadStatus::Terminated
            && let Some(wait_state) = state_for(vm, heap, object.as_tagged(heap))
        {
            wait_state.wait_finished(&mut heap);
        }
        if vm.is_shutdown() {
            return shutdown_now(vm, heap, state);
        }
        if let Some(exception) = object.as_tagged(heap).exception(heap) {
            let exception = scope.handle(exception);
            object.as_tagged(heap).clear_exception(heap);
            state.set_pending_exception(exception.as_tagged(heap).erase());
            return raise_runtime(heap, Thrown);
        }
        vm.known().undefined.get(heap).erase()
    })
}

fn thread_is_alive<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    Convert::boolean(heap, object.as_ref().is_alive(heap))
}

fn thread_name<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    object.as_ref().name(heap)
}

/// Create the guest `Thread` object for a native thread context.
pub fn create_thread_object(vm: &VM, heap: &mut Heap, state: &ThreadState) {
    state.handle_scope(|scope| {
        let id = state.id();
        let name = vm
            .interner()
            .intern_str(heap, &scope, &format!("thread-{id}"))
            .erase();
        let hole = heap.known().the_hole.get_handle(heap, &scope).erase();
        let object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Platform,
                target: hole,
                name,
                state: ThreadStatus::Running,
                tid: id as i64,
            },
            &scope,
        );
        init_thread_fields(heap, vm, &scope, object);
        state.set_thread_object(object.as_tagged(heap));
    });
}

fn thread_id<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    Smi::new(object.as_ref().tid(heap)).into_tagged()
}

fn thread_park<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let has_task = state
        .thread_object(heap)
        .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
        .is_some_and(|object| state_for(vm, heap, object).is_some());
    if !has_task || !state.is_continuation_started() {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    }
    if args.is_empty() {
        state.park(heap);
        return vm.known().undefined.get(heap).erase();
    }
    let Some(nanos) = args.get(heap, 0).to_number(heap) else {
        raise!(vm, heap, state, VmError::CannotConvertToNumber);
    };
    if nanos < 0.0 || !nanos.is_finite() {
        raise!(vm, heap, state, VmError::InvalidTimeValue);
    }
    let timed_out = state.park_timed(heap, Duration::from_nanos(nanos as u64));
    Convert::boolean(heap, timed_out)
}

fn thread_nano_time<'a>(_ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let origin = NANO_TIME_ORIGIN.get_or_init(Instant::now);
    Smi::new(origin.elapsed().as_nanos().max(1) as i64).into_tagged()
}

fn thread_available_processors<'a>(
    _ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    _args: Args,
) -> Tagged<'a, Value> {
    let count = std::thread::available_parallelism().map_or(1, |count| count.get());
    Smi::new(count as i64).into_tagged()
}

fn thread_unpark<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(target) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let Some(task) = state_for(vm, heap, target) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let woke = task.unpark();
    if woke {
        target.as_ref().set_state(ThreadStatus::Ready);
    }
    Convert::boolean(heap, woke)
}

fn thread_is_ready<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    Convert::boolean(
        heap,
        matches!(
            object.as_ref().state(heap),
            ThreadStatus::New | ThreadStatus::Ready
        ),
    )
}

fn thread_mount<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(target) = scope.cast::<ThreadObject>(heap, call.receiver.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        if !target.as_tagged(heap).is_virtual(heap) {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }
        let Some(target_state) = state_for(vm, heap, target.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let status = target.as_tagged(heap).state(heap);
        let mounted = target_state.is_mounted_by_task();
        let runnable = status == ThreadStatus::New
            || status == ThreadStatus::Ready
            || (status == ThreadStatus::Running && mounted);
        let current_task = state
            .thread_object(heap)
            .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
            .is_some_and(|object| state_for(vm, heap, object).is_some());
        if !runnable || !current_task || !state.is_continuation_started() {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }

        if !target_state.begin_mount() {
            panic!("concurrent mount of a virtual thread");
        }
        struct MountGuard<'a>(&'a ThreadState);
        impl Drop for MountGuard<'_> {
            fn drop(&mut self) {
                self.0.end_mount();
            }
        }
        let _mount_guard = MountGuard(&target_state);
        if !target_state.is_continuation_started() {
            let start = TaskStart {
                callable: target.as_tagged(heap).target(heap).raw(),
                args: Vec::new(),
                runtime: target.as_tagged(heap).runtime().unwrap_or(RuntimeId(0)),
            };
            prepare_task(&target_state, vm, start);
        }

        target.as_tagged(heap).set_state(ThreadStatus::Running);
        target_state.set_resume_target(state.continuation_ptr(), true);
        target_state.set_resume_fiber(crate::tsan::current_fiber());
        target_state.run_on_heap(
            heap,
            state.continuation_ptr(),
            target_state.continuation_ptr(),
        );

        // The continuation has yielded: publish the park, and report whether
        // this mount owns resubmission (yield or a permit that arrived while
        // still parking). A fully parked task is owned by its waker.
        let mut resubmit = target_state.after_mount();
        if resubmit {
            target.as_tagged(heap).set_state(ThreadStatus::Ready);
        }

        if let Some(outcome) = task_outcome(&target_state) {
            resubmit = false;
            if outcome.is_err()
                && let Some(exception) = target_state.take_pending_exception_tagged(heap)
            {
                target.as_tagged(heap).set_exception(heap, exception);
            }
            target.as_tagged(heap).set_state(ThreadStatus::Terminated);
            release_task_run(&target_state);
            for slot in target_state.take_waiter_slots() {
                if let Some(waiter) = slot.load(heap) {
                    waiter.set_state(ThreadStatus::Ready);
                }
            }
        }
        Convert::boolean(heap, resubmit)
    })
}

fn thread_interrupt<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    object.as_ref().set_interrupted(true);
    if let Some(task) = state_for(vm, heap, object)
        && task.unpark()
    {
        object.as_ref().set_state(ThreadStatus::Ready);
    }
    vm.known().undefined.get(heap).erase()
}

fn thread_is_interrupted<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    _args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(object) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    Convert::boolean(heap, object.as_ref().is_interrupted(heap))
}

fn thread_current<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if let Some(object) = state.thread_object(heap) {
        return extend_lifetime!(heap, object.erase());
    }
    create_thread_object(vm, heap, state);
    let object = state
        .thread_object(heap)
        .expect("thread object must be installed");
    extend_lifetime!(heap, object.erase())
}

fn thread_sleep<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(millis) = args.get(heap, 0).to_number(heap) else {
        raise!(vm, heap, state, VmError::CannotConvertToNumber);
    };
    if millis < 0.0 || !millis.is_finite() {
        raise!(vm, heap, state, VmError::InvalidTimeValue);
    }
    let total = Duration::from_millis(millis as u64);
    let current = state
        .thread_object(heap)
        .and_then(|object| object.erase().get_as::<ThreadObject>(heap));
    if let Some(current) = current
        && current.as_ref().is_interrupted(heap)
    {
        return vm.known().undefined.get(heap).erase();
    }
    let start = Instant::now();
    loop {
        if vm.is_shutdown() {
            return shutdown_now(vm, heap, state);
        }
        let interrupted = {
            let heap = &*heap;
            state
                .thread_object(heap)
                .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
                .is_some_and(|current| current.as_ref().is_interrupted(heap))
        };
        if interrupted {
            break;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            break;
        }
        let wait = (total - elapsed).min(Duration::from_millis(50));
        heap.block_on(|| vm.wait_thread_event(wait));
    }
    vm.known().undefined.get(heap).erase()
}

fn thread_yield<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let suspended = {
        let current = state
            .thread_object(heap)
            .and_then(|object| object.erase().get_as::<ThreadObject>(heap));
        if let Some(current) = current
            && current.as_ref().is_virtual(heap)
            && state.is_mounted_by_task()
        {
            current.as_ref().set_state(ThreadStatus::Ready);
            true
        } else {
            false
        }
    };
    if suspended {
        state.suspend();
        return vm.known().undefined.get(heap).erase();
    }
    if heap.safepoint_poll() {
        return shutdown_now(vm, heap, state);
    }
    std::thread::yield_now();
    vm.known().undefined.get(heap).erase()
}

fn shutdown_now<'a>(vm: &VM, heap: &'a Heap, state: &ThreadState) -> Tagged<'a, Value> {
    state.set_termination(Termination::Shutdown);
    state.set_pending_exception(vm.known().undefined.get(heap));
    vm.known().exception.get(heap).erase()
}

fn number_to_i64(value: Tagged<Value>, heap: &Heap) -> Option<i64> {
    if let Some(smi) = Smi::decode(value.raw()) {
        return Some(smi.value());
    }
    let number = value.to_number(heap)?;
    if !number.is_finite() || number.fract() != 0.0 {
        return None;
    }
    if number < i64::MIN as f64 || number >= 9_223_372_036_854_775_808.0 {
        return None;
    }
    Some(number as i64)
}

fn i64_to_value<'s>(heap: &mut Heap, scope: &'s HandleScope<'_>, value: i64) -> Handle<'s, Value> {
    if Smi::in_range(value) {
        return scope.handle(Smi::new(value).into_tagged());
    }
    heap.allocate_handle::<Float>(value as f64, scope).erase()
}

fn atomic_i64_new<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, state, .. } = ctx;
    state.handle_scope(|scope| {
        let Some(value) = number_to_i64(args.get(heap, 0), heap) else {
            raise!(ctx.vm, heap, state, VmError::AtomicNonInteger);
        };
        let cell = heap.allocate_handle::<AtomicI64Object>(AtomicI64ObjectInit { value }, &scope);
        cell.as_tagged(heap).erase()
    })
}

fn atomic_i64_load<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(cell) = call
        .receiver
        .as_tagged(heap)
        .get_as::<AtomicI64Object>(heap)
    else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let value = cell.as_ref().value();
    state.handle_scope(|scope| i64_to_value(heap, &scope, value).as_tagged(heap).erase())
}

fn atomic_i64_store<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(cell) = call
        .receiver
        .as_tagged(heap)
        .get_as::<AtomicI64Object>(heap)
    else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let value = args.get_handle(heap, 0);
    let Some(value) = number_to_i64(value.as_tagged(heap), heap) else {
        raise!(vm, heap, state, VmError::AtomicNonInteger);
    };
    cell.as_ref().value.store(value, Ordering::SeqCst);
    state.handle_scope(|scope| i64_to_value(heap, &scope, value).as_tagged(heap).erase())
}

fn atomic_i64_swap<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(cell) = call
        .receiver
        .as_tagged(heap)
        .get_as::<AtomicI64Object>(heap)
    else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let Some(value) = number_to_i64(args.get(heap, 0), heap) else {
        raise!(vm, heap, state, VmError::AtomicNonInteger);
    };
    let old = cell.as_ref().value.swap(value, Ordering::SeqCst);
    state.handle_scope(|scope| i64_to_value(heap, &scope, old).as_tagged(heap).erase())
}

fn atomic_i64_compare_exchange<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(cell) = call
        .receiver
        .as_tagged(heap)
        .get_as::<AtomicI64Object>(heap)
    else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let Some(expected) = number_to_i64(args.get(heap, 0), heap) else {
        raise!(vm, heap, state, VmError::AtomicNonInteger);
    };
    let Some(new) = number_to_i64(args.get(heap, 1), heap) else {
        raise!(vm, heap, state, VmError::AtomicNonInteger);
    };
    let observed = match cell.as_ref().value.compare_exchange(
        expected,
        new,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(old) | Err(old) => old,
    };
    state.handle_scope(|scope| i64_to_value(heap, &scope, observed).as_tagged(heap).erase())
}

fn atomic_i64_add<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(cell) = call
        .receiver
        .as_tagged(heap)
        .get_as::<AtomicI64Object>(heap)
    else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let Some(delta) = number_to_i64(args.get(heap, 0), heap) else {
        raise!(vm, heap, state, VmError::AtomicNonInteger);
    };
    let old = cell.as_ref().value.fetch_add(delta, Ordering::SeqCst);
    let new = old.wrapping_add(delta);
    state.handle_scope(|scope| i64_to_value(heap, &scope, new).as_tagged(heap).erase())
}
