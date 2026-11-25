use heap_tests::for_each_backend;
use vm::HeapBackend;

fn attach_reuses_native_state<B: HeapBackend>()
where
    B::Config: Default,
{
    let vm = heap_tests::vm::<B>(B::Config::default());
    let mut first = vm.attach();
    let second = vm.attach();

    assert!(std::ptr::eq(first.state(), second.state()));

    let (_, heap, state) = first.split();
    // The install-time main thread object is reused by later attaches.
    assert!(state.thread_object(heap).is_some());

    let vm2 = vm.clone();
    let has_object = std::thread::spawn(move || {
        let mut thread = vm2.attach();
        let (_, heap, state) = thread.split();
        state.thread_object(heap).is_some()
    })
    .join()
    .unwrap();
    assert!(!has_object);
}

for_each_backend!(attach_reuses_native_state);
