use crate::{
    Cell, CellInit, FixedArray, Handle, HandleScope, Heap, Map, MapKind, Object, PrototypeInfo,
    Smi, Tagged, Value, WeakFixedArray,
};
use std::{sync::Mutex, sync::MutexGuard};

pub struct PrototypeRegistry {
    lock: Mutex<()>,
}

impl PrototypeRegistry {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for PrototypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Prototype;

/// Registry work requested by a shape change.
#[derive(Clone, Copy, Default)]
pub struct ShapeChange {
    /// Also propagate the callable-parent hint to dependent parent-list maps.
    pub propagate_callable_parent: bool,
}

impl ShapeChange {
    /// Invalidate cached handlers only.
    pub const INVALIDATE_ONLY: Self = Self {
        propagate_callable_parent: false,
    };
}

impl Prototype {
    pub fn try_get_validity_cell_holder_map<'a>(
        heap: &'a Heap,
        map: Tagged<'a, Map>,
    ) -> Option<Tagged<'a, Map>> {
        if map.is_prototype() {
            return Some(map);
        }
        let proto = map.prototype.get(heap);
        let object = trackable_prototype(heap, proto)?;
        Some(object.map(heap))
    }

    pub fn get_or_create_prototype_chain_validity_cell<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        map: Handle<'s, Map>,
    ) -> Option<Handle<'s, Cell>> {
        // Multi-parent maps own one cell covering the full parent closure.
        if map.as_tagged(heap).is_multi_parent() {
            return Self::get_or_create_multi_parent_cell(heap, scope, map);
        }
        let holder = Self::try_get_validity_cell_holder_map(heap, map.as_tagged(heap))?;
        let holder = scope.handle(holder);
        holder.as_tagged(heap).mark_prototype(heap);
        // Multi-parent holders keep one closure-wide cell.
        if holder.as_tagged(heap).is_multi_parent() {
            return Self::get_or_create_multi_parent_cell(heap, scope, holder);
        }
        register_prototype_user_chain(heap, scope, holder);

        if let Some(cell) = holder.as_tagged(heap).published_validity_cell(heap)
            && cell.is_valid(heap)
        {
            return Some(cell.as_handle(scope));
        }

        let cell = heap.allocate_handle::<Cell>(
            CellInit {
                value: Smi::new(1).into_tagged().as_maybe_weak(),
            },
            scope,
        );
        holder
            .as_tagged(heap)
            .set_validity_cell(heap, cell.as_tagged(heap));
        Some(cell)
    }

    /// Validity cell for a multi-parent map, registered across its full
    /// parent closure.
    fn get_or_create_multi_parent_cell<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        map: Handle<'s, Map>,
    ) -> Option<Handle<'s, Cell>> {
        // Cells are only published after the full closure is registered.
        if let Some(cell) = map.as_tagged(heap).published_validity_cell(heap)
            && cell.is_valid(heap)
        {
            return Some(cell.as_handle(scope));
        }
        if !Self::register_multi_parent_closure(heap, scope, map) {
            return None;
        }

        let cell = heap.allocate_handle::<Cell>(
            CellInit {
                value: Smi::new(1).into_tagged().as_maybe_weak(),
            },
            scope,
        );
        map.as_tagged(heap)
            .set_validity_cell(heap, cell.as_tagged(heap));
        Some(cell)
    }

    /// Register `map` with its full parent closure; `false` if a parent
    /// is untrackable.
    pub fn register_multi_parent_closure<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        map: Handle<'s, Map>,
    ) -> bool {
        let mut parents: Vec<Handle<'s, Map>> = Vec::new();
        if !collect_parent_maps(heap, scope, map.as_tagged(heap), &mut parents) {
            return false;
        }
        let mut visited: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < parents.len() {
            let parent = parents[i];
            i += 1;
            let id = parent.as_tagged(heap).raw_addr() as usize;
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            parent.as_tagged(heap).mark_prototype(heap);
            let info = Self::get_or_create_prototype_info(heap, scope, parent);
            append_prototype_child(heap, scope, info, map, PrototypeInfo::UNREGISTERED);
            if !collect_parent_maps(heap, scope, parent.as_tagged(heap), &mut parents) {
                return false;
            }
        }
        true
    }

    pub fn ensure_store_transition_validity_cell<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        map: Handle<'s, Map>,
    ) {
        if map.as_tagged(heap).is_prototype_validity_cell_valid(heap) {
            return;
        }
        match Self::get_or_create_prototype_chain_validity_cell(heap, scope, map) {
            Some(cell) => map
                .as_tagged(heap)
                .set_validity_cell(heap, cell.as_tagged(heap)),
            None => map.as_tagged(heap).set_validity_cell_sentinel(heap),
        }
    }

    pub fn get_or_create_prototype_info<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        map: Handle<'s, Map>,
    ) -> Handle<'s, PrototypeInfo> {
        if let Some(info) = map.as_tagged(heap).try_get_prototype_info(heap) {
            return info.as_handle(scope);
        }
        loop {
            let expected = map.as_tagged(heap).prototype_info.load_word(heap);
            if let Some(winner) = map.as_tagged(heap).prototype_info.load(heap) {
                return winner.as_handle(scope);
            }
            let info = heap.allocate_handle::<PrototypeInfo>((), scope);
            let host = map.as_tagged(heap).erase();
            match map.as_tagged(heap).prototype_info.publish(
                heap,
                host,
                expected,
                info.as_tagged(heap),
            ) {
                Ok(()) => return info,
                Err(_) => continue,
            }
        }
    }

    pub fn shape_changed(heap: &Heap, old_map: Tagged<'_, Map>, change: ShapeChange) {
        if !old_map.is_prototype() {
            return;
        }
        invalidate_prototype_chains(heap, old_map);
        if change.propagate_callable_parent {
            propagate_callable_parent(heap, old_map);
        }
    }

    #[inline]
    pub fn element_mutated(heap: &Heap, obj: Tagged<'_, Object>) {
        let map = obj.map(heap);
        if !map.kind().is_prototype() {
            return;
        }
        invalidate_prototype_chains(heap, map);
    }

    /// `element_mutated` for callers that already hold the receiver's
    /// decoded map kind: skips the map/kind reload on the hot store path.
    #[inline]
    pub fn element_mutated_kind(heap: &Heap, obj: Tagged<'_, Object>, kind: MapKind) {
        if !kind.is_prototype() {
            return;
        }
        invalidate_prototype_chains(heap, obj.map(heap));
    }
}

fn trackable_prototype<'a>(heap: &'a Heap, value: Tagged<'a, Value>) -> Option<Tagged<'a, Object>> {
    if !value.is_strong_ptr() {
        return None;
    }
    if value.ptr_eq(heap.known().null.get(heap).erase())
        || value.ptr_eq(heap.known().undefined.get(heap).erase())
        || value.ptr_eq(heap.known().the_hole.get(heap).erase())
    {
        return None;
    }
    let object = value.get_as::<Object>(heap)?;
    if !object.map(heap).kind().kind().is_js_receiver() {
        return None;
    }
    Some(object)
}

/// Collect the maps of `map`'s direct parents: a single prototype object or
/// every parent in a Self-style parent list. Returns `false` (and leaves
/// `out` partially filled) when a parent cannot be tracked, which makes the
/// whole closure uncacheable.
fn collect_parent_maps<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    map: Tagged<'_, Map>,
    out: &mut Vec<Handle<'s, Map>>,
) -> bool {
    let proto = map.prototype.get(heap);
    if map.is_multi_parent() {
        let Some(pairs) = proto.get_as::<FixedArray>(heap) else {
            return false;
        };
        let mut i = 1;
        while i < pairs.len() {
            if !push_parent_map(heap, scope, pairs.at(heap, i), out) {
                return false;
            }
            i += 2;
        }
        return true;
    }
    if !proto.is_strong_ptr() {
        return true;
    }
    let known = heap.known();
    if proto.ptr_eq(known.null.get(heap).erase())
        || proto.ptr_eq(known.undefined.get(heap).erase())
        || proto.ptr_eq(known.the_hole.get(heap).erase())
    {
        return true;
    }
    push_parent_map(heap, scope, proto, out)
}

fn push_parent_map<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    word: Tagged<'_, Value>,
    out: &mut Vec<Handle<'s, Map>>,
) -> bool {
    if !word.is_strong_ptr() {
        return false;
    }
    let known = heap.known();
    if word.ptr_eq(known.null.get(heap).erase())
        || word.ptr_eq(known.undefined.get(heap).erase())
        || word.ptr_eq(known.the_hole.get(heap).erase())
    {
        return false;
    }
    let Some(obj) = word.as_heap_object() else {
        return false;
    };
    let kind = obj.map(heap).kind();
    if kind.is_proxy() || !kind.kind().is_js_receiver() {
        return false;
    }
    out.push(scope.handle(obj.map(heap)));
    true
}

/// Register `user` with every trackable ancestor.
fn register_prototype_user_chain<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    user: Handle<'s, Map>,
) {
    let mut current = user;
    loop {
        let proto_word = current.as_tagged(heap).prototype.get(heap);
        let Some(proto) = trackable_prototype(heap, proto_word) else {
            break;
        };
        let proto_map = scope.handle(proto.map(heap));
        proto_map.as_tagged(heap).mark_prototype(heap);
        let info = Prototype::get_or_create_prototype_info(heap, scope, proto_map);
        let current_info = Prototype::get_or_create_prototype_info(heap, scope, current);
        let hint = current_info.as_tagged(heap).registry_slot();
        let slot = append_prototype_child(heap, scope, info, current, hint);
        if hint != slot as i64 {
            current_info.as_tagged(heap).set_registry_slot(slot);
        }
        current = proto_map;
    }
}

const INITIAL_USERS_CAPACITY: usize = 2;

/// Capacity for `len` entries, growing from the used length.
fn users_capacity_for(len: usize) -> usize {
    len + (len / 2).max(2)
}

/// Append dependent map `child` to `info`'s registry and return its slot.
/// `hint` is the caller's cached slot, validated by identity.
///
/// The lock is never held across an allocation: a mutator waiting for it
/// could not reach a safepoint.
fn append_prototype_child<'s, T: 's>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    info: Handle<'s, PrototypeInfo>,
    child: Handle<'s, T>,
    hint: i64,
) -> usize {
    enum Plan<'s> {
        Init,
        Grow(Handle<'s, WeakFixedArray>, usize, usize),
    }

    // A validated hint means the child is already registered.
    if let Some(array) = info.as_tagged(heap).prototype_users(heap)
        && let Some(index) = hint_index(
            hint,
            array.len(),
            heap,
            array,
            child.as_tagged(heap).erase(),
        )
    {
        return index;
    }

    loop {
        let plan = {
            let _guard = heap.prototype_registry().lock();
            let info_ref = info.as_tagged(heap);
            match info_ref.prototype_users(heap) {
                None => Plan::Init,
                Some(array) => {
                    let len = info_ref.users_len();
                    let child_word = child.as_tagged(heap).erase();
                    if let Some(slot) = find_registered(heap, array, len, hint, child_word) {
                        return slot;
                    }
                    if len == array.len() {
                        let live = count_live(heap, array, len);
                        Plan::Grow(array.as_handle(scope), len, live)
                    } else {
                        array.set(heap, len, child_word.as_weak());
                        info_ref.set_users_len(heap, len + 1);
                        return len;
                    }
                }
            }
        };

        match plan {
            Plan::Init => {
                let array = heap
                    .allocate_hole_weak_array(INITIAL_USERS_CAPACITY)
                    .as_handle(scope);
                let _guard = heap.prototype_registry().lock();
                let info_ref = info.as_tagged(heap);
                let expected = info_ref.prototype_users.load_word(heap);
                if info_ref.prototype_users.decode(heap, expected).is_some() {
                    continue;
                }
                array
                    .as_tagged(heap)
                    .set(heap, 0, child.as_tagged(heap).erase().as_weak());
                info_ref
                    .prototype_users
                    .publish(
                        heap,
                        info.as_tagged(heap).erase(),
                        expected,
                        array.as_tagged(heap),
                    )
                    .expect("uncontended user registry publication");
                info_ref.set_users_len(heap, 1);
                return 0;
            }
            Plan::Grow(old, len, live) => {
                let capacity = users_capacity_for(live + 1).max(INITIAL_USERS_CAPACITY);
                let array = heap.allocate_hole_weak_array(capacity).as_handle(scope);
                let _guard = heap.prototype_registry().lock();
                let info_ref = info.as_tagged(heap);
                let Some(current) = info_ref.prototype_users(heap) else {
                    continue;
                };
                if !current.ptr_eq(old.as_tagged(heap)) || info_ref.users_len() != len {
                    continue;
                }
                // Compaction drops cleared entries.
                let mut written = 0;
                for i in 0..len {
                    let entry = old.as_tagged(heap).element_slot(i).get(heap);
                    if entry.as_strong().is_none() {
                        continue;
                    }
                    array.as_tagged(heap).set(heap, written, entry);
                    written += 1;
                }
                array
                    .as_tagged(heap)
                    .set(heap, written, child.as_tagged(heap).erase().as_weak());
                info_ref
                    .prototype_users
                    .publish(
                        heap,
                        info.as_tagged(heap).erase(),
                        current.raw().to_bits(),
                        array.as_tagged(heap),
                    )
                    .expect("uncontended user registry publication");
                info_ref.set_users_len(heap, written + 1);
                return written;
            }
        }
    }
}

/// The cached slot if it still names `child`.
fn hint_index(
    hint: i64,
    len: usize,
    heap: &Heap,
    array: Tagged<'_, WeakFixedArray>,
    child: Tagged<'_, Value>,
) -> Option<usize> {
    let index = usize::try_from(hint).ok()?;
    (index < len && same_user(heap, array, index, child)).then_some(index)
}

fn find_registered(
    heap: &Heap,
    array: Tagged<'_, WeakFixedArray>,
    len: usize,
    hint: i64,
    child: Tagged<'_, Value>,
) -> Option<usize> {
    if let Some(index) = hint_index(hint, len, heap, array, child) {
        return Some(index);
    }
    (0..len).find(|&index| same_user(heap, array, index, child))
}

fn same_user(
    heap: &Heap,
    array: Tagged<'_, WeakFixedArray>,
    index: usize,
    child: Tagged<'_, Value>,
) -> bool {
    array
        .element_slot(index)
        .get(heap)
        .as_strong()
        .is_some_and(|user| user.ptr_eq(child))
}

fn count_live(heap: &Heap, array: Tagged<'_, WeakFixedArray>, len: usize) -> usize {
    (0..len)
        .filter(|&i| array.element_slot(i).get(heap).as_strong().is_some())
        .count()
}

fn invalidate_one_prototype_validity_cell(heap: &Heap, map: Tagged<'_, Map>) {
    if let Some(cell) = map.published_validity_cell(heap)
        && cell.is_valid(heap)
    {
        cell.clear();
    }
}

/// Walk dependent maps; multi-parent maps are terminal because they cover
/// their whole closure.
fn walk_dependent_maps(
    heap: &Heap,
    map: Tagged<'_, Map>,
    path: &mut Vec<usize>,
    visit: &mut impl FnMut(Tagged<'_, Map>, bool),
) {
    let id = map.raw_addr() as usize;
    if path.contains(&id) {
        return;
    }
    let Some(info) = map.try_get_prototype_info(heap) else {
        return;
    };
    let Some(users) = info.prototype_users(heap) else {
        return;
    };
    path.push(id);
    for i in 0..users.len() {
        let Some(child) = users.element_slot(i).get(heap).as_strong() else {
            continue;
        };
        let Some(user_map) = child.get_as::<Map>(heap) else {
            continue;
        };
        let multi_parent = user_map.is_multi_parent();
        visit(user_map, multi_parent);
        if !multi_parent {
            walk_dependent_maps(heap, user_map, path, visit);
        }
    }
    path.pop();
}

fn invalidate_prototype_chains(heap: &Heap, map: Tagged<'_, Map>) {
    invalidate_one_prototype_validity_cell(heap, map);
    let mut path = Vec::new();
    walk_dependent_maps(heap, map, &mut path, &mut |user_map, _multi_parent| {
        // One clear per dependent map; a multi-parent cell covers its closure.
        invalidate_one_prototype_validity_cell(heap, user_map);
    });
}

/// Mark dependent parent-list maps after an ancestor becomes callable.
fn propagate_callable_parent(heap: &Heap, map: Tagged<'_, Map>) {
    let mut path = Vec::new();
    walk_dependent_maps(heap, map, &mut path, &mut |user_map, multi_parent| {
        if multi_parent {
            user_map.mark_callable_parent(heap);
        }
    });
}
