//! Generational-GC behaviour observed through the VM: minor cycles move
//! survivors, the write barrier retains old-to-young edges, weak cells
//! settle, and traced chains stay consistent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use heap_tests::vm;
use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{DenseString, FixedArray, Handle, HandleScope, Smi, Thread, VM};

fn test_vm() -> VM {
    vm::<MarkSweep>(MarkSweepConfig::default())
}

fn string<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    text: &str,
) -> Handle<'s, DenseString> {
    DenseString::from_utf8(thread.heap(), scope, text)
}

#[test]
fn minor_moves_rooted_survivors_and_follows_weak_refs() {
    let vm = test_vm();
    let mut thread = vm.attach();

    thread.handle_scope(|thread, scope| {
        let value = string(thread, &scope, "survivor");
        let before = value.as_tagged(thread.heap()).raw();
        assert!(vm.heap().is_young(before), "the survivor starts young");
        let weak = vm.track_weak(value.as_tagged(thread.heap()));

        thread.heap().collect_minor();

        let heap = thread.heap();
        let after = value.as_tagged(heap).raw();
        assert_ne!(after, before, "a minor cycle evacuates young survivors");
        assert!(after.is_strong_ptr());
        assert!(vm.heap().contains(after.raw_addr()));
        assert_eq!(
            vm.weak_value(weak).raw_addr(),
            after.raw_addr(),
            "the weak cell follows the evacuated object"
        );
        assert!(value.as_tagged(heap).data(heap).matches_ascii(b"survivor"));
    });
}

#[test]
fn write_barrier_keeps_old_to_young_target_alive() {
    let vm = test_vm();
    let mut thread = vm.attach();

    thread.handle_scope(|thread, scope| {
        let holder: Handle<FixedArray> = thread
            .heap()
            .allocate_handle::<FixedArray>(scope.stage(&[Smi::new(0).into_tagged()]), &scope);

        // promote the holder into the old generation
        thread.heap().collect_minor();
        assert!(!vm.heap().is_young(holder.as_tagged(thread.heap()).raw()));

        // the young target is reachable only through the old holder's slot
        let weak = thread.handle_scope(|thread, inner| {
            let young = string(thread, &inner, "young-target");
            let weak = vm.track_weak(young.as_tagged(thread.heap()));
            {
                let heap = thread.heap();
                holder
                    .as_tagged(heap)
                    .set(heap, 0, young.as_tagged(heap).erase());
            }
            weak
        });

        thread.heap().collect_minor();

        assert!(
            !vm.weak_value(weak).is_cleared(),
            "the write barrier must retain the young target"
        );
        let heap = thread.heap();
        let value = holder.as_tagged(heap).at(heap, 0);
        assert!(value.raw().is_strong_ptr());
        let young = value
            .get_as::<DenseString>(heap)
            .expect("the slot holds the young string");
        assert!(young.data(heap).matches_ascii(b"young-target"));
    });
}

#[test]
fn weak_young_refs_settle_after_minor() {
    let vm = test_vm();
    let mut thread = vm.attach();

    thread.handle_scope(|thread, scope| {
        let live = string(thread, &scope, "live");
        let weak_live = vm.track_weak(live.as_tagged(thread.heap()));
        let weak_dead = thread.handle_scope(|thread, inner| {
            let dead = string(thread, &inner, "dead");
            vm.track_weak(dead.as_tagged(thread.heap()))
        });

        thread.heap().collect_minor();

        assert!(!vm.weak_value(weak_live).is_cleared());
        assert!(vm.weak_value(weak_dead).is_cleared());
    });
}

#[test]
fn chains_relocate_consistently() {
    const LEN: usize = 256;
    let vm = test_vm();
    let mut thread = vm.attach();

    thread.handle_scope(|thread, scope| {
        let mut scope = scope;
        // Only the head stays rooted; the links keep the rest alive.
        let head = {
            let escapable = scope.escapable_scope();
            let mut next: Option<Handle<FixedArray>> = None;
            for _ in 0..LEN {
                let value = {
                    let heap = thread.heap();
                    match next {
                        Some(handle) => handle.as_tagged(heap).erase(),
                        None => Smi::new(0).into_tagged(),
                    }
                };
                let staged = escapable.stage(&[value]);
                let array = thread
                    .heap()
                    .allocate_handle::<FixedArray>(staged, &escapable);
                next = Some(array);
            }
            escapable.escape(next.unwrap())
        };

        thread.heap().collect_minor();

        let mut current = head;
        let mut count = 0;
        loop {
            count += 1;
            let heap = thread.heap();
            let value = current.as_tagged(heap).at(heap, 0);
            match value.get_as::<FixedArray>(heap) {
                Some(next) => current = scope.handle(next),
                None => {
                    assert_eq!(Smi::decode(value.raw()), Some(Smi::new(0)));
                    break;
                }
            }
        }
        assert_eq!(count, LEN);
    });
}

#[test]
fn full_gc_marks_traced_chains() {
    const LEN: usize = 128;
    let vm = test_vm();
    let mut thread = vm.attach();

    thread.handle_scope(|thread, scope| {
        let mut scope = scope;
        let mut weak = Vec::new();
        // Only the head is rooted; every other link is reachable solely
        // through the chain the collector must trace.
        let _head = {
            let escapable = scope.escapable_scope();
            let mut next: Option<Handle<FixedArray>> = None;
            for _ in 0..LEN {
                let value = {
                    let heap = thread.heap();
                    match next {
                        Some(handle) => handle.as_tagged(heap).erase(),
                        None => Smi::new(0).into_tagged(),
                    }
                };
                let staged = escapable.stage(&[value]);
                let array = thread
                    .heap()
                    .allocate_handle::<FixedArray>(staged, &escapable);
                weak.push(vm.track_weak(array.as_tagged(thread.heap())));
                next = Some(array);
            }
            escapable.escape(next.unwrap())
        };

        thread.heap().collect();

        for index in weak {
            assert!(
                !vm.weak_value(index).is_cleared(),
                "a link the marker failed to trace died"
            );
        }
    });
}

#[test]
fn allocation_continues_across_external_cycles() {
    let vm = test_vm();
    let running = Arc::new(AtomicBool::new(true));
    let allocations = Arc::new(AtomicUsize::new(0));

    let mut threads = Vec::new();
    for _ in 0..2 {
        let vm = vm.clone();
        let running = Arc::clone(&running);
        let allocations = Arc::clone(&allocations);
        threads.push(std::thread::spawn(move || {
            let mut thread = vm.attach();
            while running.load(Ordering::Relaxed) {
                thread.handle_scope(|thread, scope| {
                    for _ in 0..16 {
                        let _ = thread
                            .heap()
                            .allocate::<FixedArray>(scope.stage(&[Smi::new(0).into_tagged()]));
                    }
                });
                allocations.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for _ in 0..25 {
        vm.heap().collect();
        std::thread::sleep(Duration::from_millis(1));
    }
    running.store(false, Ordering::Relaxed);
    for thread in threads {
        thread.join().unwrap();
    }

    assert!(allocations.load(Ordering::Relaxed) > 0);
}
