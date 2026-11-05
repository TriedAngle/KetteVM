use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{DefaultInterpreter, DenseString, Encoding, Thread, VM};

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .attach()
}

#[test]
fn interning_deduplicates_and_preserves_content() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let a = thread.intern(&scope, "hello");
        let text = String::from("hello");
        let b = thread.intern(&scope, &text);
        let c = thread.intern(&scope, "world");

        {
            let heap = thread.heap();
            assert_eq!(a.as_tagged(heap), b.as_tagged(heap));
            assert_ne!(a.as_tagged(heap), c.as_tagged(heap));
        }

        // content round trip (compressed encoding: Latin1)
        let (text, hash_a, hash_b) = {
            let heap = thread.heap();
            (
                a.as_tagged(heap).to_rust_string(heap),
                a.as_tagged(heap).hash(heap),
                b.as_tagged(heap).hash(heap),
            )
        };
        assert_eq!(text, "hello");
        assert_eq!(hash_a, hash_b);

        // re-interning an interned string forwards to the same entry
        let d = thread.intern(&scope, &text);
        let heap = thread.heap();
        assert_eq!(a.as_tagged(heap), d.as_tagged(heap));
    });
}

#[test]
fn interning_compresses_utf16_to_latin1() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        // "héllo" fits Latin1; "€" (0x20AC) does not
        let latin1_content = thread.intern(&scope, "héllo");
        let utf16_content = thread.intern(&scope, "€");

        // re-interning the same content through a heap string (the
        // keyed-lookup path) finds the canonical instance
        let again = DenseString::from_utf8(thread.heap(), &scope, "héllo");
        let canonical = {
            let (vm, heap, _) = thread.split();
            vm.interner().intern_value(heap, &scope, &again)
        };
        {
            let heap = thread.heap();
            assert_eq!(latin1_content.as_tagged(heap), canonical.as_tagged(heap));
        }

        let heap = thread.heap();
        let l = latin1_content.as_tagged(heap);
        let u = utf16_content.as_tagged(heap);
        // encodings come from the map's kind bits
        assert_eq!(l.encoding(heap), Encoding::Latin1);
        assert_eq!(u.encoding(heap), Encoding::Utf16);
        assert_eq!(l.len(), 5);
        assert_eq!(u.len(), 1);
        // code-unit access is O(1) in both encodings
        assert_eq!(l.code_unit(heap, 1), 0xE9);
        assert_eq!(u.code_unit(heap, 0), 0x20AC);
    });
}

#[test]
fn interning_is_thread_safe() {
    let mut thread = test_vm();

    let expected = thread.handle_scope(|thread, scope| {
        let shared = thread.intern(&scope, "shared");
        let heap = thread.heap();
        shared.as_tagged(heap).raw().to_bits()
    });

    let vm = thread.vm().clone();
    let mut threads = Vec::new();
    for _ in 0..4 {
        let vm = vm.clone();
        threads.push(std::thread::spawn(move || {
            let mut thread = vm.attach();
            thread.handle_scope(|thread, scope| {
                let shared = thread.intern(&scope, "shared");
                let heap = thread.heap();
                shared.as_tagged(heap).raw().to_bits()
            })
        }));
    }
    for thread in threads {
        assert_eq!(thread.join().unwrap(), expected);
    }
}
