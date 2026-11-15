use crate::{
    Args, AtomicI64Object, AtomicI64ObjectInit, AtomicOptionGcSlot, CallInfo, Convert, DenseString,
    Float, Handle, HandleScope, HandleSlice, Heap, HostCtx, Map, MapInit, MapKind, Object,
    PropertyDescriptor, RuntimeId, SetupError, Smi, Tagged, Termination, Thread, ThreadObject,
    ThreadObjectInit, ThreadState, VM, Value, Visitor, VmError, extend_lifetime,
    objects::thread::{NEW, RUNNABLE, TERMINATED},
    raise,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Install the `Thread` and `Atomic` globals.
pub fn install(vm: &mut VM) -> Result<(), SetupError> {
    let thread_new_index = vm.register_native(thread_new);
    let thread_start_index = vm.register_native(thread_start);
    let thread_join_index = vm.register_native(thread_join);
    let thread_is_alive_index = vm.register_native(thread_is_alive);
    let thread_name_index = vm.register_native(thread_name);
    let thread_current_index = vm.register_native(thread_current);
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
        let heap = thread.heap();
        let object_prototype = heap
            .known()
            .object_prototype
            .get_handle(heap, &scope)
            .erase();
        let make_prototype = |thread: &mut Thread| {
            let heap = thread.heap();
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

        let thread_prototype = make_prototype(thread);
        Object::install_method(
            thread,
            &scope,
            thread_prototype,
            "start",
            thread_start_index,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            thread_prototype,
            "join",
            thread_join_index,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            thread_prototype,
            "isAlive",
            thread_is_alive_index,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            thread_prototype,
            "name",
            thread_name_index,
            0,
        );
        {
            let heap = thread.heap();
            let map = heap.known().thread_object_map.get_handle(heap, &scope);
            map.as_tagged(heap).prototype.set(
                heap,
                map.as_tagged(heap).erase(),
                thread_prototype.as_tagged(heap).erase(),
            );
        }

        let atomic_prototype = make_prototype(thread);
        Object::install_method(
            thread,
            &scope,
            atomic_prototype,
            "load",
            atomic_i64_load_index,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            atomic_prototype,
            "store",
            atomic_i64_store_index,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            atomic_prototype,
            "swap",
            atomic_i64_swap_index,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            atomic_prototype,
            "compareExchange",
            atomic_i64_compare_exchange_index,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            atomic_prototype,
            "add",
            atomic_i64_add_index,
            1,
        );
        {
            let heap = thread.heap();
            let map = heap.known().atomic_i64_map.get_handle(heap, &scope);
            map.as_tagged(heap).prototype.set(
                heap,
                map.as_tagged(heap).erase(),
                atomic_prototype.as_tagged(heap).erase(),
            );
        }

        let main_name = thread.intern(&scope, "main");
        let main = {
            let heap = thread.heap();
            let hole = heap.known().the_hole.get_handle(heap, &scope).erase();
            heap.allocate_handle::<ThreadObject>(
                ThreadObjectInit {
                    target: hole,
                    name: main_name.erase(),
                    state: RUNNABLE,
                    slot: -1,
                },
                &scope,
            )
        };
        let main_object = {
            let heap = thread.heap();
            scope
                .cast::<Object>(heap, main.as_tagged(heap).erase())
                .expect("main thread object")
        };
        Object::install_method(thread, &scope, main_object, "new", thread_new_index, 1);
        Object::install_method(
            thread,
            &scope,
            main_object,
            "current",
            thread_current_index,
            0,
        );
        Object::install_method(thread, &scope, main_object, "sleep", thread_sleep_index, 1);
        Object::install_method(thread, &scope, main_object, "yield", thread_yield_index, 0);
        {
            let heap = thread.heap();
            vm.threads().set_main(main.as_tagged(heap));
        }
        let atomic_i64 = make_prototype(thread);
        Object::install_method(thread, &scope, atomic_i64, "new", atomic_i64_new_index, 1);
        let atomic_namespace = make_prototype(thread);
        let atomic_i64_name = thread.intern(&scope, "AtomicI64");
        let atomic_i64_name = scope.handle(atomic_i64_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            atomic_namespace,
            atomic_i64_name,
            PropertyDescriptor::data(atomic_i64.erase()),
        )
        .expect("install atomic.AtomicI64");
        let global = {
            let heap = thread.heap();
            heap.known().global_object.get_handle(heap, &scope)
        };
        let thread_value = main_object.erase();
        let atomic_value = atomic_namespace.erase();
        for (name, value) in [("Thread", thread_value), ("atomic", atomic_value)] {
            let name = thread.intern(&scope, name);
            let name = scope.handle(name.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
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
}

impl Default for ThreadRegistry {
    fn default() -> Self {
        Self {
            threads: Mutex::new(Vec::new()),
            next_name: AtomicUsize::new(0),
            main: AtomicOptionGcSlot::uninit(),
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

    fn main<'a>(&self, heap: &'a Heap) -> Tagged<'a, ThreadObject> {
        self.main.load(heap).expect("main thread not installed")
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

// Creates a not-yet-started thread for `target`.
fn thread_new<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let target = args.get_handle(heap, 0);
        let name = args.get_handle(heap, 1);
        let name = if name.as_tagged(heap).get_as::<DenseString>(heap).is_some() {
            name
        } else {
            let id = vm.threads().next_name();
            vm.interner()
                .intern_str(heap, &scope, &format!("thread-{id}"))
                .erase()
        };
        let object = heap.allocate_handle::<ThreadObject>(
            ThreadObjectInit {
                target,
                name,
                state: NEW,
                slot: -1,
            },
            &scope,
        );
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
        if object
            .as_tagged(heap)
            .state
            .compare_exchange_smi(Smi::new(NEW), Smi::new(RUNNABLE))
            .is_err()
        {
            raise!(vm, heap, state, VmError::ThreadAlreadyStarted);
        }
        let target = object.as_tagged(heap).target(heap);
        let slot = Arc::new(ThreadSlot::new(heap));
        // Register the roots before the child can run, so the GC sees them.
        let id = vm.threads().insert(Arc::clone(&slot));
        slot.object.store(object.as_tagged(heap).erase());
        slot.target.store(target);
        object.as_tagged(heap).set_slot(id as i64);
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
                object.as_tagged(heap).set_state(NEW);
                object.as_tagged(heap).set_slot(-1);
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
        if let Some(object) = slot.object.load(heap) {
            if let Some(object) = object.erase().get_as::<ThreadObject>(heap) {
                state.set_thread_object(object);
            }
        }
        let callable = {
            let heap = thread.heap();
            slot.target
                .load(heap)
                .and_then(|target| scope.cast::<Object>(heap, target))
        };
        if let Some(callable) = callable {
            if thread.execute(callable, &[], runtime).is_err() {
                let (_, heap, state) = thread.split();
                if let Some(exception) = state.take_pending_exception_tagged(heap) {
                    slot.exception.store(exception);
                }
            }
        }
        {
            let (_, heap, _) = thread.split();
            if let Some(object) = slot.object.load(heap) {
                if let Some(object) = object.erase().get_as::<ThreadObject>(heap) {
                    object.as_ref().set_state(TERMINATED);
                }
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
        let (id, started) = {
            let heap = &*heap;
            let object = object.as_tagged(heap);
            (object.slot(heap), object.state(heap) != NEW)
        };
        if id < 0 || !started {
            return vm.known().undefined.get(heap).erase();
        }
        let Some(slot) = vm.threads().get(id as usize) else {
            return vm.known().undefined.get(heap).erase();
        };
        loop {
            let done = {
                let heap = &*heap;
                object.as_tagged(heap).state(heap) == TERMINATED
            };
            if done || vm.is_shutdown() {
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
            let text = match Object::to_string(vm, heap, state, exception) {
                Ok(text) => Some(extend_lifetime!(heap, text)),
                Err(_) => None,
            };
            let message = match text {
                Some(text) => text
                    .get_as::<DenseString>(heap)
                    .map(|text| text.to_rust_string(heap))
                    .unwrap_or_default(),
                None => String::new(),
            };
            eprintln!("uncaught exception in thread: {message}");
            slot.exception
                .store_word(vm.known().the_hole.raw().to_bits());
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

fn thread_current<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if let Some(object) = state.thread_object(heap) {
        return extend_lifetime!(heap, object.erase());
    }
    let main = vm.threads().main(heap);
    state.set_thread_object(main);
    extend_lifetime!(heap, main.erase())
}

fn thread_sleep<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(millis) = args.get(heap, 0).to_number(heap) else {
        raise!(vm, heap, state, VmError::CannotConvertToNumber);
    };
    if !(millis >= 0.0) || !millis.is_finite() {
        raise!(vm, heap, state, VmError::InvalidTimeValue);
    }
    let total = Duration::from_millis(millis as u64);
    let start = Instant::now();
    loop {
        if vm.is_shutdown() {
            return shutdown_now(vm, heap, state);
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
    let HostCtx { vm, heap, .. } = ctx;
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
