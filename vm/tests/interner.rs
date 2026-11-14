use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    Compare, DefaultInterpreter, DenseString, Encoding, ExternalString, ExternalStringInit,
    StringRef, Thread, Utf8String, VM, flatten,
};

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

#[test]
fn strings_are_nul_terminated() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let latin = thread.intern(&scope, "hello");
        let wide = thread.intern(&scope, "€");

        {
            let heap = thread.heap();
            let latin = latin.as_tagged(heap);
            assert_eq!(latin.encoding(heap), Encoding::Latin1);
            let data = latin.terminated_data(heap);
            assert_eq!(data.len(), latin.len() + 1);
            assert_eq!(data.code_unit(latin.len()), 0);

            let wide = wide.as_tagged(heap);
            assert_eq!(wide.encoding(heap), Encoding::Utf16);
            let data = wide.terminated_data(heap);
            assert_eq!(data.len(), wide.len() + 1);
            assert_eq!(data.code_unit(wide.len()), 0);
        }

        let joined = {
            let heap = thread.heap();
            DenseString::concat(heap, &scope, latin.clone().erase(), wide.clone().erase())
        };
        {
            let heap = thread.heap();
            let joined = joined.as_tagged(heap);
            assert_eq!(joined.to_rust_string(heap), "hello€");
            let data = joined.terminated_data(heap);
            assert_eq!(data.len(), joined.len() + 1);
            assert_eq!(data.code_unit(joined.len()), 0);
        }
    });
}

#[test]
fn utf8_strings_expose_code_units() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let text = Utf8String::from_str(thread.heap(), &scope, "a€𐐷");
        {
            let heap = thread.heap();
            let string = text.as_tagged(heap);
            assert_eq!(string.unit_len(), 4);
            assert_eq!(string.byte_len(), "a€𐐷".len());
            assert_eq!(string.to_rust_string(heap), "a€𐐷");
            assert_eq!(string.terminated_bytes(heap).last(), Some(&0));

            let view = StringRef::try_from(heap, string.erase()).expect("string");
            assert_eq!(view.encoding(heap), Encoding::Utf8);
            assert_eq!(view.len(), 4);
            assert!(!view.is_flat());
            assert_eq!(view.to_units(heap), vec![0x61, 0x20ac, 0xd801, 0xdc37]);
            assert_eq!(view.code_unit(heap, 0), 0x61);
            assert_eq!(view.code_unit(heap, 2), 0xd801);
            assert_eq!(view.code_unit(heap, 3), 0xdc37);
            assert_eq!(view.utf8_bytes(heap), Some("a€𐐷".as_bytes()));
        }

        let handle = scope.handle(text.as_tagged(thread.heap()).erase());
        let flat = flatten(thread.heap(), &scope, handle).expect("flattens");
        {
            let heap = thread.heap();
            assert_eq!(flat.as_tagged(heap).to_rust_string(heap), "a€𐐷");
            assert_eq!(flat.as_tagged(heap).len(), 4);
        }
    });
}

#[test]
fn utf8_strings_compare_and_convert() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let dense = thread.intern(&scope, "value");
        let utf8 = Utf8String::from_str(thread.heap(), &scope, "value");
        let number = Utf8String::from_str(thread.heap(), &scope, "-12.5");
        {
            let heap = thread.heap();
            assert!(Compare::strict_equal(
                heap,
                dense.as_tagged(heap).erase(),
                utf8.as_tagged(heap).erase(),
            ));
            assert_eq!(number.as_tagged(heap).erase().to_number(heap), Some(-12.5));
            assert!(
                Compare::less_than(
                    heap,
                    number.as_tagged(heap).erase(),
                    dense.as_tagged(heap).erase()
                )
                .is_some()
            );
        }

        let utf8_handle = scope.handle(utf8.as_tagged(thread.heap()).erase());
        let joined = DenseString::concat(thread.heap(), &scope, dense.clone().erase(), utf8_handle);
        {
            let heap = thread.heap();
            assert_eq!(joined.as_tagged(heap).to_rust_string(heap), "valuevalue");
        }
    });
}

#[test]
fn external_strings_read_host_memory() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let latin1 = vec![b'h', b'i', 0];
        let utf16: Vec<u16> = "h€".encode_utf16().chain([0]).collect();
        let utf8 = b"h\xe2\x82\xac\0".to_vec();

        let latin = thread.heap().allocate_handle::<ExternalString>(
            ExternalStringInit {
                address: latin1.as_ptr() as usize,
                len: 2,
                unit_len: 2,
                encoding: Encoding::Latin1,
                owner: None,
            },
            &scope,
        );
        let wide = thread.heap().allocate_handle::<ExternalString>(
            ExternalStringInit {
                address: utf16.as_ptr() as usize,
                len: 2,
                unit_len: 2,
                encoding: Encoding::Utf16,
                owner: None,
            },
            &scope,
        );
        let raw = thread.heap().allocate_handle::<ExternalString>(
            ExternalStringInit {
                address: utf8.as_ptr() as usize,
                len: 4,
                unit_len: 2,
                encoding: Encoding::Utf8,
                owner: None,
            },
            &scope,
        );

        {
            let heap = thread.heap();
            assert_eq!(latin.as_tagged(heap).to_rust_string(heap), "hi");
            assert_eq!(wide.as_tagged(heap).to_rust_string(heap), "h€");
            assert_eq!(raw.as_tagged(heap).to_rust_string(heap), "h€");
            assert_eq!(raw.as_tagged(heap).code_unit(heap, 1), 0x20ac);

            let view = StringRef::try_from(heap, latin.as_tagged(heap).erase()).expect("string");
            assert_eq!(view.len(), 2);
            assert_eq!(view.encoding(heap), Encoding::Latin1);
            assert_eq!(view.utf8_bytes(heap), Some(&b"hi"[..]));

            let view = StringRef::try_from(heap, wide.as_tagged(heap).erase()).expect("string");
            assert_eq!(view.encoding(heap), Encoding::Utf16);
            assert_eq!(view.utf8_bytes(heap), None);
            assert_eq!(view.to_units(heap), vec![b'h' as u16, 0x20ac]);
        }
    });
}
