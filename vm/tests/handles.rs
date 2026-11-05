use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    DefaultInterpreter, EdgeVisitable, FixedArray, Handle, Heap, RawCell, Smi, Tagged, Thread,
    ThreadState, VM, Visitor,
};

struct Counter(usize);

impl Visitor for Counter {
    fn visit(&mut self, _cell: &RawCell) {
        self.0 += 1;
    }
}

fn root_count(state: &ThreadState) -> usize {
    let mut counter = Counter(0);
    state.visit_edges(&mut counter);
    counter.0
}

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .attach()
}

fn smi_bits(heap: &Heap, handle: Handle<'_, Smi>) -> i64 {
    Smi::decode(handle.as_tagged(heap).raw()).unwrap().value()
}

#[test]
fn handles_read_back_their_values() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let base = root_count(thread.state());
        let a = scope.handle(Tagged::smi(42).unwrap());
        let b = scope.handle(Tagged::smi(-7).unwrap());
        let heap = thread.heap();
        assert_eq!(smi_bits(heap, a), 42);
        assert_eq!(smi_bits(heap, b), -7);
        assert_eq!(root_count(thread.state()), base + 2);
    });
}

#[test]
fn closed_scope_unroots_its_handles() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let keep = scope.handle(Tagged::smi(1).unwrap());
        let outer = root_count(thread.state());

        thread.handle_scope(|thread, inner| {
            inner.handle(Tagged::smi(2).unwrap());
            inner.handle(Tagged::smi(3).unwrap());
            assert_eq!(root_count(thread.state()), outer + 2);
        });

        // inner scope closed: its slots are reclaimed, outer handle survives
        assert_eq!(root_count(thread.state()), outer);
        assert_eq!(smi_bits(thread.heap(), keep), 1);
    });
}

#[test]
fn reclaimed_slots_are_reused() {
    let mut thread = test_vm();
    let base = thread.handle_scope(|thread, scope| {
        let base = root_count(thread.state());
        for i in 0..10 {
            scope.handle(Tagged::smi(i).unwrap());
        }
        assert_eq!(root_count(thread.state()), base + 10);
        base
    });
    assert_eq!(root_count(thread.state()), base);

    thread.handle_scope(|thread, scope| {
        for i in 0..5 {
            scope.handle(Tagged::smi(i * 100).unwrap());
        }
        assert_eq!(root_count(thread.state()), base + 5);
        let fourth = scope.handle(Tagged::smi(400).unwrap());
        assert_eq!(smi_bits(thread.heap(), fourth), 400);
    });
}

#[test]
fn blocks_extend_when_full() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let base = root_count(thread.state());
        let mut handles = Vec::new();
        for i in 0..1030 {
            handles.push(scope.handle(Tagged::smi(i).unwrap()));
        }

        assert_eq!(root_count(thread.state()), base + 1030);
        let heap = thread.heap();
        for (i, handle) in handles.iter().enumerate() {
            assert_eq!(smi_bits(heap, *handle), i as i64);
        }
    });
}

#[test]
fn escaped_handle_survives_inner_scope() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let mut scope = scope;
        let base = root_count(thread.state());
        let keep = scope.handle(Tagged::smi(1).unwrap());
        assert_eq!(smi_bits(thread.heap(), keep), 1);

        let escaped = {
            let escapable = scope.escapable_scope();
            let h = escapable.handle(Tagged::smi(2).unwrap());
            escapable.handle(Tagged::smi(3).unwrap());
            escapable.escape(h)
        };

        assert_eq!(root_count(thread.state()), base + 2);
        assert_eq!(smi_bits(thread.heap(), escaped), 2);
    });
}

#[test]
fn escapable_scope_closed_without_escape_reclaims() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let mut scope = scope;
        let base = root_count(thread.state());
        let keep = scope.handle(Tagged::smi(1).unwrap());
        assert_eq!(smi_bits(thread.heap(), keep), 1);

        {
            let escapable = scope.escapable_scope();
            escapable.handle(Tagged::smi(2).unwrap());
        }

        // only the escape slot remains in the parent scope
        assert_eq!(root_count(thread.state()), base + 2);
    });
}

#[test]
fn handles_survive_collection() {
    let mut thread = test_vm();
    thread.handle_scope(|thread, scope| {
        let values = scope.stage(&[Tagged::smi(42).unwrap()]);
        let array = thread.heap().allocate_handle::<FixedArray>(values, &scope);
        thread.heap().collect();

        let heap = thread.heap();
        let tagged = array.as_tagged(heap);
        assert_eq!(Smi::decode(tagged.at(heap, 0).raw()).unwrap().value(), 42);
    });
}
