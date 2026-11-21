use crate::simple_scheduler::{Scheduler, SchedulerObject, SchedulerObjectInit};
use crate::{
    Args, AtomicI64Object, AtomicI64ObjectInit, AtomicOptionGcSlot, CallInfo, Convert, DenseString,
    Float, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, Object,
    PropertyDescriptor, RuntimeId, SetupError, Smi, Tagged, TaskStart, TaskStateObject,
    TaskStateObjectInit, Termination, ThreadKind, ThreadObject, ThreadObjectInit, ThreadState,
    ThreadStatus, Thrown, VM, Value, Visitor, VmError, extend_lifetime, prepare_task, raise,
    raise_runtime, release_task_run, task_outcome,
};
use std::{
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
    let thread_scheduler_index = vm.register_native(thread_scheduler);
    let thread_interrupt_index = vm.register_native(thread_interrupt);
    let thread_is_interrupted_index = vm.register_native(thread_is_interrupted);
    let thread_mount_index = vm.register_native(thread_mount);
    let thread_park_index = vm.register_native(thread_park);
    let thread_unpark_index = vm.register_native(thread_unpark);
    let thread_nano_time_index = vm.register_native(thread_nano_time);
    let thread_is_ready_index = vm.register_native(thread_is_ready);
    let thread_sleep_index = vm.register_native(thread_sleep);
    let thread_yield_index = vm.register_native(thread_yield);
    let atomic_i64_new_index = vm.register_native(atomic_i64_new);
    let atomic_i64_load_index = vm.register_native(atomic_i64_load);
    let atomic_i64_store_index = vm.register_native(atomic_i64_store);
    let atomic_i64_swap_index = vm.register_native(atomic_i64_swap);
    let atomic_i64_compare_exchange_index = vm.register_native(atomic_i64_compare_exchange);
    let atomic_i64_add_index = vm.register_native(atomic_i64_add);

    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let (thread_vm, heap, state) = thread.split();
        let default_scheduler = heap.allocate_handle::<SchedulerObject>(
            SchedulerObjectInit {
                scheduler: Arc::new(Scheduler::new()),
            },
            &scope,
        );
        vm.set_default_scheduler(default_scheduler.as_tagged(heap));
        vm.register_scheduler(default_scheduler.as_tagged(heap));
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
            3,
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
            "scheduler",
            thread_scheduler_index,
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
        let main_tid = vm.threads().next_name();
        let hole = heap.known().the_hole.get_handle(heap, &scope).erase();
        let main = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Platform,
                target: hole,
                name: main_name.erase(),
                task: None,
                state: ThreadStatus::Running,
                tid: main_tid as i64,
                platform_slot: -1,
            },
            &scope,
        );
        let main_object = scope
            .cast::<Object>(heap, main.as_tagged(heap).erase())
            .expect("main thread object");
        let main = main.as_tagged(heap);
        vm.threads().set_main(main);
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

// Host state of one started thread; the roots keep it alive across the boundary.
pub struct ThreadSlot {
    object: AtomicOptionGcSlot<Object>,
    target: AtomicOptionGcSlot<Value>,
    exception: AtomicOptionGcSlot<Value>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ThreadSlot {
    fn new(heap: &Heap) -> Self {
        let hole = heap.known().the_hole.raw();
        Self {
            // Safety: all slots start as the hole and are visited as roots.
            object: unsafe { AtomicOptionGcSlot::from_value(hole) },
            target: unsafe { AtomicOptionGcSlot::from_value(hole) },
            exception: unsafe { AtomicOptionGcSlot::from_value(hole) },
            handle: Mutex::new(None),
        }
    }
}

pub struct ThreadRegistry {
    threads: Mutex<Vec<Arc<ThreadSlot>>>,
    next_name: AtomicUsize,
    main: AtomicOptionGcSlot<ThreadObject>,
    /// Thread state attached by each host thread.
    attached: Mutex<Vec<(std::thread::ThreadId, Arc<ThreadState>)>>,
}

impl Default for ThreadRegistry {
    fn default() -> Self {
        Self {
            threads: Mutex::new(Vec::new()),
            next_name: AtomicUsize::new(0),
            main: AtomicOptionGcSlot::uninit(),
            attached: Mutex::new(Vec::new()),
        }
    }
}

impl ThreadRegistry {
    fn insert(&self, slot: Arc<ThreadSlot>) -> usize {
        let mut threads = self.threads.lock().unwrap();
        let id = threads.len();
        threads.push(slot);
        id
    }

    fn get(&self, id: usize) -> Option<Arc<ThreadSlot>> {
        self.threads.lock().unwrap().get(id).cloned()
    }

    fn next_name(&self) -> usize {
        self.next_name.fetch_add(1, Ordering::Relaxed)
    }

    fn set_main(&self, main: Tagged<'_, ThreadObject>) {
        self.main.store(main);
    }

    pub fn main<'a>(&self, heap: &'a Heap) -> Tagged<'a, ThreadObject> {
        self.main.load(heap).expect("main thread not installed")
    }

    pub fn attached_state(&self) -> Option<Arc<ThreadState>> {
        let host = std::thread::current().id();
        self.attached
            .lock()
            .unwrap()
            .iter()
            .find_map(|(id, state)| (*id == host).then(|| Arc::clone(state)))
    }

    pub fn remember_attached(&self, state: &Arc<ThreadState>) {
        self.attached
            .lock()
            .unwrap()
            .push((std::thread::current().id(), Arc::clone(state)));
    }

    pub fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.main.as_raw());
        for slot in self.threads.lock().unwrap().iter() {
            visitor.visit(slot.object.as_raw());
            visitor.visit(slot.target.as_raw());
            visitor.visit(slot.exception.as_raw());
        }
    }
}

fn default_scheduler(vm: &VM, heap: &Heap) -> Arc<Scheduler> {
    let scheduler = vm.default_scheduler(heap);
    Arc::clone(scheduler.as_ref().scheduler())
}

fn task_scheduler(vm: &VM, heap: &Heap, state: &ThreadState) -> Arc<Scheduler> {
    state
        .thread_object(heap)
        .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
        .and_then(|object| object.task(heap))
        .and_then(|context| context.scheduler(heap))
        .map(|scheduler| Arc::clone(scheduler.as_ref().scheduler()))
        .unwrap_or_else(|| default_scheduler(vm, heap))
}

// Creates a not-yet-started platform thread for `target`.
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
        let id = vm.threads().next_name();
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
                task: None,
                state: ThreadStatus::New,
                tid: id as i64,
                platform_slot: -1,
            },
            &scope,
        );
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
        let first = args.get_handle(heap, 1);
        let second = args.get_handle(heap, 2);
        let undefined = heap.known().undefined.get(heap);
        let (target, scheduler) = if first.as_tagged(heap) == undefined {
            (second, scope.handle(vm.default_scheduler(heap)))
        } else if second.as_tagged(heap) == undefined && first.as_tagged(heap).is_callable(heap) {
            (first, scope.handle(vm.default_scheduler(heap)))
        } else {
            let Some(scheduler) = first.as_tagged(heap).get_as::<SchedulerObject>(heap) else {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            };
            (second, scope.handle(scheduler))
        };
        if !target.as_tagged(heap).is_callable(heap) {
            raise!(vm, heap, state, VmError::NotCallable);
        }
        let id = vm.threads().next_name();
        let name = if name.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
            name
        } else {
            vm.interner()
                .intern_str(heap, &scope, &format!("thread-{id}"))
                .erase()
        };
        let vm_state = Arc::new(ThreadState::new(heap));
        vm.register_thread_state(&vm_state);
        let context = heap.allocate_handle::<TaskStateObject>(
            TaskStateObjectInit {
                state: Arc::clone(&vm_state),
            },
            &scope,
        );
        let object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                kind: ThreadKind::Virtual,
                target,
                name,
                task: Some(context),
                state: ThreadStatus::New,
                tid: id as i64,
                platform_slot: -1,
            },
            &scope,
        );
        context
            .as_tagged(heap)
            .set_scheduler(heap, scheduler.as_tagged(heap));
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
            let Some(context) = object.as_tagged(heap).task(heap) else {
                raise!(vm, heap, state, VmError::InvalidOperandType);
            };
            let runtime = if state.is_frame_active() {
                state
                    .stack()
                    .frame_header(state.frame_base())
                    .code(heap)
                    .runtime()
            } else {
                RuntimeId(0)
            };
            context.as_ref().set_runtime(runtime);
            let target_scheduler = context
                .as_ref()
                .scheduler(heap)
                .map(|scheduler| Arc::clone(scheduler.as_ref().scheduler()))
                .unwrap_or_else(|| default_scheduler(vm, heap));
            target_scheduler.push_ready(heap, object.as_tagged(heap));
            return vm.known().undefined.get(heap).erase();
        }
        let target = object.as_tagged(heap).target(heap);
        let slot = Arc::new(ThreadSlot::new(heap));
        // Register the roots before the child can run, so the GC sees them.
        let id = vm.threads().insert(Arc::clone(&slot));
        slot.object.store(object.as_tagged(heap).erase());
        slot.target.store(target);
        object.as_tagged(heap).set_platform_slot(id as i64);
        let runtime = if state.is_frame_active() {
            state
                .stack()
                .frame_header(state.frame_base())
                .code(heap)
                .runtime()
        } else {
            RuntimeId(0)
        };
        let vm_for_thread = vm.clone();
        let slot_for_thread = Arc::clone(&slot);
        let spawned = std::thread::Builder::new().spawn(move || {
            run_thread(vm_for_thread, slot_for_thread, runtime);
        });
        match spawned {
            Ok(handle) => {
                slot.handle.lock().unwrap().replace(handle);
            }
            Err(_) => {
                object.as_tagged(heap).set_state(ThreadStatus::New);
                object.as_tagged(heap).set_platform_slot(-1);
                slot.object.clear(heap);
                slot.target.clear(heap);
                raise!(vm, heap, state, VmError::InvalidOperandType);
            }
        }
        vm.known().undefined.get(heap).erase()
    })
}

fn run_thread(vm: VM, slot: Arc<ThreadSlot>, runtime: RuntimeId) {
    let mut thread = vm.attach();
    thread.handle_scope(|thread, scope| {
        let (_, heap, state) = thread.split();
        if let Some(object) = slot.object.load(heap)
            && let Some(object) = object.erase().get_as::<ThreadObject>(heap)
        {
            state.set_thread_object(object);
        }
        let callable = {
            let heap = thread.heap();
            slot.target
                .load(heap)
                .and_then(|target| scope.cast::<Object>(heap, target))
        };
        if let Some(callable) = callable
            && thread.execute(callable, &[], runtime).is_err()
        {
            let (_, heap, state) = thread.split();
            if let Some(exception) = state.take_pending_exception_tagged(heap) {
                slot.exception.store(exception);
            }
        }
        {
            let (_, heap, _) = thread.split();
            if let Some(object) = slot.object.load(heap)
                && let Some(object) = object.erase().get_as::<ThreadObject>(heap)
            {
                object.as_ref().set_state(ThreadStatus::Terminated);
            }
            slot.target
                .store_word(heap.known().the_hole.raw().to_bits());
            slot.object.clear(heap);
        }
        vm.notify_thread_event();
    });
}

// Waits for the thread to terminate.
fn thread_join<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(object) = scope.cast::<ThreadObject>(heap, call.receiver.as_tagged(heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        if object.as_tagged(heap).is_virtual(heap) {
            if object.as_tagged(heap).state(heap) == ThreadStatus::New {
                return vm.known().undefined.get(heap).erase();
            }
            let Some(context) = object.as_tagged(heap).task(heap) else {
                return vm.known().undefined.get(heap).erase();
            };
            if object.as_tagged(heap).state(heap) == ThreadStatus::Terminated {
                if let Some(exception) = context.as_ref().exception(heap) {
                    let exception = scope.handle(exception);
                    state.set_pending_exception(exception.as_tagged(heap).erase());
                    return raise_runtime(heap, Thrown);
                }
                return vm.known().undefined.get(heap).erase();
            }
            let Some(current) = state
                .thread_object(heap)
                .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
            else {
                return vm.known().undefined.get(heap).erase();
            };
            if current.as_ref().is_interrupted(heap) {
                return vm.known().undefined.get(heap).erase();
            }
            let target_scheduler = context
                .as_ref()
                .scheduler(heap)
                .map(|scheduler| Arc::clone(scheduler.as_ref().scheduler()))
                .unwrap_or_else(|| default_scheduler(vm, heap));
            if target_scheduler.add_waiter_and_block(heap, object.as_tagged(heap), current) {
                state.suspend(heap);
                if let Some(task) = state
                    .thread_object(heap)
                    .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
                    .and_then(|object| object.task(heap))
                {
                    task.as_ref().clear_waiting_on(heap);
                }
                if object.as_tagged(heap).state(heap) == ThreadStatus::Terminated
                    && let Some(task) = object.as_tagged(heap).task(heap)
                    && let Some(exception) = task.as_ref().exception(heap)
                {
                    let exception = scope.handle(exception);
                    state.set_pending_exception(exception.as_tagged(heap).erase());
                    return raise_runtime(heap, Thrown);
                }
            } else if let Some(exception) = context.as_ref().exception(heap) {
                let exception = scope.handle(exception);
                state.set_pending_exception(exception.as_tagged(heap).erase());
                return raise_runtime(heap, Thrown);
            }
            return vm.known().undefined.get(heap).erase();
        }
        let (id, started) = {
            let heap = &*heap;
            let object = object.as_tagged(heap);
            (
                object.platform_slot(heap),
                object.state(heap) != ThreadStatus::New,
            )
        };
        if id < 0 || !started {
            return vm.known().undefined.get(heap).erase();
        }
        let Some(slot) = vm.threads().get(id as usize) else {
            return vm.known().undefined.get(heap).erase();
        };
        loop {
            let (done, interrupted) = {
                let heap = &*heap;
                let current = state
                    .thread_object(heap)
                    .and_then(|object| object.erase().get_as::<ThreadObject>(heap));
                (
                    object.as_tagged(heap).state(heap) == ThreadStatus::Terminated,
                    current.is_some_and(|current| current.as_ref().is_interrupted(heap)),
                )
            };
            if done || interrupted || vm.is_shutdown() {
                break;
            }
            heap.block_on(|| vm.wait_thread_event(Duration::from_millis(50)));
        }
        if let Some(handle) = slot.handle.lock().unwrap().take() {
            heap.block_on(|| {
                let _ = handle.join();
            });
        }
        if vm.is_shutdown() {
            return shutdown_now(vm, heap, state);
        }
        if let Some(exception) = slot.exception.load(heap) {
            let exception = scope.handle(exception);
            slot.exception
                .store_word(vm.known().the_hole.raw().to_bits());
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
/// It is not joinable; `platform_slot` stays -1.
pub fn create_thread_object(vm: &VM, heap: &mut Heap, state: &ThreadState) {
    state.handle_scope(|scope| {
        let id = vm.threads().next_name();
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
                task: None,
                state: ThreadStatus::Running,
                tid: id as i64,
                platform_slot: -1,
            },
            &scope,
        );
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
        .is_some_and(|object| object.as_ref().task(heap).is_some());
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
    let scheduler = task_scheduler(vm, heap, state);
    let timed_out = state.park_timed(heap, &scheduler, Duration::from_nanos(nanos as u64));
    Convert::boolean(heap, timed_out)
}

fn thread_nano_time<'a>(_ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let origin = NANO_TIME_ORIGIN.get_or_init(Instant::now);
    Smi::new(origin.elapsed().as_nanos().max(1) as i64).into_tagged()
}

fn thread_unpark<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(target) = call.receiver.as_tagged(heap).get_as::<ThreadObject>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let Some(task) = target.as_ref().task(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType);
    };
    let target_state = Arc::clone(task.as_ref().state());
    if target_state.unpark() {
        if let Some(scheduler) = task.as_ref().scheduler(heap) {
            scheduler
                .as_ref()
                .scheduler()
                .cancel_park_timer(heap, target);
        }
        if target_state.is_mounted_by_task() {
            target.as_ref().set_state(ThreadStatus::Ready);
        } else if let Some(scheduler) = task.as_ref().scheduler(heap) {
            scheduler.as_ref().scheduler().push_ready(heap, target);
        } else {
            target.as_ref().set_state(ThreadStatus::Ready);
        }
    }
    vm.known().undefined.get(heap).erase()
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
        let Some(task) = target.as_tagged(heap).task(heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        };
        let target_state = Arc::clone(task.as_ref().state());
        let task = scope.handle(task);
        let status = target.as_tagged(heap).state(heap);
        let mounted = target_state.is_mounted_by_task();
        let runnable = status == ThreadStatus::New
            || (matches!(status, ThreadStatus::Ready | ThreadStatus::Running) && mounted);
        let current_task = state
            .thread_object(heap)
            .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
            .and_then(|object| object.task(heap));
        if !runnable || current_task.is_none() || !state.is_continuation_started() {
            raise!(vm, heap, state, VmError::InvalidOperandType);
        }

        if !target_state.is_continuation_started() {
            let start = TaskStart {
                callable: target.as_tagged(heap).target(heap).raw(),
                args: Vec::new(),
                runtime: task
                    .as_tagged(heap)
                    .as_ref()
                    .runtime()
                    .unwrap_or(RuntimeId(0)),
            };
            prepare_task(&target_state, vm, heap, start);
        }

        target.as_tagged(heap).set_state(ThreadStatus::Running);
        target_state.set_resume_target(state.continuation_ptr(), true);
        unsafe {
            crate::continuation::switch(state.continuation_ptr(), target_state.continuation_ptr())
        };

        if let Some(outcome) = task_outcome(&target_state) {
            if outcome.is_err()
                && let Some(exception) = target_state.take_pending_exception_tagged(heap)
            {
                task.as_tagged(heap).as_ref().set_exception(heap, exception);
            }
            target.as_tagged(heap).set_state(ThreadStatus::Terminated);
            release_task_run(&target_state);
            for slot in target_state.take_waiter_slots() {
                if let Some(waiter) = slot.load(heap)
                    && let Some(waiter_task) = waiter.task(heap)
                {
                    waiter_task.clear_waiting_on(heap);
                    if waiter_task.as_ref().state().is_mounted_by_task() {
                        waiter.set_state(ThreadStatus::Ready);
                    } else if let Some(scheduler) = waiter_task.as_ref().scheduler(heap) {
                        scheduler.as_ref().scheduler().push_ready(heap, waiter);
                    } else {
                        waiter.set_state(ThreadStatus::Ready);
                    }
                }
            }
        }
        vm.known().undefined.get(heap).erase()
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
    if let Some(task) = object.as_ref().task(heap)
        && let Some(scheduler) = task.as_ref().scheduler(heap)
    {
        scheduler.as_ref().scheduler().wake_blocked(heap, object);
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

fn thread_scheduler<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if let Some(scheduler) = state
        .thread_object(heap)
        .and_then(|object| object.erase().get_as::<ThreadObject>(heap))
        .and_then(|object| object.task(heap))
        .and_then(|context| context.scheduler(heap))
    {
        return scheduler.erase();
    }
    vm.default_scheduler(heap).erase()
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
    if let Some(current) = current
        && current.as_ref().task(heap).is_some()
    {
        let scheduler = task_scheduler(vm, heap, state);
        if total.is_zero() {
            scheduler.yield_current(heap, current);
        } else {
            scheduler.sleep_current(heap, current, total);
        }
        state.suspend(heap);
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
    let current = state
        .thread_object(heap)
        .and_then(|object| object.erase().get_as::<ThreadObject>(heap));
    if let Some(current) = current
        && current.as_ref().task(heap).is_some()
    {
        if state.is_mounted_by_task() {
            current.as_ref().set_state(ThreadStatus::Ready);
        } else {
            task_scheduler(vm, heap, state).yield_current(heap, current);
        }
        state.suspend(heap);
    } else {
        std::thread::yield_now();
    }
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
