use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{DefaultInterpreter, STRONG_PTR, Smi, Tagged, Thread, VM, Value, WEAK_PTR};

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .attach()
}

mod value {
    use super::*;

    #[test]
    fn bits_roundtrip() {
        for bits in [0, 1, 2, 3, u64::MAX] {
            assert_eq!(Value::from_bits(bits).to_bits(), bits);
        }
    }

    #[test]
    fn raw_addr_clears_tag_bits() {
        assert_eq!(Value::from_bits(0x1000).raw_addr(), 0x1000);
        assert_eq!(Value::from_bits(0x1001).raw_addr(), 0x1000);
        assert_eq!(Value::from_bits(0x1002).raw_addr(), 0x1000);
        assert_eq!(Value::from_bits(0x1003).raw_addr(), 0x1000);
    }

    #[test]
    fn classification_of_smi() {
        let v = Smi::new(42).encode();
        assert!(v.is_smi());
        assert!(!v.is_ptr());
        assert!(!v.is_strong_ptr());
        assert!(!v.is_weak_ptr());
    }
}

mod smi {
    use super::*;

    #[test]
    fn range_bounds_are_inclusive() {
        assert!(Smi::in_range(Smi::MAX));
        assert!(Smi::in_range(Smi::MIN));
        assert!(!Smi::in_range(Smi::MAX + 1));
        assert!(!Smi::in_range(Smi::MIN - 1));
    }

    #[test]
    fn min_is_negative_of_max_plus_one() {
        // Two's-complement range: exactly one more negative than positive.
        assert_eq!(Smi::MIN, -Smi::MAX - 1);
    }

    #[test]
    fn encode_decode_roundtrip() {
        for n in [0, 1, -1, 42, -42, Smi::MAX, Smi::MIN] {
            let smi = Smi::new(n);
            let decoded = Smi::decode(smi.encode()).unwrap();
            assert_eq!(decoded, smi);
            assert_eq!(decoded.value(), n);
        }
    }

    #[test]
    fn decode_rejects_pointers() {
        let (strong, weak) = (
            Value::from_bits(0x1000 | STRONG_PTR),
            Value::from_bits(0x1000 | WEAK_PTR),
        );
        assert_eq!(Smi::decode(strong), None);
        assert_eq!(Smi::decode(weak), None);
    }
}

mod debug {
    use super::*;

    #[test]
    fn value_formats_smi() {
        assert_eq!(format!("{:?}", Smi::new(-7).encode()), "Value(Smi(-7))");
        assert_eq!(format!("{:?}", Smi::new(0).encode()), "Value(Smi(0))");
    }

    #[test]
    fn value_formats_pointers() {
        let (strong, weak) = (
            Value::from_bits(0x1000 | STRONG_PTR),
            Value::from_bits(0x1000 | WEAK_PTR),
        );
        assert_eq!(format!("{:?}", strong), "Value(Strong(0x1000))");
        assert_eq!(format!("{:?}", weak), "Value(Weak(0x1000))");
    }

    #[test]
    fn smi_formats_inner_value() {
        assert_eq!(format!("{:?}", Smi::new(42)), "Smi(42)");
    }
}

mod tagged {
    use super::*;

    #[test]
    fn smi_constructor_checks_range() {
        assert!(Tagged::<Smi>::smi(Smi::MAX).is_some());
        assert!(Tagged::<Smi>::smi(Smi::MAX + 1).is_none());
    }

    #[test]
    fn from_smi_roundtrip() {
        let smi = Smi::new(-3);
        let tagged = Tagged::<Smi>::from_smi(smi);

        assert!(tagged.is_smi());
        assert!(!tagged.is_ptr());
        assert_eq!(tagged.to_smi(), Some(smi));
    }

    #[test]
    fn heap_object_classification() {
        let mut thread = test_vm();
        thread.handle_scope(|thread, scope| {
            let interned = thread.intern(&scope, "x");
            let heap = thread.heap();
            let tagged = interned.as_tagged(heap);

            assert!(tagged.is_ptr());
            assert!(tagged.is_strong_ptr());
            assert!(!tagged.is_weak_ptr());
            assert!(!tagged.is_smi());
            assert_eq!(tagged.erase().raw(), tagged.raw());
        });
    }

    #[test]
    fn maybe_weak_roundtrip() {
        let mut thread = test_vm();
        thread.handle_scope(|thread, scope| {
            let interned = thread.intern(&scope, "x");
            let heap = thread.heap();
            let strong = interned.as_tagged(heap);

            let weak = strong.as_weak();
            assert!(weak.raw().is_weak_ptr());
            assert!(!weak.raw().is_strong_ptr());
            assert_eq!(weak.raw().raw_addr(), strong.raw().raw_addr());
            assert_eq!(weak.as_strong().unwrap().raw(), strong.raw());

            let maybe = strong.as_maybe_weak();
            assert!(!maybe.is_cleared());
            assert_eq!(maybe.as_strong().unwrap().raw(), strong.raw());
            assert_eq!(maybe.raw(), strong.raw());
        });
    }

    #[test]
    fn erase_and_into_preserve_bits() {
        let tagged = Tagged::<Smi>::smi(7).unwrap();

        let erased: Value = tagged.raw();
        let re_tagged = Tagged::<Value>::try_smi(erased).unwrap();
        assert_eq!(re_tagged.raw().to_bits(), tagged.raw().to_bits());
        assert!(re_tagged.is_smi());
    }

    #[test]
    fn ptr_eq_compares_bits() {
        let a = Tagged::<Smi>::smi(1).unwrap();
        let b = Tagged::<Smi>::smi(1).unwrap();
        let c = Tagged::<Smi>::smi(2).unwrap();

        assert!(a.ptr_eq(b));
        assert!(!a.ptr_eq(c));
    }

    #[test]
    fn debug_formats_type_and_value() {
        let tagged = Tagged::<Smi>::smi(3).unwrap();
        let s = format!("{:?}", tagged);

        assert!(s.starts_with("Tagged("));
        assert!(s.contains("Smi"));
        assert!(s.contains("Value(Smi(3))"));
    }
}
