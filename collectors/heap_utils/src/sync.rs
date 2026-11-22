use std::{
    ptr,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Condvar, Mutex, MutexGuard},
};

/// Per-mutator safepoint state.
/// `parked` is the acknowledged collection epoch, not a counter.
pub struct LocalNode {
    parked: AtomicUsize,
    blocked: AtomicBool,
    cancelled: AtomicBool,
    linked: AtomicBool,
}

impl LocalNode {
    pub const fn detached() -> Self {
        Self {
            parked: AtomicUsize::new(0),
            blocked: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            linked: AtomicBool::new(false),
        }
    }

    pub fn blocked(&self) -> bool {
        self.blocked.load(Ordering::Acquire)
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn take_cancelled(&self) -> bool {
        self.cancelled.swap(false, Ordering::AcqRel)
    }
}

struct State {
    epoch: usize,
    running: bool,
    nodes: Vec<*const LocalNode>,
    work: Option<Arc<dyn Fn() + Send + Sync>>,
    work_gen: usize,
}

/// Stop-the-world rendezvous for mutators.
pub struct Safepoint {
    signal: AtomicUsize,
    state: Mutex<State>,
    cv: Condvar,
}

unsafe impl Send for Safepoint {}
unsafe impl Sync for Safepoint {}

impl Default for Safepoint {
    fn default() -> Self {
        Self::new()
    }
}

impl Safepoint {
    pub const fn new() -> Self {
        Self {
            signal: AtomicUsize::new(0),
            state: Mutex::new(State {
                epoch: 0,
                running: false,
                nodes: Vec::new(),
                work: None,
                work_gen: 0,
            }),
            cv: Condvar::new(),
        }
    }

    #[inline(always)]
    pub fn requested(&self) -> bool {
        self.signal.load(Ordering::Relaxed) != 0
    }

    pub fn is_armed(&self) -> bool {
        self.state.lock().unwrap().running
    }

    pub fn set_work(&self, work: Arc<dyn Fn() + Send + Sync>) {
        self.state.lock().unwrap().work = Some(work);
    }

    /// Wake parked mutators with new marking work.
    pub fn publish_work(&self) {
        let mut state = self.state.lock().unwrap();
        state.work_gen = state.work_gen.wrapping_add(1);
        self.cv.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Link a mutator; waits for a running collection to end.
    pub fn attach(&self, node: &LocalNode) {
        let mut state = self.lock();
        while state.running {
            state = self.cv.wait(state).unwrap();
        }
        debug_assert!(!node.linked.load(Ordering::Relaxed));
        state.nodes.push(ptr::from_ref(node));
        node.linked.store(true, Ordering::Relaxed);
    }

    /// Unlink a mutator; acknowledges a running collection first.
    pub fn detach(&self, node: &LocalNode) {
        loop {
            self.park_for_collection(node);
            let mut state = self.lock();
            if !state.running {
                debug_assert!(node.linked.load(Ordering::Relaxed));
                let at = state
                    .nodes
                    .iter()
                    .position(|&n| ptr::eq(n, node))
                    .expect("detaching a linked node");
                state.nodes.swap_remove(at);
                node.linked.store(false, Ordering::Relaxed);
                return;
            }
        }
    }

    /// Mark the mutator host-blocked; it must not touch the heap.
    pub fn block(&self, node: &LocalNode) -> bool {
        let _state = self.lock();
        node.blocked.store(true, Ordering::Release);
        self.cv.notify_all();
        node.cancelled()
    }

    /// Leave the blocked state and wait out a running collection.
    pub fn unblock(&self, node: &LocalNode) {
        {
            let _state = self.lock();
            node.blocked.store(false, Ordering::Release);
            self.cv.notify_all();
        }
        self.park_for_collection(node);
    }

    /// Park until the current collection ends.
    pub fn park_for_collection(&self, node: &LocalNode) -> bool {
        let mut state = self.lock();
        let mut serviced = 0usize;
        loop {
            if !state.running {
                return node.cancelled();
            }
            let epoch = state.epoch;
            if node.parked.load(Ordering::Relaxed) != epoch {
                node.parked.store(epoch, Ordering::Release);
                self.cv.notify_all();
            }
            if let Some(work) = state.work.clone() {
                if serviced != state.work_gen {
                    serviced = state.work_gen;
                    drop(state);
                    work();
                    state = self.lock();
                    continue;
                }
            }
            state = self.cv.wait(state).unwrap();
        }
    }

    /// Stop all mutators, run `gc`, then release them.
    pub fn stop_the_world(&self, requester: Option<&LocalNode>, gc: impl FnOnce()) {
        let collector = requester.map_or(ptr::null(), ptr::from_ref);
        let mut state = self.lock();
        if state.running {
            drop(state);
            match requester {
                Some(node) => {
                    self.park_for_collection(node);
                }
                None => {
                    let mut state = self.lock();
                    while state.running {
                        state = self.cv.wait(state).unwrap();
                    }
                }
            }
            return;
        }

        state.epoch = state.epoch.wrapping_add(1);
        state.running = true;
        let epoch = state.epoch;
        self.signal.store(epoch, Ordering::Release);
        self.cv.notify_all();

        loop {
            let pending = state.nodes.iter().any(|&node| {
                // Safety: linked nodes outlive the safepoint link.
                let node = unsafe { &*node };
                !ptr::eq(node, collector)
                    && !node.blocked()
                    && node.parked.load(Ordering::Acquire) != epoch
            });
            if pending {
                state = self.cv.wait(state).unwrap();
                continue;
            }
            break;
        }
        drop(state);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(gc));

        let mut state = self.lock();
        state.running = false;
        self.signal.store(0, Ordering::Release);
        self.cv.notify_all();
        drop(state);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Like [`Self::stop_the_world`], but guarantees `work` runs even when a
    /// collection is already in progress: wait for it, then stop the world.
    pub fn stop_the_world_exclusive(&self, requester: Option<&LocalNode>, work: impl FnOnce()) {
        let mut work = Some(work);
        loop {
            let collector = requester.map_or(ptr::null(), ptr::from_ref);
            let mut state = self.lock();
            if state.running {
                drop(state);
                match requester {
                    Some(node) => {
                        self.park_for_collection(node);
                    }
                    None => {
                        let mut state = self.lock();
                        while state.running {
                            state = self.cv.wait(state).unwrap();
                        }
                    }
                }
                continue;
            }

            state.epoch = state.epoch.wrapping_add(1);
            state.running = true;
            let epoch = state.epoch;
            self.signal.store(epoch, Ordering::Release);
            self.cv.notify_all();

            loop {
                let pending = state.nodes.iter().any(|&node| {
                    // Safety: linked nodes outlive the safepoint link.
                    let node = unsafe { &*node };
                    !ptr::eq(node, collector)
                        && !node.blocked()
                        && node.parked.load(Ordering::Acquire) != epoch
                });
                if pending {
                    state = self.cv.wait(state).unwrap();
                    continue;
                }
                break;
            }
            drop(state);

            let work = work.take().expect("exclusive work runs once");
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));

            let mut state = self.lock();
            state.running = false;
            self.signal.store(0, Ordering::Release);
            self.cv.notify_all();
            drop(state);
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
            return;
        }
    }

    /// Request cancellation; mutators observe it at their next poll.
    pub fn cancel_executions(&self, requester: &LocalNode, protocol: impl FnOnce()) {
        let state = self.lock();
        for &node in &state.nodes {
            if ptr::eq(node, requester) {
                continue;
            }
            // Safety: linked nodes outlive the safepoint link.
            unsafe { &*node }.cancelled.store(true, Ordering::Release);
        }
        self.cv.notify_all();
        drop(state);
        protocol();
    }
}
