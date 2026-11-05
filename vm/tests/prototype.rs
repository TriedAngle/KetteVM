use mark_sweep::{MarkSweep, MarkSweepConfig};
use vm::ic::{InlineCache, LoadHit};
use vm::{
    DefaultInterpreter, FeedbackVector, FeedbackVectorInit, FixedArray, Handle, HandleScope,
    HandleSlice, Object, PropertyDescriptor, SlotName, Smi, Thread, VM, Value,
};

fn test_vm() -> Thread {
    VM::new::<MarkSweep, DefaultInterpreter>(MarkSweepConfig::default())
        .unwrap()
        .attach()
}

fn slot_name<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    name: &str,
) -> Handle<'s, SlotName> {
    let interned = thread.intern(scope, name);
    let named = {
        let heap = thread.heap();
        interned.as_tagged(heap).erase().as_name()
    };
    scope.handle(named)
}

fn object_with<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    proto: Handle<'_, Value>,
    props: &[(&str, i64)],
) -> Handle<'s, Object> {
    let map = thread.heap().known().object_initial_map.root_handle();
    let obj = thread
        .heap()
        .new_object(scope, map, HandleSlice::EMPTY)
        .as_handle(scope);
    Object::set_prototype(thread.heap(), scope, obj, proto).unwrap();
    for (name, value) in props {
        let name = slot_name(thread, scope, name);
        let value = scope.handle(Smi::new(*value));
        Object::add_own_property(
            thread.heap(),
            scope,
            obj,
            name,
            PropertyDescriptor::data(value),
        )
        .unwrap();
    }
    obj
}

fn parents_of<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    p1: Handle<'_, Object>,
    p2: Handle<'_, Object>,
) -> Handle<'s, FixedArray> {
    let first = thread.intern(scope, "parent");
    let second = thread.intern(scope, "mixin");
    let values = scope.stage_handles(&[first.erase(), p1.erase(), second.erase(), p2.erase()]);
    thread.heap().allocate_handle::<FixedArray>(values, scope)
}

/// A parent-name slot (the pair list exposes each parent under its own
/// name, read-only) is cached as `KIND_PARENT` and reads the parent object
/// out of the map's pair list.
#[test]
fn multi_parent_name_load_returns_the_parent() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let null = thread.heap().known().null.root_handle().erase();
        let p1 = object_with(thread, &scope, null, &[]);
        let p2 = object_with(thread, &scope, null, &[]);
        let protos = parents_of(thread, &scope, p1, p2).erase();
        let home = object_with(thread, &scope, protos, &[]);
        let name = slot_name(thread, &scope, "parent");

        let vector = thread
            .heap()
            .allocate_handle::<FeedbackVector>(FeedbackVectorInit { length: 2 }, &scope);
        InlineCache::update_load(
            thread.heap(),
            &scope,
            Some(vector),
            0,
            Some(home),
            name,
            false,
        );

        let heap = thread.heap();
        let hit = InlineCache::try_load(
            heap,
            Some(vector.as_tagged(heap)),
            0,
            home.as_tagged(heap).erase(),
        );
        assert!(
            matches!(hit, Some(LoadHit::Value(v)) if v == p1.as_tagged(heap).erase()),
            "the parent-name slot must return the first parent object"
        );
    });
}

/// A cached miss through a Self-style parent list must be invalidated when
/// a parent gains the property.
#[test]
fn multi_parent_cached_miss_invalidates_when_parent_gains_property() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let null = thread.heap().known().null.root_handle().erase();
        let p1 = object_with(thread, &scope, null, &[]);
        let p2 = object_with(thread, &scope, null, &[]);
        let protos = parents_of(thread, &scope, p1, p2).erase();
        let home = object_with(thread, &scope, protos, &[]);
        let y = slot_name(thread, &scope, "y");

        let vector = thread
            .heap()
            .allocate_handle::<FeedbackVector>(FeedbackVectorInit { length: 2 }, &scope);
        InlineCache::update_load(thread.heap(), &scope, Some(vector), 0, Some(home), y, true);
        {
            let heap = thread.heap();
            let hit = InlineCache::try_load(
                heap,
                Some(vector.as_tagged(heap)),
                0,
                home.as_tagged(heap).erase(),
            );
            assert!(matches!(hit, Some(LoadHit::NotFound)), "the miss is cached");
        }

        let value = scope.handle(Smi::new(9));
        Object::add_own_property(
            thread.heap(),
            &scope,
            p2,
            y,
            PropertyDescriptor::data(value),
        )
        .unwrap();

        let heap = thread.heap();
        let hit = InlineCache::try_load(
            heap,
            Some(vector.as_tagged(heap)),
            0,
            home.as_tagged(heap).erase(),
        );
        assert!(hit.is_none(), "the cached miss must be invalidated");
    });
}

/// A load cached through a Self-style parent list must be invalidated when
/// *any* parent in the closure changes shape, even the higher-priority one
/// that did not hold the property.
#[test]
fn multi_parent_ic_invalidates_on_parent_shape_change() {
    let mut thread = test_vm();

    thread.handle_scope(|thread, scope| {
        let null = thread.heap().known().null.root_handle().erase();
        let p1 = object_with(thread, &scope, null, &[]);
        let p2 = object_with(thread, &scope, null, &[("x", 42)]);
        let protos = parents_of(thread, &scope, p1, p2).erase();
        let home = object_with(thread, &scope, protos, &[]);
        let x = slot_name(thread, &scope, "x");

        let vector = thread
            .heap()
            .allocate_handle::<FeedbackVector>(FeedbackVectorInit { length: 2 }, &scope);

        // Record a load that resolves through the second parent.
        InlineCache::update_load(thread.heap(), &scope, Some(vector), 0, Some(home), x, false);
        {
            let heap = thread.heap();
            let hit = InlineCache::try_load(
                heap,
                Some(vector.as_tagged(heap)),
                0,
                home.as_tagged(heap).erase(),
            );
            assert!(
                matches!(hit, Some(LoadHit::Value(v)) if v == Smi::new(42).encode()),
                "cached load must resolve through the second parent"
            );
        }

        // Give the higher-priority parent the same property: the closure
        // changed even though the receiver's own map did not.
        let value = scope.handle(Smi::new(7));
        Object::add_own_property(
            thread.heap(),
            &scope,
            p1,
            x,
            PropertyDescriptor::data(value),
        )
        .unwrap();

        {
            let heap = thread.heap();
            let hit = InlineCache::try_load(
                heap,
                Some(vector.as_tagged(heap)),
                0,
                home.as_tagged(heap).erase(),
            );
            assert!(
                hit.is_none(),
                "a parent shape change must invalidate the cached handler"
            );
        }

        // Re-record and observe the shadowing value.
        InlineCache::update_load(thread.heap(), &scope, Some(vector), 0, Some(home), x, false);
        let heap = thread.heap();
        let hit = InlineCache::try_load(
            heap,
            Some(vector.as_tagged(heap)),
            0,
            home.as_tagged(heap).erase(),
        );
        assert!(
            matches!(hit, Some(LoadHit::Value(v)) if v == Smi::new(7).encode()),
            "the re-recorded handler must see the shadowing value"
        );
    });
}
