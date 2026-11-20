use heap_utils::{LocalNode, Safepoint};
use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

fn spin_until(mut f: impl FnMut() -> bool) {
    while !f() {
        std::thread::yield_now();
    }
}

#[test]
fn single_node_cycle_runs_gc_immediately() {
    let sync = Safepoint::new();
    let node = LocalNode::detached();
    sync.attach(&node);

    let ran = AtomicUsize::new(0);
    sync.stop_the_world(Some(&node), || {
        ran.fetch_add(1, Ordering::Relaxed);
    });

    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert!(!sync.is_armed());
    sync.detach(&node);
}

#[test]
fn mutators_park_before_gc_runs() {
    let sync = Safepoint::new();
    let attached = AtomicUsize::new(0);
    let ran = AtomicUsize::new(0);

    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| {
                let node = LocalNode::detached();
                sync.attach(&node);
                attached.fetch_add(1, Ordering::Relaxed);
                spin_until(|| sync.requested());
                sync.park_for_collection(&node);
            });
        }
        spin_until(|| attached.load(Ordering::Relaxed) == 3);
        s.spawn(|| {
            sync.stop_the_world(None, || {
                ran.fetch_add(1, Ordering::Relaxed);
            });
        });
    });

    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert!(!sync.is_armed());
}

#[test]
fn detach_acknowledges_a_running_cycle() {
    let sync = Safepoint::new();
    let node = LocalNode::detached();
    sync.attach(&node);

    let ran = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let leaver = s.spawn(|| {
            spin_until(|| sync.requested());
            sync.detach(&node);
        });
        sync.stop_the_world(None, || {
            ran.fetch_add(1, Ordering::Relaxed);
        });
        leaver.join().unwrap();
    });

    assert_eq!(ran.load(Ordering::Relaxed), 1);
    assert!(!sync.is_armed());
}

#[test]
fn attach_waits_for_a_running_cycle() {
    let sync = Safepoint::new();
    let first = LocalNode::detached();
    sync.attach(&first);
    let late = LocalNode::detached();
    let started = AtomicBool::new(false);
    let release = AtomicBool::new(false);
    let attached = AtomicBool::new(false);

    std::thread::scope(|s| {
        s.spawn(|| {
            sync.stop_the_world(None, || {
                started.store(true, Ordering::Release);
                spin_until(|| release.load(Ordering::Acquire));
            });
        });
        spin_until(|| sync.is_armed());

        s.spawn(|| {
            sync.attach(&late);
            attached.store(true, Ordering::Release);
            sync.detach(&late);
        });

        // Ack the cycle from another thread so the collector can start while
        // this thread coordinates the late attacher and the release gate.
        let parker = s.spawn(|| {
            sync.park_for_collection(&first);
        });

        std::thread::sleep(Duration::from_millis(10));
        assert!(!attached.load(Ordering::Acquire));

        spin_until(|| started.load(Ordering::Acquire));
        assert!(!attached.load(Ordering::Acquire));

        release.store(true, Ordering::Release);
        spin_until(|| attached.load(Ordering::Acquire));
        parker.join().unwrap();
    });

    sync.detach(&first);
}

#[test]
fn unblock_acknowledges_and_waits_for_the_cycle() {
    let sync = Safepoint::new();
    let node = LocalNode::detached();
    sync.attach(&node);
    assert!(!sync.block(&node));

    let started = AtomicBool::new(false);
    let release = AtomicBool::new(false);
    let unblocked = AtomicBool::new(false);

    std::thread::scope(|s| {
        s.spawn(|| {
            // A blocked node is excluded from the rendezvous.
            sync.stop_the_world(None, || {
                started.store(true, Ordering::Release);
                spin_until(|| release.load(Ordering::Acquire));
            });
        });
        spin_until(|| started.load(Ordering::Acquire));

        s.spawn(|| {
            sync.unblock(&node);
            unblocked.store(true, Ordering::Release);
        });
        std::thread::sleep(Duration::from_millis(10));
        assert!(!unblocked.load(Ordering::Acquire));

        release.store(true, Ordering::Release);
        spin_until(|| unblocked.load(Ordering::Acquire));
    });

    sync.detach(&node);
}

#[test]
fn second_requester_parks_instead_of_running_gc() {
    let sync = Safepoint::new();
    let a = LocalNode::detached();
    let b = LocalNode::detached();
    sync.attach(&a);
    sync.attach(&b);

    let first_runs = AtomicUsize::new(0);
    let second_runs = AtomicUsize::new(0);
    std::thread::scope(|s| {
        s.spawn(|| {
            sync.stop_the_world(None, || {
                first_runs.fetch_add(1, Ordering::Relaxed);
            });
        });
        spin_until(|| sync.requested());
        s.spawn(|| {
            sync.stop_the_world(Some(&b), || {
                second_runs.fetch_add(1, Ordering::Relaxed);
            });
        });
        sync.park_for_collection(&a);
    });

    assert_eq!(first_runs.load(Ordering::Relaxed), 1);
    assert_eq!(second_runs.load(Ordering::Relaxed), 0);
    sync.detach(&a);
    sync.detach(&b);
}

#[test]
fn panicking_gc_still_disarms() {
    let sync = Safepoint::new();
    let node = LocalNode::detached();
    sync.attach(&node);

    std::thread::scope(|s| {
        s.spawn(|| {
            spin_until(|| sync.requested());
            sync.park_for_collection(&node);
        });
        let panicked = s.spawn(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sync.stop_the_world(None, || panic!("gc exploded"));
            }))
            .unwrap_err();
        });
        assert!(panicked.join().is_ok());
    });

    assert!(!sync.is_armed());
    sync.detach(&node);
}

#[test]
fn churn_stress() {
    let sync = Safepoint::new();
    let cycles = AtomicUsize::new(0);

    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| {
                let node = LocalNode::detached();
                sync.attach(&node);
                for _ in 0..2000 {
                    sync.park_for_collection(&node);
                    std::thread::yield_now();
                }
                sync.detach(&node);
            });
        }
        s.spawn(|| {
            for _ in 0..100 {
                sync.stop_the_world(None, || {
                    cycles.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
    });

    assert_eq!(cycles.load(Ordering::Relaxed), 100);
}

#[test]
fn cancel_is_per_node_and_survives_cycles() {
    let sync = Safepoint::new();
    let requester = LocalNode::detached();
    let node = LocalNode::detached();
    sync.attach(&requester);

    let observed = AtomicUsize::new(0);
    let attached = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            sync.attach(&node);
            attached.store(true, Ordering::Release);
            while !node.take_cancelled() {
                sync.park_for_collection(&node);
                std::thread::yield_now();
            }
            observed.fetch_add(1, Ordering::Relaxed);
            sync.detach(&node);
        });

        spin_until(|| attached.load(Ordering::Acquire));
        let protocol = AtomicUsize::new(0);
        sync.cancel_executions(&requester, || {
            protocol.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(protocol.load(Ordering::Relaxed), 1);
        assert!(!sync.is_armed());
        // The requester is not cancelled.
        assert!(!requester.cancelled());

        // A normal cycle still runs while the node has not taken its cancel.
        sync.stop_the_world(Some(&requester), || {});
        spin_until(|| observed.load(Ordering::Relaxed) == 1);
    });

    sync.detach(&requester);
}

#[test]
fn attach_after_cancel_starts_fresh() {
    let sync = Safepoint::new();
    let requester = LocalNode::detached();
    sync.attach(&requester);
    sync.cancel_executions(&requester, || {});

    std::thread::scope(|s| {
        s.spawn(|| {
            let node = LocalNode::detached();
            sync.attach(&node);
            assert!(!sync.park_for_collection(&node));
            assert!(!node.cancelled());
            sync.detach(&node);
        });
    });

    sync.detach(&requester);
}
