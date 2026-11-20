use crate::{
    AtomicOptionGcSlot, EdgeVisitable, Header, Heap, HeapObject, ObjectKind, Smi, Tagged,
    ThreadObject, ThreadStatus, Visitor, continuation::Continuation,
};
use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// Per-carrier roots.
pub struct Carrier {
    id: usize,
    current: AtomicOptionGcSlot<ThreadObject>,
    continuation: UnsafeCell<Continuation>,
}

unsafe impl Send for Carrier {}
unsafe impl Sync for Carrier {}

impl Default for Carrier {
    fn default() -> Self {
        Self {
            id: 0,
            current: AtomicOptionGcSlot::uninit(),
            continuation: UnsafeCell::new(Continuation::zeroed()),
        }
    }
}

impl Carrier {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            current: AtomicOptionGcSlot::uninit(),
            continuation: UnsafeCell::new(Continuation::zeroed()),
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn set_current(&self, thread: Tagged<'_, ThreadObject>) {
        self.current.store(thread);
    }

    pub fn clear_current(&self) {
        self.current.store_word(Smi::new(0).encode().to_bits());
    }

    pub fn current<'a>(&self, heap: &'a Heap) -> Option<Tagged<'a, ThreadObject>> {
        self.current.load(heap)
    }

    pub fn continuation_ptr(&self) -> *mut Continuation {
        self.continuation.get()
    }
}

struct SchedulerState {
    ready: VecDeque<AtomicOptionGcSlot<ThreadObject>>,
    timers: Vec<(Instant, AtomicOptionGcSlot<ThreadObject>)>,
}

/// One cooperative scheduler instance.
pub struct Scheduler {
    state: Mutex<SchedulerState>,
    carriers: Mutex<Vec<Arc<Carrier>>>,
    next_carrier: AtomicUsize,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            state: Mutex::new(SchedulerState {
                ready: VecDeque::new(),
                timers: Vec::new(),
            }),
            carriers: Mutex::new(Vec::new()),
            next_carrier: AtomicUsize::new(1),
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_carrier(&self) -> Arc<Carrier> {
        let id = self.next_carrier.fetch_add(1, Ordering::Relaxed);
        let carrier = Arc::new(Carrier::new(id));
        self.carriers.lock().unwrap().push(Arc::clone(&carrier));
        carrier
    }

    pub fn unregister_carrier(&self, carrier: &Arc<Carrier>) {
        self.carriers
            .lock()
            .unwrap()
            .retain(|registered| !Arc::ptr_eq(registered, carrier));
    }

    pub fn enqueue(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>) {
        if thread.task(heap).is_none() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        match thread.state(heap) {
            ThreadStatus::Ready | ThreadStatus::Terminated => return,
            ThreadStatus::New | ThreadStatus::Running | ThreadStatus::Blocked => {}
        }
        thread.set_state(ThreadStatus::Ready);
        // Safety: the slot starts as the hole and is visited as a root.
        let slot = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
        slot.store(thread);
        state.ready.push_back(slot);
    }

    pub fn yield_current(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>) {
        self.enqueue(heap, thread);
    }

    pub fn sleep_current(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>, duration: Duration) {
        if thread.task(heap).is_none() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if thread.state(heap) == ThreadStatus::Terminated {
            return;
        }
        thread.set_state(ThreadStatus::Blocked);
        // Safety: the slot starts as the hole and is visited as a root.
        let slot = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
        slot.store(thread);
        state.timers.push((Instant::now() + duration, slot));
    }

    /// Block `waiter` on `target`; false if the target already terminated.
    pub fn add_waiter_and_block(
        &self,
        heap: &Heap,
        target: Tagged<'_, ThreadObject>,
        waiter: Tagged<'_, ThreadObject>,
    ) -> bool {
        let (Some(target_context), Some(waiter_task)) = (target.task(heap), waiter.task(heap))
        else {
            return false;
        };
        let _state = self.state.lock().unwrap();
        if target.state(heap) == ThreadStatus::Terminated {
            return false;
        }
        target_context.add_waiter(heap, waiter);
        waiter_task.set_waiting_on(heap, target);
        waiter.set_state(ThreadStatus::Blocked);
        true
    }

    fn wake_timers(state: &mut SchedulerState, heap: &Heap) {
        let now = Instant::now();
        let mut i = 0;
        while i < state.timers.len() {
            if state.timers[i].0 <= now {
                let (_, slot) = state.timers.swap_remove(i);
                let Some(thread) = slot.load(heap) else {
                    continue;
                };
                if thread.state(heap) == ThreadStatus::Blocked {
                    thread.set_state(ThreadStatus::Ready);
                    state.ready.push_back(slot);
                }
            } else {
                i += 1;
            }
        }
    }

    pub fn next_timeout(&self, heap: &Heap) -> Option<Duration> {
        let mut state = self.state.lock().unwrap();
        Self::wake_timers(&mut state, heap);
        let next = state.timers.iter().map(|(at, _)| *at).min()?;
        Some(next.saturating_duration_since(Instant::now()))
    }

    pub fn next<'a>(&self, heap: &'a Heap, carrier: &Carrier) -> Option<Tagged<'a, ThreadObject>> {
        let mut state = self.state.lock().unwrap();
        Self::wake_timers(&mut state, heap);
        let pending = state.ready.len();
        for _ in 0..pending {
            let Some(slot) = state.ready.pop_front() else {
                break;
            };
            let Some(thread) = slot.load(heap) else {
                continue;
            };
            let Some(task) = thread.task(heap) else {
                continue;
            };
            let bound = task.carrier();
            if bound != 0 && bound != carrier.id() {
                state.ready.push_back(slot);
                continue;
            }
            if bound == 0 {
                task.set_carrier(carrier.id());
            }
            if thread.state(heap) != ThreadStatus::Ready {
                continue;
            }
            thread.set_state(ThreadStatus::Running);
            carrier.set_current(thread);
            return Some(thread);
        }
        None
    }

    pub fn terminate(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>) {
        let Some(context) = thread.task(heap) else {
            return;
        };
        let waiters = {
            let mut state = self.state.lock().unwrap();
            let raw = thread.raw().to_bits();
            state.ready.retain(|slot| slot.load_word(heap) != raw);
            state.timers.retain(|(_, slot)| slot.load_word(heap) != raw);
            context.take_waiter_slots()
        };
        for slot in waiters {
            let Some(waiter) = slot.load(heap) else {
                continue;
            };
            if waiter.state(heap) == ThreadStatus::Terminated {
                continue;
            }
            if let Some(waiter_task) = waiter.task(heap) {
                waiter_task.clear_waiting_on(heap);
            }
            let scheduler = waiter
                .task(heap)
                .and_then(|context| context.scheduler(heap))
                .map(|scheduler| scheduler.as_ref().scheduler());
            match scheduler {
                Some(scheduler) => scheduler.push_ready(heap, waiter),
                None => self.push_ready(heap, waiter),
            }
        }
    }

    /// Wake a blocked thread without making it runnable elsewhere.
    pub fn wake_blocked(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>) {
        let Some(task) = thread.task(heap) else {
            return;
        };
        let mut state = self.state.lock().unwrap();
        let raw = thread.raw().to_bits();
        state.timers.retain(|(_, slot)| slot.load_word(heap) != raw);
        if let Some(target) = task.waiting_on(heap) {
            if let Some(target_task) = target.task(heap) {
                target_task.remove_waiter(heap, thread);
            }
            task.clear_waiting_on(heap);
        }
        if thread.state(heap) != ThreadStatus::Blocked {
            return;
        }
        thread.set_state(ThreadStatus::Ready);
        // Safety: the slot starts as the hole and is visited as a root.
        let slot = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
        slot.store(thread);
        state.ready.push_back(slot);
    }

    /// Push an already-`Ready` thread onto this scheduler's run queue.
    pub fn push_ready(&self, heap: &Heap, thread: Tagged<'_, ThreadObject>) {
        if thread.task(heap).is_none() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if thread.state(heap) == ThreadStatus::Terminated {
            return;
        }
        thread.set_state(ThreadStatus::Ready);
        // Safety: the slot starts as the hole and is visited as a root.
        let slot = unsafe { AtomicOptionGcSlot::from_value(heap.known().the_hole.raw()) };
        slot.store(thread);
        state.ready.push_back(slot);
    }

    pub fn visit_edges(&self, visitor: &mut dyn Visitor) {
        let state = self.state.lock().unwrap();
        for slot in &state.ready {
            visitor.visit(slot.as_raw());
        }
        for (_, slot) in &state.timers {
            visitor.visit(slot.as_raw());
        }
        drop(state);
        for carrier in self.carriers.lock().unwrap().iter() {
            visitor.visit(carrier.current.as_raw());
        }
    }
}

/// GC handle for a [`Scheduler`].
#[repr(C)]
pub struct SchedulerObject {
    pub header: Header,
    native: MaybeUninit<Arc<Scheduler>>,
}

pub struct SchedulerObjectInit {
    pub scheduler: Arc<Scheduler>,
}

impl SchedulerObject {
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        // Safety: `init` writes the Arc before the object is published.
        unsafe { self.native.assume_init_ref() }
    }
}

impl HeapObject for SchedulerObject {
    const KIND: ObjectKind = ObjectKind::SchedulerObject;
    type Init<'a> = SchedulerObjectInit;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Layout::new::<Self>()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        self.header
            .map
            .init(heap.known().scheduler_object_map.get(heap));
        // Safety: uninitialized heap storage; write without dropping.
        unsafe {
            self.native
                .as_mut_ptr()
                .write(Arc::clone(&config.scheduler))
        };
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Layout::new::<Self>()
    }
}

impl EdgeVisitable for SchedulerObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        self.scheduler().visit_edges(visitor);
    }
}
