use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::{
    DefaultInterpreter, Handle, HandleScope, HandleSlice, Heap, Map, Object, PropertyDescriptor,
    SlotName, Smi, Tagged, Thread, VM, Value,
};

fn test_vm() -> Thread {
    let vm = VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default()).unwrap();
    vm.arm_gc_stress();
    vm.attach()
}

fn intern_name<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    s: &str,
) -> Handle<'s, SlotName> {
    let interned = thread.intern(scope, s);
    let heap = thread.heap();
    scope.handle(interned.as_tagged(heap))
}

fn map_handle<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    obj: Handle<'_, Object>,
) -> Handle<'s, Map> {
    let heap = thread.heap();
    let map = obj.as_tagged(heap).map(heap);
    scope.handle(map)
}

fn add_prop(
    thread: &mut Thread,
    scope: &HandleScope<'_>,
    obj: Handle<'_, Object>,
    name: Handle<'_, SlotName>,
    value: i64,
) {
    let value = scope.handle(Smi::new(value));
    Object::add_own_property(
        thread.heap(),
        scope,
        obj,
        name,
        PropertyDescriptor::data(value),
    )
    .unwrap();
}

fn transition_target<'a>(
    heap: &'a Heap,
    map: Tagged<'a, Map>,
    name: Tagged<'a, SlotName>,
) -> Option<Tagged<'a, Value>> {
    let array = map.transitions.load(heap)?;
    for entry in array.as_slice().as_chunks::<2>().0 {
        if entry[0].get(heap).ptr_eq(name.erase()) {
            return entry[1].get_strong(heap);
        }
    }
    None
}

#[test]
fn append_records_pred_and_shares_transitions() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let name_b = intern_name(thread, &scope, "b");
        let root = thread.heap().known().object_initial_map.root_handle();

        let obj = thread
            .heap()
            .new_object(&scope, root, HandleSlice::EMPTY)
            .as_handle(&scope);
        add_prop(thread, &scope, obj, name_a, 1);
        let map_a = map_handle(thread, &scope, obj);
        add_prop(thread, &scope, obj, name_b, 2);
        let map_ab = map_handle(thread, &scope, obj);

        let other = thread
            .heap()
            .new_object(&scope, root, HandleSlice::EMPTY)
            .as_handle(&scope);
        add_prop(thread, &scope, other, name_a, 3);
        let map_a_again = map_handle(thread, &scope, other);

        let heap = thread.heap();
        assert!(root.as_tagged(heap).pred(heap).is_none());
        assert!(
            map_a
                .as_tagged(heap)
                .pred(heap)
                .is_some_and(|p| p.ptr_eq(root.as_tagged(heap)))
        );
        assert!(
            map_ab
                .as_tagged(heap)
                .pred(heap)
                .is_some_and(|p| p.ptr_eq(map_a.as_tagged(heap)))
        );

        let mut current = map_ab.as_tagged(heap);
        while let Some(pred) = current.pred(heap) {
            current = pred;
        }
        assert!(current.ptr_eq(root.as_tagged(heap)));

        assert!(map_a_again.as_tagged(heap).ptr_eq(map_a.as_tagged(heap)));
        assert!(
            map_a_again
                .as_tagged(heap)
                .pred(heap)
                .is_some_and(|p| p.ptr_eq(root.as_tagged(heap)))
        );
    });
}

#[test]
fn redefine_records_pred() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let root = thread.heap().known().object_initial_map.root_handle();
        let obj = thread
            .heap()
            .new_object(&scope, root, HandleSlice::EMPTY)
            .as_handle(&scope);

        add_prop(thread, &scope, obj, name_a, 1);
        let map_a = map_handle(thread, &scope, obj);

        let v = scope.handle(Smi::new(1));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            obj,
            name_a,
            PropertyDescriptor::Data {
                value: v,
                writable: false,
                enumerable: true,
                configurable: true,
            },
        )
        .unwrap();
        let map_a_readonly = map_handle(thread, &scope, obj);

        let heap = thread.heap();
        assert!(!map_a_readonly.as_tagged(heap).ptr_eq(map_a.as_tagged(heap)));
        assert!(
            map_a_readonly
                .as_tagged(heap)
                .pred(heap)
                .is_some_and(|p| p.ptr_eq(map_a.as_tagged(heap)))
        );
    });
}

#[test]
fn delete_records_pred() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let name_b = intern_name(thread, &scope, "b");
        let root = thread.heap().known().object_initial_map.root_handle();
        let obj = thread
            .heap()
            .new_object(&scope, root, HandleSlice::EMPTY)
            .as_handle(&scope);

        add_prop(thread, &scope, obj, name_a, 1);
        add_prop(thread, &scope, obj, name_b, 2);
        let map_ab = map_handle(thread, &scope, obj);

        let key = {
            let heap = thread.heap();
            scope.handle(name_a.as_tagged(heap).erase())
        };
        assert!(
            Object::delete_own_property(thread.heap(), &scope, obj, key).unwrap(),
            "the property is configurable and must delete"
        );
        let map_b = map_handle(thread, &scope, obj);

        let heap = thread.heap();
        assert!(!map_b.as_tagged(heap).ptr_eq(map_ab.as_tagged(heap)));
        assert!(
            map_b
                .as_tagged(heap)
                .pred(heap)
                .is_some_and(|p| p.ptr_eq(map_ab.as_tagged(heap)))
        );
    });
}

#[test]
fn live_transition_subtree_survives_gc() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let name_b = intern_name(thread, &scope, "b");
        let root_handle = thread.heap().known().object_initial_map.root_handle();

        let obj = thread
            .heap()
            .new_object(&scope, root_handle, HandleSlice::EMPTY)
            .as_handle(&scope);
        add_prop(thread, &scope, obj, name_a, 1);
        add_prop(thread, &scope, obj, name_b, 2);

        thread.heap().collect();

        let map_ab = map_handle(thread, &scope, obj);
        let heap = thread.heap();
        let root = root_handle.as_tagged(heap);
        assert!(
            transition_target(heap, root, name_a.as_tagged(heap)).is_some(),
            "root -> {{a}} must survive while an object uses {{a, b}}"
        );
        let mut current = map_ab.as_tagged(heap);
        while let Some(pred) = current.pred(heap) {
            current = pred;
        }
        assert!(
            current.ptr_eq(root),
            "the pred chain must still reach the root"
        );
        let map_a = map_ab
            .as_tagged(heap)
            .pred(heap)
            .expect("intermediate map alive");
        assert!(
            transition_target(heap, map_a, name_b.as_tagged(heap)).is_some(),
            "{{a}} -> {{a, b}} must survive"
        );
    });
}

#[test]
fn dead_transition_targets_are_cleared_by_gc() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let name_b = intern_name(thread, &scope, "b");
        let root_handle = thread.heap().known().object_initial_map.root_handle();

        let live = thread
            .heap()
            .new_object(&scope, root_handle, HandleSlice::EMPTY)
            .as_handle(&scope);
        add_prop(thread, &scope, live, name_a, 1);

        thread.handle_scope(|thread, inner| {
            let dead = thread
                .heap()
                .new_object(&inner, root_handle, HandleSlice::EMPTY)
                .as_handle(&inner);
            add_prop(thread, &inner, dead, name_b, 2);
        });

        let heap = thread.heap();
        let root = root_handle.as_tagged(heap);
        assert!(transition_target(heap, root, name_a.as_tagged(heap)).is_some());
        assert!(transition_target(heap, root, name_b.as_tagged(heap)).is_some());

        thread.heap().collect();

        let heap = thread.heap();
        let root = root_handle.as_tagged(heap);
        assert!(
            transition_target(heap, root, name_a.as_tagged(heap)).is_some(),
            "the live {{a}} branch must survive"
        );
        assert!(
            transition_target(heap, root, name_b.as_tagged(heap)).is_none(),
            "the dead {{b}} branch's weak entry must be cleared"
        );
    });
}

#[test]
fn unused_whole_subtree_is_collected() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let name_a = intern_name(thread, &scope, "a");
        let name_b = intern_name(thread, &scope, "b");
        let root_handle = thread.heap().known().object_initial_map.root_handle();

        thread.handle_scope(|thread, inner| {
            let obj = thread
                .heap()
                .new_object(&inner, root_handle, HandleSlice::EMPTY)
                .as_handle(&inner);
            add_prop(thread, &inner, obj, name_a, 1);
            add_prop(thread, &inner, obj, name_b, 2);
        });

        thread.heap().collect();

        let heap = thread.heap();
        let root = root_handle.as_tagged(heap);
        assert!(
            transition_target(heap, root, name_a.as_tagged(heap)).is_none(),
            "an unreferenced subtree must not be retained"
        );
    });
}
