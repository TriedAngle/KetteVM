use crate::{
    AccessorPair, CallTarget, CallableInfoObject, Cell, ContextObject, DataHandler,
    DataHandlerInit, DenseString, FeedbackVector, FixedArray, FunctionKind, Handle, HandleScope,
    Heap, LanguageMode, Map, MaybeWeak, NativeIndex, Object, ObjectKind, Prototype, ShapeChange,
    SiteRead, SlotName, Smi, Tagged, Value, WeakFixedArray, WeakFixedArrayInit, info_is_immutable,
};

/// Beyond this many live (map, handler) pairs a site goes megamorphic.
pub const MAX_POLYMORPHIC_ENTRIES: usize = 4;

const KIND_FIELD: i64 = 0;
const KIND_SLOW: i64 = 1;
/// Dense array element load; payload = flags (holeyness is pinned by the
/// map check in the probe: a packed handler can only ever run against
/// the packed map it was recorded for).
const KIND_ELEMENT: i64 = 2;
/// Dense array element store; payload = flags.
const KIND_ELEMENT_STORE: i64 = 3;
/// `DenseString` index load; payload = flags.
const KIND_INDEXED_STRING: i64 = 4;
/// Array `length`: read the receiver's length slot directly (the map check
/// in the probe already pinned it to an array map). Payload unused.
const KIND_ARRAY_LENGTH: i64 = 5;
/// Prototype getter: `data[0]` is the weak `AccessorPair`; the receiver's
/// chain is guarded by the handler's validity cell.
const KIND_ACCESSOR: i64 = 6;
/// Prototype setter: `data[0]` is the weak `AccessorPair`; the receiver's
/// chain is guarded by the handler's validity cell.
const KIND_SETTER: i64 = 7;
/// Cached miss: the guarded chain has no such property.
const KIND_NON_EXISTENT: i64 = 8;
/// Self-style parent-name slot: `data[0]` is the weak holder object; the
/// payload is the pair element index holding the value.
const KIND_PARENT: i64 = 9;
/// Native accessor setter: payload is the setter's runtime index. The map
/// check pins the descriptor, so the cached index cannot go stale.
const KIND_NATIVE_SETTER: i64 = 10;
/// Script-context lexical binding cached on a global load/store site.
/// Payload = slot index in the low 32 bits, binding info in the next byte.
const KIND_GLOBAL_LEXICAL: i64 = 11;
/// Native accessor getter: `data[0]` is the `AccessorInfo`; the load handler
/// reads the getter index from it and calls the native directly.
const KIND_NATIVE_GETTER: i64 = 12;
const KIND_INDEX_GET: i64 = 13;
const KIND_INDEX_SET: i64 = 14;

/// Debug name for the kind byte of a directive handler word.
pub fn handler_kind_name(kind: i64) -> &'static str {
    match kind {
        KIND_FIELD => "field",
        KIND_SLOW => "slow",
        KIND_ELEMENT => "element",
        KIND_ELEMENT_STORE => "element-store",
        KIND_INDEXED_STRING => "indexed-string",
        KIND_ARRAY_LENGTH => "array-length",
        KIND_ACCESSOR => "accessor",
        KIND_SETTER => "setter",
        KIND_NON_EXISTENT => "non-existent",
        KIND_PARENT => "parent",
        KIND_NATIVE_SETTER => "native-setter",
        KIND_GLOBAL_LEXICAL => "global-lexical",
        KIND_NATIVE_GETTER => "native-getter",
        KIND_INDEX_GET => "index-get",
        KIND_INDEX_SET => "index-set",
        _ => "unknown",
    }
}
const GLOBAL_LEXICAL_INFO_SHIFT: i64 = 32;
const GLOBAL_LEXICAL_SLOT_MASK: i64 = 0xffff_ffff;

/// Element-load payload flags.
const ELEMENT_HOLEY: i64 = 1 << 0;
const ELEMENT_ALLOW_OOB: i64 = 1 << 1;

/// Element-store payload flags.
const STORE_HOLEY: i64 = 1 << 0;
const STORE_GROW: i64 = 1 << 1;
/// The prototype chain was verified element-free at install time and
/// the handler is guarded by its validity cell: stores into holes and
/// appends past `length` may proceed without consulting the chain.
const STORE_CHAIN_CLEAN: i64 = 1 << 2;

/// Result of the inlined monomorphic probe (see [`InlineCache::probe_mono`]).
pub enum MonoProbe<'a> {
    /// a hot-kind handler (bare field / one-hop prototype field) loaded
    /// inline
    Value(Tagged<'a, Value>),
    /// mono (or first-pair poly) match whose handler needs the out-of-line
    /// application (accessors, chains, slow)
    Handler(Tagged<'a, Object>, Tagged<'a, MaybeWeak<Value>>),
    /// a polymorphic site whose first pair was tried inline: continue the
    /// search from `start`
    Poly {
        obj: Tagged<'a, Object>,
        map: Tagged<'a, Map>,
        pairs: Tagged<'a, WeakFixedArray>,
        start: usize,
    },
    /// megamorphic / weak mismatch / unusable state: go cold
    Miss,
    /// not a heap receiver (or no feedback): the cold path decides
    NotReceiver,
}

/// Result of a load hit.
pub enum LoadHit<'a> {
    Value(Tagged<'a, Value>),
    /// invoke with the receiver as `this`
    Getter(Tagged<'a, Value>),
    /// invoke the native accessor whose `AccessorInfo` this is
    NativeGetter(Tagged<'a, Value>),
    NotFound,
}

/// Result of a cached global script-context lexical load.
pub enum GlobalLexicalHit<'a> {
    Value(Tagged<'a, Value>),
    /// The binding exists but is still in its TDZ.
    Uninitialized,
}

/// Result of a keyed element load hit.
pub enum ElementHit<'a> {
    Value(Tagged<'a, Value>),
    /// a single-code-unit string load (`"ab"[1]`); the caller allocates
    Char(u16),
    /// a guarded missing element; only returned by the or-hole probe.
    NotFound,
}

/// Result of a store hit.
pub enum StoreHit<'a> {
    /// store completed
    Done,
    /// invoke with (receiver, value)
    Setter(Tagged<'a, Value>),
    NativeSetter {
        index: NativeIndex,
        accessor: Tagged<'a, Value>,
    },
}

/// What the slow-path store decided.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcomeKind {
    /// own writable data written (or a silently ignored failure)
    Done,
    /// property added: the receiver changed maps
    Transition,
    /// a setter was (or will be) invoked
    CallSetter,
}

/// A built handler, fully rooted.
enum Handler<'s> {
    /// A bare `(kind, payload)` directive: an own field, array length, a
    /// native setter, a cached miss, `Slow`, or an element handler.
    Smi(Smi),
    /// A guarded handler (prototype field/accessor/setter/parent-name/miss).
    Data(Handle<'s, DataHandler>),
    /// An own accessor, stored as a whole weak pair.
    WeakPair(Handle<'s, AccessorPair>),
    /// A store transition: migrate the receiver to this map.
    WeakMap(Handle<'s, Map>),
}

/// What the load analysis found, rooted so it survives handler
/// construction.
#[derive(Clone, Copy)]
struct Plan<'s> {
    kind: PlanKind,
    /// field offset (data) or parent-pair element index (parent name)
    offset: usize,
    receiver_map: Handle<'s, Map>,
    pair: Option<Handle<'s, AccessorPair>>,
    native_accessor: Option<Handle<'s, Value>>,
    holder: Option<Handle<'s, Object>>,
    /// the property is an own property of the receiver (no guarded chain)
    own: bool,
}

#[derive(Clone, Copy)]
enum PlanKind {
    Field,
    ArrayLength,
    Slow,
    Accessor,
    NativeGetter,
    ParentName,
    NonExistent,
}

struct StorePlan<'s> {
    field: Option<usize>,
    setter: Option<Handle<'s, AccessorPair>>,
    native_setter: Option<NativeIndex>,
    native_accessor: Option<Handle<'s, Value>>,
    /// the setter is an own accessor (no guarded chain)
    setter_own: bool,
    receiver_map: Handle<'s, Map>,
}

enum StoreAction<'s> {
    Field(usize),
    Transition(Handle<'s, Map>),
    Setter(Handle<'s, Value>),
    NativeSetter {
        index: NativeIndex,
        accessor: Handle<'s, Value>,
    },
    /// setter-less accessor: writes are silently ignored
    Noop,
}

/// A monomorphic bytecode-callee hit: everything a frame push needs,
/// pre-decoded.
pub struct CallHit<'a> {
    pub target: Tagged<'a, Object>,
    pub info: Tagged<'a, CallableInfoObject>,
    pub context: Tagged<'a, ContextObject>,
    pub kind: FunctionKind,
}

pub enum CallProbe<'a> {
    Bytecode(CallHit<'a>),
    Native(NativeIndex),
    Miss,
}

/// What the generic path resolved a call site's callee to.
pub enum CallRecord<'a> {
    Bytecode(Tagged<'a, CallableInfoObject>),
    Native(NativeIndex),
}

pub struct InlineCache;

fn kind_smi(kind: i64, payload: i64) -> Smi {
    Smi::new(kind | (payload << 8))
}

fn decode_global_lexical(payload: i64) -> (usize, i64) {
    (
        (payload & GLOBAL_LEXICAL_SLOT_MASK) as usize,
        (payload >> GLOBAL_LEXICAL_INFO_SHIFT) & 0xff,
    )
}

#[inline]
fn global_lexical_site<'a>(
    heap: &'a Heap,
    vector: Option<Tagged<'a, FeedbackVector>>,
    slot: usize,
) -> Option<(Tagged<'a, ContextObject>, usize, i64)> {
    let vector = vector?;
    let site = vector.read_pair(heap, slot);
    let context = site.state.as_strong()?.get_as::<ContextObject>(heap)?;
    let (kind, payload) = decode_handler_smi(site.handler)?;
    if kind != KIND_GLOBAL_LEXICAL {
        return None;
    }
    let (slot_index, info) = decode_global_lexical(payload);
    Some((context, slot_index, info))
}

/// Decode the `(kind, payload)` view of a handler word.
#[inline]
fn decode_handler_smi<T>(word: Tagged<'_, T>) -> Option<(i64, i64)> {
    let v = word.to_i64()?;
    Some((v & 0xff, v >> 8))
}

/// `Some` only for ordinary JSReceiver objects (not proxies): the only
/// receivers whose lookups the map walk fully describes.
#[inline(always)]
fn ic_receiver<'a>(receiver: Tagged<'a, Value>, heap: &'a Heap) -> Option<Tagged<'a, Object>> {
    let obj = receiver.as_heap_object()?;
    let kind = obj.map(heap).kind();
    if !kind.is_js_receiver() || kind.is_proxy() {
        return None;
    }
    Some(obj)
}

/// The IC identity and lookup origin for a receiver.
///
/// Primitives use their canonical maps (Smi has no heap object, so the
/// synthetic `smi_map` stands in). Nullish receivers have no prototype
/// chain of their own, so operators on them look up through
/// `Object.prototype`.
#[inline(always)]
fn ic_origin<'a>(
    heap: &'a Heap,
    receiver: Tagged<'a, Value>,
) -> Option<(Tagged<'a, Map>, Tagged<'a, Object>)> {
    if let Some(obj) = receiver.as_heap_object() {
        let kind = obj.map(heap).kind();
        if kind.is_proxy() {
            return None;
        }
        if kind.kind() == ObjectKind::Oddball && receiver.is_nullish(heap) {
            return Some((obj.map(heap), heap.known().object_prototype.get(heap)));
        }
        if !kind.is_js_receiver()
            && !matches!(kind.kind(), ObjectKind::Float | ObjectKind::FixedByteArray)
        {
            return None;
        }
        return Some((obj.map(heap), obj));
    }
    if receiver.is_smi() {
        let origin = receiver.primitive_prototype(heap)?.as_heap_object()?;
        return Some((heap.known().smi_map.get(heap), origin));
    }
    None
}

/// The original map-keyed probe body. Kept in a helper so the JS heap-object
/// path stays identical to before the primitive-map work.
#[inline(always)]
fn probe_map<'a>(
    heap: &'a Heap,
    vector: Tagged<'a, FeedbackVector>,
    slot: usize,
    map: Tagged<'a, Map>,
    obj: Tagged<'a, Object>,
) -> MonoProbe<'a> {
    let site = read_site_snapshot(vector, heap, slot);
    let state = site.state;

    if state.is_weak_ref_to(map) {
        return mono_result(heap, obj, site.handler);
    }
    if state.is_weak_ptr() {
        return MonoProbe::Miss;
    }

    let Some(pairs) = state
        .as_strong()
        .and_then(|s| s.get_as::<WeakFixedArray>(heap))
    else {
        return MonoProbe::Miss;
    };
    let len = pairs.len();
    if len >= 2
        && let Some(m) = pairs.get(heap, 0).as_strong()
        && m.ptr_eq(map)
    {
        return mono_result(heap, obj, pairs.get(heap, 1));
    }
    if len >= 4
        && let Some(m) = pairs.get(heap, 2).as_strong()
        && m.ptr_eq(map)
    {
        return mono_result(heap, obj, pairs.get(heap, 3));
    }
    MonoProbe::Poly {
        obj,
        map,
        pairs,
        start: if len >= 4 { 4 } else { 2 },
    }
}

/// Snapshot a property/call site. Strong states are self-contained; only a
/// weak mono state needs the pair snapshot.
#[inline(always)]
fn read_site_snapshot<'a>(
    vector: Tagged<'_, FeedbackVector>,
    heap: &'a Heap,
    slot: usize,
) -> SiteRead<'a> {
    let state = vector.read_site(heap, slot);
    if state.is_weak_ptr() {
        vector.read_pair(heap, slot)
    } else {
        SiteRead {
            state,
            handler: Tagged::CLEARED,
        }
    }
}

/// Handler word for `map`'s monomorphic or polymorphic entry.
#[inline(always)]
fn probe<'a>(
    heap: &'a Heap,
    site: SiteRead<'a>,
    map: Tagged<'a, Map>,
) -> Option<Tagged<'a, MaybeWeak<Value>>> {
    let state = site.state;
    if state.is_weak_ptr() {
        if !state.ptr_eq(map) {
            return None;
        }
        return Some(site.handler);
    }
    let pairs = state.as_strong()?.get_as::<WeakFixedArray>(heap)?;
    let len = pairs.len();
    let mut i = 0;
    while i + 1 < len {
        if let Some(m) = pairs.get(heap, i).as_strong()
            && m.ptr_eq(map)
        {
            return Some(pairs.get(heap, i + 1));
        }
        i += 2;
    }
    None
}

#[inline(always)]
fn probe_element_smi<'a>(
    heap: &'a Heap,
    vector: Option<Tagged<'a, FeedbackVector>>,
    slot: usize,
    receiver: Tagged<'a, Value>,
) -> Option<(Tagged<'a, Object>, Tagged<'a, Map>, i64, i64)> {
    let vector = vector?;
    let site = read_site_snapshot(vector, heap, slot);
    let obj = receiver.as_heap_object()?;
    let map = obj.map(heap);
    let handler = probe(heap, site, map)?;
    if handler.is_ptr() {
        return None;
    }
    let (kind, payload) = decode_handler_smi(handler)?;
    Some((obj, map, kind, payload))
}

fn probe_index_method<'a>(
    heap: &'a Heap,
    vector: Option<Tagged<'a, FeedbackVector>>,
    slot: usize,
    receiver: Tagged<'a, Value>,
    kind: i64,
) -> Option<Tagged<'a, Value>> {
    let vector = vector?;
    let site = read_site_snapshot(vector, heap, slot);
    let obj = receiver.as_heap_object()?;
    let map = obj.map(heap);
    let handler = probe(heap, site, map)?;
    if !handler.is_ptr() || handler.is_weak_ptr() {
        return None;
    }
    let handler = handler.as_strong()?.get_as::<DataHandler>(heap)?;
    apply_index_method(heap, handler, kind)
}

/// Whether `map`'s published chain validity cell is still valid — the
/// guard for handler payload bits that promise a chain property (holes
/// resolve to `undefined`, hole stores are plain).
#[inline(always)]
fn map_chain_cell_valid(heap: &Heap, map: Tagged<'_, Map>) -> bool {
    map.published_validity_cell(heap)
        .is_some_and(|cell| cell.is_valid(heap))
}

/// Publish the chain validity cell on `map` so hit paths can guard a
/// chain-dependent payload bit with two loads off the receiver map.
/// `false` when the chain cannot be tracked.
fn attach_chain_cell(heap: &mut Heap, scope: &HandleScope<'_>, map: &Handle<'_, Map>) -> bool {
    match Prototype::get_or_create_prototype_chain_validity_cell(heap, scope, *map) {
        Some(cell) => {
            map.as_tagged(heap)
                .set_validity_cell(heap, cell.as_tagged(heap));
            true
        }
        None => false,
    }
}

impl Handler<'_> {
    fn word<'a>(&self, heap: &'a Heap) -> Tagged<'a, MaybeWeak<Value>> {
        match self {
            Self::Smi(s) => s.into_tagged().as_maybe_weak(),
            Self::Data(dh) => dh.as_tagged(heap).erase().as_maybe_weak(),
            Self::WeakPair(pair) => pair.as_tagged(heap).erase().as_weak(),
            Self::WeakMap(m) => m.as_tagged(heap).erase().as_weak(),
        }
    }
}

/// Build a guarded handler. `None` when the receiver's prototype closure
/// cannot be tracked (a proxy or non-receiver prototype).
fn build_data_handler<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    receiver_map: &Handle<'s, Map>,
    smi_handler: Smi,
    data: &[Handle<'s, Value>],
) -> Option<Handle<'s, DataHandler>> {
    let cell = Prototype::get_or_create_prototype_chain_validity_cell(heap, scope, *receiver_map)?;
    let count = data.len();
    Some(
        heap.allocate_token_enter_heap(DataHandler::layout_for(count), |token, heap| {
            let validity_cell = cell.as_tagged(heap).erase().as_maybe_weak();
            let words: Vec<Tagged<'_, MaybeWeak<Value>>> = data
                .iter()
                .map(|word| word.as_tagged(heap).erase().as_weak())
                .collect();
            token.allocate_handle::<DataHandler>(
                DataHandlerInit {
                    smi_handler,
                    validity_cell,
                    data: &words,
                },
                scope,
            )
        }),
    )
}

/// Build a guarded handler, falling back to `Slow` when the closure cannot
/// be tracked.
fn guarded<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    receiver_map: &Handle<'s, Map>,
    smi_handler: Smi,
    data: &[Handle<'s, Value>],
) -> Handler<'s> {
    match build_data_handler(heap, scope, receiver_map, smi_handler, data) {
        Some(dh) => Handler::Data(dh),
        None => Handler::Smi(kind_smi(KIND_SLOW, 0)),
    }
}

/// Whether `map` has any prototype edge at all (object parent or a
/// Self-style parent list).
fn has_parents(heap: &Heap, map: Tagged<'_, Map>) -> bool {
    let proto = map.prototype.get(heap);
    if !proto.is_strong_ptr() {
        return false;
    }
    let known = heap.known();
    !(proto.ptr_eq(known.null.get(heap).erase())
        || proto.ptr_eq(known.undefined.get(heap).erase())
        || proto.ptr_eq(known.the_hole.get(heap).erase()))
}

/// Whether a prototype-chain object contributes no index-keyed lookups:
/// no integer-named descriptors and (for arrays) no non-hole elements.
fn object_has_no_elements(heap: &Heap, word: Tagged<'_, Value>) -> bool {
    object_has_no_elements_at(heap, word, 64)
}

fn object_has_no_elements_at(heap: &Heap, word: Tagged<'_, Value>, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let Some(obj) = word.get_as::<Object>(heap) else {
        return false;
    };
    let map = obj.map(heap);
    let kind = map.kind();
    if kind.is_proxy() || !matches!(kind.kind(), ObjectKind::Object | ObjectKind::Array) {
        return false;
    }
    for d in map.descriptors() {
        if d.name(heap).to_i64().is_some() {
            return false;
        }
    }
    if kind.is_array() {
        if !kind.is_dense_elements() {
            // a dictionary-backed hop may hold elements
            return false;
        }
        let elements = obj.elements_array(heap);
        if let Some(elements) = elements {
            let hole = heap.known().the_hole.get(heap);
            for i in 0..elements.len() {
                if elements.at(heap, i) != hole {
                    return false;
                }
            }
        }
    }
    let proto = map.prototype.get(heap);
    if let Some(pairs) = proto.get_as::<FixedArray>(heap) {
        let mut i = 1;
        while i < pairs.len() {
            if !object_has_no_elements_at(heap, pairs.at(heap, i), depth - 1) {
                return false;
            }
            i += 2;
        }
    } else if has_parents(heap, map) && !object_has_no_elements_at(heap, proto, depth - 1) {
        return false;
    }
    true
}

/// Whether the prototype chain starting at `map`'s parents holds no
/// index-keyed properties, so element misses may resolve to
/// `undefined` without walking it. Install-time decision; staleness is
/// guarded by the chain validity cell attached to the handler.
fn chain_has_no_elements(heap: &Heap, map: Tagged<'_, Map>) -> bool {
    let mut current = map;
    loop {
        let proto = current.prototype.get(heap);
        if let Some(pairs) = proto.get_as::<FixedArray>(heap) {
            let mut i = 1;
            while i < pairs.len() {
                if !object_has_no_elements(heap, pairs.at(heap, i)) {
                    return false;
                }
                i += 2;
            }
            return true;
        }
        if !has_parents(heap, current) {
            return true;
        }
        if !object_has_no_elements(heap, proto) {
            return false;
        }
        let Some(obj) = proto.get_as::<Object>(heap) else {
            return false;
        };
        current = obj.map(heap);
    }
}

impl<'s> Plan<'s> {
    fn non_existent(&self) -> bool {
        matches!(self.kind, PlanKind::NonExistent)
    }

    fn into_handler(self, heap: &mut Heap, scope: &'s HandleScope<'_>) -> Handler<'s> {
        let receiver_map = &self.receiver_map;
        match self.kind {
            // Own data field: a bare, chain-free Smi handler.
            PlanKind::Field if self.own => Handler::Smi(kind_smi(KIND_FIELD, self.offset as i64)),
            PlanKind::Field => guarded(
                heap,
                scope,
                receiver_map,
                kind_smi(KIND_FIELD, self.offset as i64),
                &[self.holder.expect("prototype field has a holder").erase()],
            ),
            PlanKind::ArrayLength => Handler::Smi(kind_smi(KIND_ARRAY_LENGTH, 0)),
            PlanKind::Slow => Handler::Smi(kind_smi(KIND_SLOW, 0)),
            PlanKind::NativeGetter => guarded(
                heap,
                scope,
                receiver_map,
                kind_smi(KIND_NATIVE_GETTER, 0),
                &[self.native_accessor.expect("native getter has an accessor")],
            ),
            // Own accessor: the whole weak pair.
            PlanKind::Accessor if self.own => {
                let pair = self.pair.expect("accessor has a pair");
                Handler::WeakPair(pair.as_tagged(heap).as_handle(scope))
            }
            PlanKind::Accessor => guarded(
                heap,
                scope,
                receiver_map,
                kind_smi(KIND_ACCESSOR, 0),
                &[self.pair.expect("accessor has a pair").erase()],
            ),
            PlanKind::ParentName => guarded(
                heap,
                scope,
                receiver_map,
                kind_smi(KIND_PARENT, self.offset as i64),
                &[self.holder.expect("parent name has a holder").erase()],
            ),
            PlanKind::NonExistent if self.own => Handler::Smi(kind_smi(KIND_NON_EXISTENT, 0)),
            PlanKind::NonExistent => guarded(
                heap,
                scope,
                receiver_map,
                kind_smi(KIND_NON_EXISTENT, 0),
                &[],
            ),
        }
    }
}

/// Analyze a load into a cacheable plan.
///
/// `start` is where the prototype walk begins (the receiver itself, or
/// `Object.prototype` for nullish receivers); `identity` is the map the
/// site is guarded on; `receiver` is the actual value, used to decide
/// whether a hit is an own property of the receiver.
fn analyze_load<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    start: Handle<'_, Value>,
    receiver: Tagged<'_, Value>,
    identity: Handle<'s, Map>,
    name: Handle<'_, SlotName>,
) -> Plan<'s> {
    let receiver_map = identity;
    let mk = |kind, offset, pair, holder, own| Plan {
        kind,
        offset,
        receiver_map,
        pair,
        native_accessor: None,
        holder,
        own,
    };
    // side-effect-free: no getters or traps run while building the plan
    let hit = Lookup::new(heap, scope, start, name.erase(), false).next(heap, scope);
    use crate::lookup::{Hit, Lookup};
    let cacheable = |holder: Handle<'s, Object>| -> bool {
        holder
            .as_tagged(heap)
            .map(heap)
            .kind()
            .kind()
            .is_js_receiver()
    };
    let receiver_object = receiver.as_heap_object();
    let own = |holder: Tagged<'_, Object>| -> bool {
        receiver_object.is_some_and(|receiver| holder.ptr_eq(receiver))
    };
    match hit {
        Hit::Data { holder, offset, .. } => {
            let holder = scope.handle(holder);
            if !cacheable(holder) {
                return mk(PlanKind::Slow, 0, None, None, false);
            }
            mk(
                PlanKind::Field,
                offset,
                None,
                Some(holder),
                own(holder.as_tagged(heap)),
            )
        }
        Hit::NativeAccessor { .. }
            if receiver_object.is_some_and(|receiver| {
                receiver.array_length(heap, name.as_tagged(heap)).is_some()
            }) =>
        {
            mk(PlanKind::ArrayLength, 0, None, None, true)
        }
        Hit::NativeAccessor { holder, info, .. } => {
            let holder = scope.handle(holder);
            let own = own(holder.as_tagged(heap));
            let mut plan = mk(PlanKind::NativeGetter, 0, None, Some(holder), own);
            plan.native_accessor = Some(scope.handle(info.erase()));
            plan
        }
        Hit::Accessor { holder, pair, .. } => {
            let holder = scope.handle(holder);
            if !cacheable(holder) {
                return mk(PlanKind::Slow, 0, None, None, false);
            }
            let own = own(holder.as_tagged(heap));
            mk(
                PlanKind::Accessor,
                0,
                Some(scope.handle(pair)),
                Some(holder),
                own,
            )
        }
        Hit::Parent { holder, slot, .. } => {
            let holder = scope.handle(holder);
            if !cacheable(holder) {
                return mk(PlanKind::Slow, 0, None, None, false);
            }
            mk(PlanKind::ParentName, slot, None, Some(holder), false)
        }
        // strings/elements/proxies stay on the slow path
        Hit::StringOwn { .. } | Hit::Element { .. } | Hit::Proxy(_) => {
            mk(PlanKind::Slow, 0, None, None, false)
        }
        Hit::Miss => {
            let own = !has_parents(heap, receiver_map.as_tagged(heap));
            mk(PlanKind::NonExistent, 0, None, None, own)
        }
    }
}

fn index_method_handler<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    receiver: Handle<'_, Object>,
    name: Handle<'_, SlotName>,
    kind: i64,
) -> Option<Handler<'s>> {
    let identity = scope.handle(receiver.as_tagged(heap).map(heap));
    let plan = analyze_load(
        heap,
        scope,
        receiver.erase(),
        receiver.as_tagged(heap).erase(),
        identity,
        name,
    );
    if !matches!(plan.kind, PlanKind::Field) {
        return None;
    }
    if !chain_has_no_elements(heap, plan.receiver_map.as_tagged(heap)) {
        return None;
    }
    let holder = plan.holder?;
    Some(guarded(
        heap,
        scope,
        &plan.receiver_map,
        kind_smi(kind, plan.offset as i64),
        &[holder.erase()],
    ))
}

#[inline(always)]
fn check_prototype_validity_cell(heap: &Heap, handler: Tagged<'_, DataHandler>) -> bool {
    match handler.validity_cell(heap).as_strong() {
        None => true,
        Some(cell) => cell
            .get_as::<Cell>(heap)
            .is_some_and(|cell| cell.is_valid(heap)),
    }
}

fn getter_hit<'a>(heap: &'a Heap, pair: Tagged<'a, AccessorPair>) -> LoadHit<'a> {
    let getter = pair.get.get(heap);
    if getter == heap.known().undefined.get(heap) {
        LoadHit::Value(heap.known().undefined.get(heap).erase())
    } else {
        LoadHit::Getter(getter)
    }
}

#[inline(always)]
fn handler_holder<'a>(
    heap: &'a Heap,
    handler: Tagged<'a, DataHandler>,
) -> Option<Tagged<'a, Object>> {
    handler.data(heap, 0).as_strong()?.get_as::<Object>(heap)
}

#[cold]
#[inline(never)]
fn apply_parent_handler<'a>(
    heap: &'a Heap,
    handler: Tagged<'a, DataHandler>,
    payload: i64,
) -> Option<LoadHit<'a>> {
    let holder = handler_holder(heap, handler)?;
    let pairs = holder
        .map(heap)
        .prototype
        .get(heap)
        .get_as::<FixedArray>(heap)?;
    Some(LoadHit::Value(pairs.at(heap, payload as usize)))
}

#[inline(always)]
fn apply_index_method<'a>(
    heap: &'a Heap,
    handler: Tagged<'a, DataHandler>,
    kind: i64,
) -> Option<Tagged<'a, Value>> {
    if !check_prototype_validity_cell(heap, handler) {
        return None;
    }
    let (actual, payload) = decode_handler_smi(handler.smi_handler(heap))?;
    if actual != kind {
        return None;
    }
    let holder = handler_holder(heap, handler)?;
    Some(holder.slot(heap, payload as usize).get(heap))
}

#[inline(always)]
fn apply_data_handler<'a>(heap: &'a Heap, handler: Tagged<'a, DataHandler>) -> Option<LoadHit<'a>> {
    if !check_prototype_validity_cell(heap, handler) {
        return None;
    }
    let (kind, payload) = decode_handler_smi(handler.smi_handler(heap))?;
    match kind {
        KIND_FIELD => {
            let holder = handler_holder(heap, handler)?;
            Some(LoadHit::Value(
                holder.slot(heap, payload as usize).get(heap),
            ))
        }
        KIND_ACCESSOR => {
            let pair = handler
                .data(heap, 0)
                .as_strong()?
                .get_as::<AccessorPair>(heap)?;
            Some(getter_hit(heap, pair))
        }
        KIND_NATIVE_GETTER => Some(LoadHit::NativeGetter(handler.data(heap, 0).as_strong()?)),
        KIND_PARENT => apply_parent_handler(heap, handler, payload),
        KIND_NON_EXISTENT => Some(LoadHit::NotFound),
        _ => None,
    }
}

#[inline(always)]
fn apply_fast<'a>(
    heap: &'a Heap,
    receiver: Tagged<'a, Object>,
    handler: Tagged<'a, MaybeWeak<Value>>,
) -> Option<LoadHit<'a>> {
    if !handler.is_ptr() {
        let (kind, payload) = decode_handler_smi(handler)?;
        return match kind {
            KIND_FIELD => Some(LoadHit::Value(
                receiver.slot(heap, payload as usize).get(heap),
            )),
            KIND_ARRAY_LENGTH => Some(LoadHit::Value(receiver.length.get(heap).erase())),
            KIND_NON_EXISTENT => Some(LoadHit::NotFound),
            _ => None,
        };
    }
    if handler.is_weak_ptr() {
        // own accessor as a whole weak pair, cleared weak maps, ...
        return None;
    }
    apply_data_handler(heap, handler.as_strong()?.get_as::<DataHandler>(heap)?)
}

/// Finish an inline mono probe: the value when the interpreter can complete
/// the load itself (an own field or a guarded prototype field), otherwise
/// the handler for the out-of-line path.
#[inline(always)]
fn mono_result<'a>(
    heap: &'a Heap,
    obj: Tagged<'a, Object>,
    handler: Tagged<'a, MaybeWeak<Value>>,
) -> MonoProbe<'a> {
    match apply_fast(heap, obj, handler) {
        Some(LoadHit::Value(v)) => MonoProbe::Value(v),
        _ => MonoProbe::Handler(obj, handler),
    }
}

#[inline(always)]
fn apply_load_handler<'a>(
    heap: &'a Heap,
    receiver: Tagged<'a, Object>,
    handler: Tagged<'a, MaybeWeak<Value>>,
) -> Option<LoadHit<'a>> {
    // A weak word is an own accessor stored as a whole pair; everything else
    // (bare Smi or strong DataHandler) is handled by the fast path.
    if handler.is_weak_ptr() {
        let pair = handler.as_strong()?.get_as::<AccessorPair>(heap)?;
        return Some(getter_hit(heap, pair));
    }
    apply_fast(heap, receiver, handler)
}

/// Execute a dense-array element load handler.
#[inline]
fn apply_element_load<'a>(
    heap: &'a Heap,
    obj: Tagged<'a, Object>,
    map: Tagged<'a, Map>,
    index: usize,
    payload: i64,
    or_hole: bool,
) -> Option<ElementHit<'a>> {
    let holey = payload & ELEMENT_HOLEY != 0;
    let allow_oob = payload & ELEMENT_ALLOW_OOB != 0;
    let oob = |heap: &'a Heap| {
        (allow_oob && map_chain_cell_valid(heap, map)).then(|| {
            if or_hole {
                ElementHit::NotFound
            } else {
                ElementHit::Value(heap.known().undefined.get(heap).erase())
            }
        })
    };
    let len = obj.length();
    if index >= len {
        return oob(heap);
    }
    let elements = obj.elements_array(heap)?;
    if holey {
        // `length` can exceed the backing store: those indices are holes
        if index >= elements.len() {
            return oob(heap);
        }
        let v = elements.at(heap, index);
        if v == heap.known().the_hole.get(heap) {
            return oob(heap);
        }
        Some(ElementHit::Value(v))
    } else {
        // packed: capacity covers `length`, and no slot is a hole
        Some(ElementHit::Value(elements.at(heap, index)))
    }
}

/// Execute an indexed-string load handler.
fn apply_indexed_string<'a>(
    heap: &'a Heap,
    receiver: Tagged<'a, Value>,
    index: usize,
    payload: i64,
    or_hole: bool,
) -> Option<ElementHit<'a>> {
    let s = receiver.get_as::<DenseString>(heap)?;
    if index >= s.len() {
        if payload & ELEMENT_ALLOW_OOB != 0
            && receiver
                .as_heap_object()
                .is_some_and(|h| map_chain_cell_valid(heap, h.map(heap)))
        {
            return Some(if or_hole {
                ElementHit::NotFound
            } else {
                ElementHit::Value(heap.known().undefined.get(heap).erase())
            });
        }
        return None;
    }
    Some(ElementHit::Char(s.code_unit(heap, index)))
}

/// Store semantics over the shared walk: own writable data → field,
/// user accessor anywhere → setter, own native accessor → native setter,
/// everything else → Slow.
fn analyze_store<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    receiver: Tagged<'_, Object>,
    name: Tagged<'_, SlotName>,
) -> StorePlan<'s> {
    let mut plan = StorePlan {
        field: None,
        setter: None,
        native_setter: None,
        native_accessor: None,
        setter_own: false,
        receiver_map: scope.handle(receiver.map(heap)),
    };
    let hit = {
        let recv_handle = scope.handle(receiver.erase());
        let key_handle = scope.handle(name.erase());
        Lookup::new(heap, scope, recv_handle, key_handle, false).next(heap, scope)
    };
    use crate::lookup::{Hit, Lookup};
    match hit {
        Hit::Data { holder, offset, .. } if holder.erase().ptr_eq(receiver.erase()) => {
            plan.field = Some(offset)
        }
        Hit::Accessor { holder, pair, .. } => {
            plan.setter_own = holder.erase().ptr_eq(receiver.erase());
            plan.setter = Some(scope.handle(pair));
        }
        // Only own native accessors cache a setter; inherited writes define
        // on the receiver and stay slow.
        Hit::NativeAccessor { holder, info, .. } if holder.erase().ptr_eq(receiver.erase()) => {
            plan.native_setter = info.setter();
            plan.native_accessor = Some(scope.handle(info.erase()));
        }
        _ => {}
    }
    plan
}

fn store_handler<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    transition_target: Option<Handle<'s, Map>>,
    plan: StorePlan<'s>,
) -> Handler<'s> {
    if let Some(target) = transition_target {
        if !target.as_tagged(heap).is_prototype() {
            Prototype::ensure_store_transition_validity_cell(heap, scope, target);
            return Handler::WeakMap(target);
        }
        return Handler::Smi(kind_smi(KIND_SLOW, 0));
    }
    if let Some(offset) = plan.field {
        return Handler::Smi(kind_smi(KIND_FIELD, offset as i64));
    }
    if let Some(setter) = plan.native_setter {
        debug_assert!(setter.0 < (1 << 55), "native handler payload overflow");
        let accessor = plan.native_accessor.expect("native setter has an accessor");
        return guarded(
            heap,
            scope,
            &plan.receiver_map,
            kind_smi(KIND_NATIVE_SETTER, setter.0 as i64),
            &[accessor],
        );
    }
    let Some(pair) = plan.setter else {
        return Handler::Smi(kind_smi(KIND_SLOW, 0));
    };
    if plan.setter_own {
        return Handler::WeakPair(pair);
    }
    guarded(
        heap,
        scope,
        &plan.receiver_map,
        kind_smi(KIND_SETTER, 0),
        &[pair.erase()],
    )
}

/// A setter found on a prototype or owned by the receiver: run it, or treat
/// a setter-less accessor as a silently ignored write in sloppy code. Strict
/// code must throw, which is not representable as a `StoreHit`; `None`
/// defers to the slow path, where `Lookup::set` applies strict-mode rules.
fn setter_action<'s>(
    heap: &Heap,
    scope: &'s HandleScope<'_>,
    pair: Tagged<'_, AccessorPair>,
    mode: LanguageMode,
) -> Option<StoreAction<'s>> {
    let setter = pair.set.get(heap);
    if setter == heap.known().undefined.get(heap) {
        if mode.is_strict() {
            None
        } else {
            Some(StoreAction::Noop)
        }
    } else {
        Some(StoreAction::Setter(scope.handle(setter)))
    }
}

impl InlineCache {
    #[inline(always)]
    pub fn probe_mono<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
    ) -> MonoProbe<'a> {
        let Some(vector) = vector else {
            return MonoProbe::NotReceiver;
        };
        let Some(obj) = receiver.as_heap_object() else {
            return MonoProbe::NotReceiver;
        };
        probe_map(heap, vector, slot, obj.map(heap), obj)
    }

    /// Smi receiver probe for the Kette operator sites. JavaScript receivers
    /// always have a heap object, so this path stays out of `probe_mono`.
    #[inline(never)]
    pub fn probe_smi<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
    ) -> MonoProbe<'a> {
        let Some(vector) = vector else {
            return MonoProbe::NotReceiver;
        };
        let Some(origin) = receiver
            .primitive_prototype(heap)
            .and_then(|proto| proto.as_heap_object())
        else {
            return MonoProbe::NotReceiver;
        };
        probe_map(heap, vector, slot, heap.known().smi_map.get(heap), origin)
    }

    #[inline(never)]
    pub fn apply_mono<'a>(
        heap: &'a Heap,
        obj: Tagged<'a, Object>,
        handler: Tagged<'a, MaybeWeak<Value>>,
    ) -> Option<LoadHit<'a>> {
        apply_load_handler(heap, obj, handler)
    }

    #[inline(never)]
    pub fn try_load_resume<'a>(
        heap: &'a Heap,
        obj: Tagged<'a, Object>,
        map: Tagged<'a, Map>,
        pairs: Tagged<'a, WeakFixedArray>,
        start: usize,
    ) -> Option<LoadHit<'a>> {
        let len = pairs.len();
        let mut i = start;
        while i + 1 < len {
            if let Some(m) = pairs.get(heap, i).as_strong()
                && m.ptr_eq(map)
            {
                return apply_load_handler(heap, obj, pairs.get(heap, i + 1));
            }
            i += 2;
        }
        None
    }

    pub fn try_load<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
    ) -> Option<LoadHit<'a>> {
        let vector = vector?;
        let site = read_site_snapshot(vector, heap, slot);
        let obj = ic_receiver(receiver, heap)?;
        let map = obj.map(heap);
        let handler = probe(heap, site, map)?;
        apply_load_handler(heap, obj, handler)
    }

    #[inline]
    pub fn try_load_global_lexical<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
    ) -> Option<GlobalLexicalHit<'a>> {
        let (context, slot_index, _) = global_lexical_site(heap, vector, slot)?;
        let value = context
            .slots
            .get(heap)
            .as_ref()
            .element_slot(slot_index)
            .get(heap);
        Some(if value == heap.known().the_hole.get(heap).erase() {
            GlobalLexicalHit::Uninitialized
        } else {
            GlobalLexicalHit::Value(value)
        })
    }

    #[inline]
    pub fn try_store_global_lexical<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        value: Tagged<'a, Value>,
    ) -> Option<Tagged<'a, Value>> {
        let (context, slot_index, info) = global_lexical_site(heap, vector, slot)?;
        let binding = context.slots.get(heap).as_ref().element_slot(slot_index);
        if binding.get(heap) == heap.known().the_hole.get(heap).erase() || info_is_immutable(info) {
            return None;
        }
        binding.set(heap, context.erase(), value);
        Some(value)
    }

    /// Cache a script-context lexical binding on a global load/store site.
    pub fn update_global_lexical(
        heap: &Heap,
        vector: Option<Tagged<'_, FeedbackVector>>,
        slot: usize,
        context: Tagged<'_, ContextObject>,
        slot_index: usize,
        info: i64,
    ) {
        let Some(vector) = vector else { return };
        let host = vector.erase();
        let payload = (slot_index as i64 & GLOBAL_LEXICAL_SLOT_MASK)
            | ((info & 0xff) << GLOBAL_LEXICAL_INFO_SHIFT);
        let handler = kind_smi(KIND_GLOBAL_LEXICAL, payload);
        vector.write_site(heap, slot, |heap, state, handler_slot| {
            state.set_release(heap, host, context.erase());
            handler_slot.set_release(heap, host, handler.into_tagged());
        });
    }

    pub fn update_load(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Option<Handle<'_, Value>>,
        name: Handle<'_, SlotName>,
        cache_non_existent: bool,
    ) {
        let Some(vector) = vector else { return };
        let Some(receiver) = receiver else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let receiver = scope.handle(receiver.as_tagged(heap));
        let Some((identity, start)) = ic_origin(heap, receiver.as_tagged(heap)) else {
            return;
        };
        let identity = scope.handle(identity);
        let start = scope.handle(start.erase());
        let vector = scope.handle(vector.as_tagged(heap));
        let name = scope.handle(name.as_tagged(heap));
        let plan = analyze_load(heap, scope, start, receiver.as_tagged(heap), identity, name);
        if plan.non_existent() && !cache_non_existent {
            return;
        }
        let handler = plan.into_handler(heap, scope);
        update_site(heap, scope, vector, slot, &identity, &handler);
    }

    #[inline]
    pub fn try_load_element<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
        index: usize,
    ) -> Option<ElementHit<'a>> {
        if receiver
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_arguments())
        {
            return None;
        }
        let (obj, map, kind, payload) = probe_element_smi(heap, vector, slot, receiver)?;
        match kind {
            KIND_ELEMENT => apply_element_load(heap, obj, map, index, payload, false),
            KIND_INDEXED_STRING => apply_indexed_string(heap, receiver, index, payload, false),
            _ => None,
        }
    }

    /// Element load that reports a guarded miss as [`ElementHit::NotFound`].
    pub fn try_load_element_or_hole<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
        index: usize,
    ) -> Option<ElementHit<'a>> {
        if receiver
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_arguments())
        {
            return None;
        }
        let (obj, map, kind, payload) = probe_element_smi(heap, vector, slot, receiver)?;
        match kind {
            KIND_ELEMENT => apply_element_load(heap, obj, map, index, payload, true),
            KIND_INDEXED_STRING => apply_indexed_string(heap, receiver, index, payload, true),
            _ => None,
        }
    }

    #[inline]
    pub fn try_load_index_method<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
        index: usize,
    ) -> Option<Tagged<'a, Value>> {
        let obj = receiver.as_heap_object()?;
        if obj.element_value(heap, index).is_some() {
            return None;
        }
        probe_index_method(heap, vector, slot, receiver, KIND_INDEX_GET)
    }

    #[inline]
    pub fn try_store_index_method<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
        index: usize,
    ) -> Option<Tagged<'a, Value>> {
        let obj = receiver.as_heap_object()?;
        if obj.element_value(heap, index).is_some() {
            return None;
        }
        probe_index_method(heap, vector, slot, receiver, KIND_INDEX_SET)
    }

    #[inline]
    pub fn try_store_element<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'a, Value>,
        index: usize,
        value: Tagged<'a, Value>,
    ) -> Option<Tagged<'a, Value>> {
        if receiver
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_arguments())
        {
            return None;
        }
        let (obj, map, kind, payload) = probe_element_smi(heap, vector, slot, receiver)?;
        if kind != KIND_ELEMENT_STORE {
            return None;
        }
        let holey = payload & STORE_HOLEY != 0;
        let append = index == obj.length();
        if index > obj.length()
            || (append && (payload & STORE_GROW == 0 || !map.length_writable(heap)))
        {
            return None;
        }
        let elements = obj.elements_array(heap)?;
        let clean = payload & STORE_CHAIN_CLEAN != 0;
        if holey && !append {
            if index >= elements.len() {
                return None;
            }
            if !clean {
                if elements.at(heap, index) == heap.known().the_hole.get(heap) {
                    return None;
                }
            } else if !map_chain_cell_valid(heap, map) {
                return None;
            }
        } else if append {
            // appends create a property: the chain must be clean
            if !clean || !map_chain_cell_valid(heap, map) {
                return None;
            }
            if index >= elements.len() {
                // no headroom: the slow path reallocates
                return None;
            }
        }
        // packed in-bounds: an own writable element, no chain consult
        elements.set(heap, index, value);
        if append {
            obj.length
                .set(heap, obj.erase(), Smi::new((index + 1) as i64));
        }
        if map.kind().is_prototype() {
            Prototype::shape_changed(heap, map, ShapeChange::INVALIDATE_ONLY);
        }
        Some(value)
    }

    /// Re-record the keyed-load site's element handler from a slow-path
    /// observation.
    pub fn update_load_element(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Option<Handle<'_, Value>>,
        index: usize,
    ) {
        let Some(vector) = vector else { return };
        let Some(receiver) = receiver else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let recv = receiver.as_tagged(heap);
        let Some(obj) = recv.as_heap_object() else {
            return;
        };
        let map = scope.handle(obj.map(heap));
        let handler = if let Some(s) = recv.get_as::<DenseString>(heap) {
            if index < s.len() && s.code_unit(heap, index) > 0xFF {
                return;
            }
            let string_prototype = heap.known().string_prototype.get(heap);
            let proto_map = scope.handle(string_prototype.map(heap));
            let clean = chain_has_no_elements(heap, proto_map.as_tagged(heap));
            if index >= s.len() && !clean {
                return;
            }
            let mut payload = 0;
            if clean {
                proto_map.as_tagged(heap).mark_prototype(heap);
                if let Some(cell) =
                    Prototype::get_or_create_prototype_chain_validity_cell(heap, scope, proto_map)
                {
                    map.as_tagged(heap)
                        .set_validity_cell(heap, cell.as_tagged(heap));
                    payload |= ELEMENT_ALLOW_OOB;
                }
            }
            Handler::Smi(kind_smi(KIND_INDEXED_STRING, payload))
        } else if obj.is_array_exotic(heap) {
            // element handlers are dense-only: dictionary arrays take
            // the generic path
            if !map.as_tagged(heap).kind().is_dense_elements() {
                return;
            }
            let mut payload = 0;
            if map.as_tagged(heap).kind().is_holey() {
                payload |= ELEMENT_HOLEY;
            }
            // the miss index tells what the handler must handle: in-bounds
            // non-hole reads never install (the interpreter's direct read
            // covers them); hole/OOB misses add the guarded OOB bit
            let len = obj.length();
            let hole_miss = payload & ELEMENT_HOLEY != 0
                && obj.elements_array(heap).is_some_and(|elements| {
                    index < elements.len()
                        && elements.at(heap, index) == heap.known().the_hole.get(heap)
                });
            if (index >= len || hole_miss)
                && chain_has_no_elements(heap, map.as_tagged(heap))
                && attach_chain_cell(heap, scope, &map)
            {
                payload |= ELEMENT_ALLOW_OOB;
            }
            Handler::Smi(kind_smi(KIND_ELEMENT, payload))
        } else {
            return;
        };
        update_site(heap, scope, vector, slot, &map, &handler);
    }

    pub fn update_load_indexed(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Option<Handle<'_, Value>>,
        index: usize,
    ) {
        let Some(vector) = vector else { return };
        let Some(receiver) = receiver else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let recv = scope.handle(receiver.as_tagged(heap));
        let Some(obj) = recv.as_tagged(heap).as_heap_object() else {
            return;
        };
        if recv.as_tagged(heap).get_as::<DenseString>(heap).is_some() || obj.is_array_exotic(heap) {
            Self::update_load_element(heap, scope, Some(vector), slot, Some(receiver), index);
            return;
        }
        let obj = scope.handle(obj);
        let name = heap.known().strings.index_get.get_handle(heap, scope);
        let Some(handler) = index_method_handler(heap, scope, obj, name, KIND_INDEX_GET) else {
            return;
        };
        let map = scope.handle(obj.as_tagged(heap).map(heap));
        update_site(heap, scope, vector, slot, &map, &handler);
    }

    pub fn update_store_indexed(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Handle<'_, Value>,
    ) {
        let Some(vector) = vector else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let recv = scope.handle(receiver.as_tagged(heap));
        let Some(obj) = recv.as_tagged(heap).as_heap_object() else {
            return;
        };
        if obj.is_array_exotic(heap) {
            return;
        }
        let obj = scope.handle(obj);
        let name = heap.known().strings.index_set.get_handle(heap, scope);
        let Some(handler) = index_method_handler(heap, scope, obj, name, KIND_INDEX_SET) else {
            return;
        };
        let map = scope.handle(obj.as_tagged(heap).map(heap));
        update_site(heap, scope, vector, slot, &map, &handler);
    }

    pub fn update_store_element(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Option<Handle<'_, Value>>,
        grew: bool,
    ) {
        let Some(vector) = vector else { return };
        let Some(receiver) = receiver else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let recv = receiver.as_tagged(heap);
        let Some(obj) = recv.as_heap_object() else {
            return;
        };
        if !obj.is_array_exotic(heap) {
            return;
        }
        let map = scope.handle(obj.map(heap));
        // element store handlers are dense-only
        if !map.as_tagged(heap).kind().is_dense_elements() {
            return;
        }
        let mut payload = 0;
        if map.as_tagged(heap).kind().is_holey() {
            payload |= STORE_HOLEY;
        }
        if chain_has_no_elements(heap, map.as_tagged(heap)) && attach_chain_cell(heap, scope, &map)
        {
            payload |= STORE_CHAIN_CLEAN;
            if grew {
                payload |= STORE_GROW;
            }
        }
        let handler = Handler::Smi(kind_smi(KIND_ELEMENT_STORE, payload));
        update_site(heap, scope, vector, slot, &map, &handler);
    }

    /// The store IC's allocation-free fast path: probe the site and write
    /// a pre-reserved field slot (or swap to a cached transition target
    /// that already has room). It never allocates, fills the IC, or grows
    /// the receiver, so it takes `&Heap` like any other no-safepoint read:
    /// callers may hold heap-anchored `Tagged`s across it. Misses, setters
    /// and growing transitions fall through to the `&mut Heap`
    /// handle-rooted `try_store`/`update_store` path.
    #[inline]
    pub fn try_store_fast(
        heap: &Heap,
        vector: Option<Tagged<'_, FeedbackVector>>,
        slot: usize,
        receiver: Tagged<'_, Value>,
        name: Tagged<'_, SlotName>,
        value: Tagged<'_, Value>,
    ) -> bool {
        let Some(vector) = vector else {
            return false;
        };
        let site = read_site_snapshot(vector, heap, slot);
        let Some(recv) = ic_receiver(receiver, heap) else {
            return false;
        };
        let map = recv.map(heap);
        let Some(handler) = probe(heap, site, map) else {
            return false;
        };
        if !handler.is_ptr() {
            let Some((KIND_FIELD, payload)) = decode_handler_smi(handler) else {
                return false;
            };
            recv.slot(heap, payload as usize)
                .set(heap, recv.erase(), value);
            return true;
        }
        if handler.is_weak_ptr() {
            // Cached own-property add (StoreHandler::StoreTransition):
            // swap to the target map and write the pre-reserved slot. A
            // missing reservation falls back to the cold grow path.
            let Some(strong) = handler.as_strong() else {
                return false;
            };
            let Some(target) = strong.get_as::<Map>(heap) else {
                return false;
            };
            if !target.is_prototype_validity_cell_valid(heap) {
                return false;
            }
            let Some(row) = target
                .descriptors()
                .iter()
                .find(|d| d.name(heap).ptr_eq(name.erase()) && !d.flags().is_accessor())
            else {
                return false;
            };
            let offset = row.offset();
            let old_len = recv.slots(heap).as_slice().len();
            if offset >= old_len {
                return false;
            }
            let host = recv.erase();
            recv.slot(heap, offset).set(heap, host, value);
            Prototype::shape_changed(heap, recv.map(heap), ShapeChange::INVALIDATE_ONLY);
            recv.header.set_map(heap, host, target);
            return true;
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_store<'a>(
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Handle<'_, Value>,
        name: Handle<'_, SlotName>,
        value: Handle<'_, Value>,
        mode: LanguageMode,
    ) -> Option<StoreHit<'a>> {
        let vector = vector?;
        let site = read_site_snapshot(vector.as_tagged(heap), heap, slot);
        let recv = ic_receiver(receiver.as_tagged(heap).erase(), heap)?;
        let map = scope.handle(recv.map(heap));
        let handler = probe(heap, site, map.as_tagged(heap))?;
        let action = if !handler.is_ptr() {
            let (kind, payload) = decode_handler_smi(handler)?;
            match kind {
                KIND_FIELD => StoreAction::Field(payload as usize),
                _ => return None,
            }
        } else if handler.is_weak_ptr() {
            let strong = handler.as_strong()?;
            if let Some(target) = strong.get_as::<Map>(heap) {
                if !target.is_prototype_validity_cell_valid(heap) {
                    return None;
                }
                StoreAction::Transition(scope.handle(target))
            } else {
                // own setter as a whole weak pair
                setter_action(heap, scope, strong.get_as::<AccessorPair>(heap)?, mode)?
            }
        } else {
            let data_handler = handler.as_strong()?.get_as::<DataHandler>(heap)?;
            if !check_prototype_validity_cell(heap, data_handler) {
                return None;
            }
            let (kind, payload) = decode_handler_smi(data_handler.smi_handler(heap))?;
            match kind {
                KIND_SETTER => {
                    let pair = data_handler
                        .data(heap, 0)
                        .as_strong()?
                        .get_as::<AccessorPair>(heap)?;
                    setter_action(heap, scope, pair, mode)?
                }
                KIND_NATIVE_SETTER => {
                    let accessor = data_handler.data(heap, 0).as_strong()?;
                    StoreAction::NativeSetter {
                        index: NativeIndex(payload as usize),
                        accessor: scope.handle(accessor),
                    }
                }
                _ => return None,
            }
        };
        match action {
            StoreAction::Field(offset) => {
                let recv = receiver.as_tagged(heap).as_heap_object()?;
                let host = recv.erase();
                recv.slot(heap, offset)
                    .set(heap, host, value.as_tagged(heap));
                Some(StoreHit::Done)
            }
            StoreAction::Transition(target) => {
                apply_transition(heap, scope, receiver, name, value, &target)
                    .then_some(StoreHit::Done)
            }
            StoreAction::Setter(setter) => Some(StoreHit::Setter(setter.as_tagged(heap))),
            StoreAction::NativeSetter { index, accessor } => Some(StoreHit::NativeSetter {
                index,
                accessor: accessor.as_tagged(heap),
            }),
            StoreAction::Noop => Some(StoreHit::Done),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_store(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Handle<'_, Value>,
        name: Handle<'_, SlotName>,
        prev_map: Handle<'_, Map>,
        outcome: StoreOutcomeKind,
    ) {
        let Some(vector) = vector else { return };
        debug_assert!(slot + 1 < vector.as_tagged(heap).len());
        let Some(recv) = ic_receiver(receiver.as_tagged(heap).erase(), heap) else {
            return;
        };
        let target =
            (outcome == StoreOutcomeKind::Transition).then(|| scope.handle(recv.map(heap)));
        let plan = analyze_store(heap, scope, recv, name.as_tagged(heap));
        let handler = store_handler(heap, scope, target, plan);
        update_site(heap, scope, vector, slot, &prev_map, &handler);
    }

    /// Fast path for a define-own data property. A field handler writes
    /// the cached slot; a transition handler migrates the receiver to the
    /// map a previous define produced (growing slots when necessary).
    pub fn try_define_own(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Handle<'_, Value>,
        name: Handle<'_, SlotName>,
        value: Handle<'_, Value>,
    ) -> bool {
        let Some(vector) = vector else {
            return false;
        };
        let Some(recv) = ic_receiver(receiver.as_tagged(heap).erase(), heap) else {
            return false;
        };
        let site = read_site_snapshot(vector.as_tagged(heap), heap, slot);
        let handler = probe(heap, site, recv.map(heap));
        let Some(handler) = handler else {
            return false;
        };
        if !handler.is_ptr() {
            let Some((KIND_FIELD, payload)) = decode_handler_smi(handler) else {
                return false;
            };
            let host = recv.erase();
            recv.slot(heap, payload as usize)
                .set(heap, host, value.as_tagged(heap));
            return true;
        }
        if handler.is_weak_ptr() {
            let Some(target) = handler.as_strong().and_then(|m| m.get_as::<Map>(heap)) else {
                return false;
            };
            let target = scope.handle(target);
            return apply_transition(heap, scope, receiver, name, value, &target);
        }
        false
    }

    /// Record the map transition (or unchanged map + field offset) a
    /// successful define produced.
    pub fn update_define_own(
        heap: &mut Heap,
        scope: &HandleScope<'_>,
        vector: Option<Handle<'_, FeedbackVector>>,
        slot: usize,
        receiver: Handle<'_, Value>,
        name: Handle<'_, SlotName>,
        prev_map: Handle<'_, Map>,
    ) {
        let Some(vector) = vector else {
            return;
        };
        let Some(recv) = ic_receiver(receiver.as_tagged(heap).erase(), heap) else {
            return;
        };
        let final_map = scope.handle(recv.map(heap));
        let final_map_ref = final_map.as_tagged(heap);
        let Some(row) = final_map_ref
            .descriptors()
            .iter()
            .find(|d| d.name(heap).ptr_eq(name.as_tagged(heap)) && !d.flags().is_accessor())
        else {
            return;
        };
        // A non-writable, non-configurable slot cannot take a different
        // value on a later receiver; leave that case on the slow path.
        if !row.flags().is_writable() && !row.flags().is_configurable() {
            return;
        }
        let handler = if final_map.as_tagged(heap).ptr_eq(prev_map.as_tagged(heap)) {
            Handler::Smi(kind_smi(KIND_FIELD, row.offset() as i64))
        } else {
            Handler::WeakMap(final_map)
        };
        update_site(heap, scope, vector, slot, &prev_map, &handler);
    }
}

/// Migrate `receiver` to the cached transition target: verify the target
/// row, grow slots when appending, store, swap the map.
fn apply_transition(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    name: Handle<'_, SlotName>,
    value: Handle<'_, Value>,
    target: &Handle<'_, Map>,
) -> bool {
    let recv = receiver
        .as_tagged(heap)
        .as_heap_object()
        .expect("gated receiver");
    let target_ref = target.as_tagged(heap);
    let Some(row) = target_ref
        .descriptors()
        .iter()
        .find(|d| d.name(heap).ptr_eq(name.as_tagged(heap)))
    else {
        return false;
    };
    if row.flags().is_accessor() {
        return false;
    }
    let offset = row.offset();
    let old_len = recv.slots(heap).as_slice().len();
    let new_len = target_ref.value_slot_count();

    if offset < old_len {
        // The receiver's array already reserves the slot: an own-field
        // write or an append inside the slack capacity.
        let host = recv.erase();
        recv.slot(heap, offset)
            .set(heap, host, value.as_tagged(heap));
        Prototype::shape_changed(heap, recv.map(heap), ShapeChange::INVALIDATE_ONLY);
        recv.header.set_map(heap, host, target.as_tagged(heap));
        return true;
    }
    if new_len != old_len + 1 || offset != old_len {
        return false;
    }

    // Reservation exhausted: grow with fresh headroom so the next appends
    // land in place.
    let capacity = new_len + Map::SLACK_MARGIN;
    let slots = heap.allocate_hole_array(capacity).as_handle(scope);
    {
        let recv = receiver
            .as_tagged(heap)
            .as_heap_object()
            .expect("gated receiver");
        let old = recv.slots(heap);
        let new = slots.as_tagged(heap);
        for k in 0..old_len {
            new.set(heap, k, old.at(heap, k));
        }
        new.set(heap, offset, value.as_tagged(heap));
        let host = recv.erase();
        recv.slots.set(heap, host, new);
        Prototype::shape_changed(heap, recv.map(heap), ShapeChange::INVALIDATE_ONLY);
        recv.header.set_map(heap, host, target.as_tagged(heap));
    }
    true
}

fn update_site(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    vector: Handle<'_, FeedbackVector>,
    slot: usize,
    map: &Handle<'_, Map>,
    handler: &Handler<'_>,
) {
    let vec_t = vector.as_tagged(heap);
    let site = read_site_snapshot(vec_t, heap, slot);
    let state = site.state;

    if !state.is_ptr() {
        vec_t.set_mono(heap, slot, map.as_tagged(heap), handler.word(heap));
        return;
    }
    if state.is_weak_ptr() {
        if let Some(current) = state.as_strong() {
            if current.ptr_eq(map.as_tagged(heap)) {
                // Same map but the handler missed: a chain changed behind
                // an identical receiver map, or the payload died. A Slow
                // handler is final.
                if decode_handler_smi(site.handler).is_some_and(|(k, _)| k == KIND_SLOW) {
                    return;
                }
                if vec_t.set_handler_if_mono(heap, slot, map.as_tagged(heap), handler.word(heap)) {
                    return;
                }
                // TODO: rebuild the poly array with this handler when the
                // state raced with another writer.
                return;
            }
            promote_from_mono(heap, scope, vector, slot, map, handler);
            return;
        }
    } else if let Some(strong) = state.as_strong() {
        if strong.ptr_eq(heap.known().megamorphic_symbol.get(heap).erase()) {
            return;
        }
        if let Some(pairs) = strong.get_as::<WeakFixedArray>(heap) {
            let pairs = scope.handle(pairs);
            update_poly(heap, scope, vector, slot, pairs, map, handler);
            return;
        }
    }
    vec_t.set_mono(heap, slot, map.as_tagged(heap), handler.word(heap));
}

/// Replace the monomorphic entry with a pair array of two entries.
fn promote_from_mono(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    vector: Handle<'_, FeedbackVector>,
    slot: usize,
    map: &Handle<'_, Map>,
    handler: &Handler<'_>,
) {
    let total = WeakFixedArray::<Value>::layout_for(4);
    let arr = heap.allocate_token_enter_heap(total, |token, heap| {
        let vec_t = vector.as_tagged(heap);
        // the reservation may have collected: the cached map is weak, so
        // re-read the entry instead of trusting the pre-allocation check
        let site = read_site_snapshot(vec_t, heap, slot);
        if !site.state.is_weak_ptr() {
            token.discard_remaining();
            return None;
        }
        let Some(old_map) = site.state.as_strong() else {
            token.discard_remaining();
            return None;
        };
        let words = [
            old_map.as_weak(),
            site.handler,
            map.as_tagged(heap).erase().as_weak(),
            handler.word(heap),
        ];
        let arr = token.allocate::<WeakFixedArray>(WeakFixedArrayInit { values: &words });
        token.discard_remaining();
        Some(scope.handle(arr))
    });
    match arr {
        Some(arr) => vector
            .as_tagged(heap)
            .set_poly(heap, slot, arr.as_tagged(heap)),
        // the stale entry died or raced with another writer: install the new one
        None => {
            vector
                .as_tagged(heap)
                .set_mono(heap, slot, map.as_tagged(heap), handler.word(heap))
        }
    }
}

fn update_poly(
    heap: &mut Heap,
    scope: &HandleScope<'_>,
    vector: Handle<'_, FeedbackVector>,
    slot: usize,
    pairs: Handle<'_, WeakFixedArray>,
    map: &Handle<'_, Map>,
    handler: &Handler<'_>,
) {
    let pairs_t = pairs.as_tagged(heap);
    let len = pairs_t.len();
    let mut found = false;
    let mut live = 0usize;
    let mut i = 0;
    while i + 1 < len {
        if let Some(m) = pairs_t.get(heap, i).as_strong() {
            if m.ptr_eq(map.as_tagged(heap)) {
                found = true;
            }
            live += 1;
        }
        i += 2;
    }
    if !found && live >= MAX_POLYMORPHIC_ENTRIES {
        vector.as_tagged(heap).set_megamorphic(heap, slot);
        return;
    }
    // Always republish a fresh array: published feedback arrays are immutable.
    // The extra pair leaves room if the found map cannot be copied and must be
    // appended instead; `discard_remaining` trims the reservation.
    let new_len = 2 * (live + 1);
    let total = WeakFixedArray::<Value>::layout_for(new_len);
    let arr = heap.allocate_token_enter_heap(total, |token, heap| {
        let pairs_t = pairs.as_tagged(heap);
        let len = pairs_t.len();
        let mut words: Vec<Tagged<'_, MaybeWeak<Value>>> = Vec::with_capacity(new_len);
        let mut replaced = false;
        let mut i = 0;
        while i + 1 < len {
            if let Some(m) = pairs_t.get(heap, i).as_strong() {
                words.push(m.as_weak());
                if !replaced && m.ptr_eq(map.as_tagged(heap)) {
                    words.push(handler.word(heap));
                    replaced = true;
                } else {
                    words.push(pairs_t.get(heap, i + 1));
                }
            }
            i += 2;
        }
        if !replaced {
            words.push(map.as_tagged(heap).erase().as_weak());
            words.push(handler.word(heap));
        }
        let arr = token.allocate::<WeakFixedArray>(WeakFixedArrayInit { values: &words });
        // the reservation was sized from the pre-collection live count: maps
        // whose only reference was this weak cache entry may have died here
        token.discard_remaining();
        scope.handle(arr)
    });
    vector
        .as_tagged(heap)
        .set_poly(heap, slot, arr.as_tagged(heap));
}

pub const CALL_TAG_NATIVE: i64 = 1;

#[inline]
fn encode_runtime_payload(index: NativeIndex) -> i64 {
    CALL_TAG_NATIVE | ((index.0 as i64) << 1)
}

#[inline]
fn decode_runtime_payload(raw: i64) -> Option<NativeIndex> {
    (raw & CALL_TAG_NATIVE != 0).then_some(NativeIndex((raw >> 1) as usize))
}

#[inline]
fn call_probe_strong<'a>(heap: &'a Heap, callee: Tagged<'a, Value>) -> CallProbe<'a> {
    match callee.call_target(heap) {
        Some(CallTarget::Native(rt)) => CallProbe::Native(rt),
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
        }) => CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }),
        _ => CallProbe::Miss,
    }
}

/// Decode the packed callable descriptor: `register_count | formal_min<<16
/// | kind<<32` (mirrors `Object::call_target`).
#[inline(always)]
fn decode_descriptor(desc: i64) -> (usize, usize, FunctionKind) {
    let desc = desc as u64;
    (
        (desc & 0xffff) as usize,
        ((desc >> 16) & 0xffff) as usize,
        FunctionKind::decode(((desc >> 32) & 0xf) as i64),
    )
}

impl InlineCache {
    /// Probe a call site: a weak-callee match returns the pre-decoded hit
    /// (or the packed runtime callee); anything else is a miss for the
    /// generic call path to handle and record.
    #[inline(always)]
    pub fn call_probe<'a>(
        heap: &'a Heap,
        vector: Option<Tagged<'a, FeedbackVector>>,
        fb: usize,
        callee: Tagged<'a, Value>,
    ) -> CallProbe<'a> {
        let Some(vector) = vector else {
            return CallProbe::Miss;
        };
        let site = read_site_snapshot(vector, heap, fb);
        let state = site.state;
        if !state.is_weak_ptr() {
            if state.is_strong_ptr() && !state.ptr_eq(heap.known().the_hole.get(heap).erase()) {
                // megamorphic site: decode the callee on every execution
                return call_probe_strong(heap, callee);
            }
            return CallProbe::Miss;
        }
        if !state.ptr_eq(callee) {
            return CallProbe::Miss;
        }
        if !callee.is_callable(heap) {
            return CallProbe::Miss;
        }
        let payload = site.handler;
        if let Some(tag) = payload.to_i64() {
            // runtime callee: index and shape come straight from the
            // payload - no object decode needed
            return match decode_runtime_payload(tag) {
                Some(rt) => CallProbe::Native(rt),
                None => CallProbe::Miss,
            };
        }

        let Some(info_word) = payload.as_strong() else {
            return CallProbe::Miss;
        };
        let info: Tagged<'a, CallableInfoObject> = unsafe { info_word.cast() };
        debug_assert!(info_word.get_as::<CallableInfoObject>(heap).is_some());
        let (_, _, kind) = decode_descriptor(info.descriptor.to_smi_unchecked().value());
        let Some(target) = callee.as_heap_object() else {
            return CallProbe::Miss;
        };
        let Some(target_info) = target.callable_info(heap) else {
            return CallProbe::Miss;
        };
        if !target_info.ptr_eq(info) {
            return CallProbe::Miss;
        }
        let context_word = target.slot(heap, 1).get(heap);
        debug_assert!(context_word.get_as::<ContextObject>(heap).is_some());
        let context: Tagged<'a, ContextObject> = unsafe { context_word.cast() };
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        })
    }
}

impl InlineCache {
    /// Record a call site's callee after the generic path resolved it:
    /// re-arm a cleared weak entry, flip the site megamorphic on an
    /// unrelated second callee, and keep it monomorphic across closures
    /// sharing the same `CallableInfoObject`.
    pub fn call_update(
        heap: &Heap,
        vector: Option<Tagged<FeedbackVector>>,
        fb: usize,
        callee: Tagged<'_, Value>,
        record: CallRecord<'_>,
    ) {
        let Some(vector) = vector else {
            return;
        };
        let host = vector.erase();
        vector.write_site(heap, fb, |heap, state_slot, tag_slot| {
            if state_slot.is_cleared() {
                // a collected weak entry leaves the site free to become
                // monomorphic again
            } else {
                let state = state_slot.get(heap);
                if state.is_strong_ptr() {
                    let hole = heap.known().the_hole.get(heap).erase();
                    if !state.ptr_eq(hole) {
                        // megamorphic: leave it alone
                        return;
                    }
                } else if !state.ptr_eq(callee) {
                    let same_code =
                        match (&record, state.as_strong().and_then(|c| c.as_heap_object())) {
                            (CallRecord::Bytecode(info), Some(old)) => old
                                .callable_info(heap)
                                .is_some_and(|old_info| old_info.ptr_eq(*info)),
                            _ => false,
                        };
                    if !same_code {
                        state_slot.set_release(
                            heap,
                            host,
                            heap.known().megamorphic_symbol.get(heap).erase(),
                        );
                        return;
                    }
                }
            }
            state_slot.set_release(heap, host, callee.as_weak());
            match record {
                // bytecode: the payload IS the resolved info
                CallRecord::Bytecode(info) => tag_slot.set_release(heap, host, info.erase()),
                // runtime: index and argument shape packed into the payload
                CallRecord::Native(rt) => tag_slot.set_release(
                    heap,
                    host,
                    Smi::new(encode_runtime_payload(rt))
                        .into_tagged()
                        .as_maybe_weak(),
                ),
            }
        });
    }
}
