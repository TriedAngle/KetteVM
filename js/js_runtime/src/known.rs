//! The well-known roots that only the JS runtime uses.
//!
//! The core [`WellKnown`](vm_core::WellKnown) keeps the heap schema every
//! front end shares (maps, sentinels, the object prototype); everything here
//! is reachable only from JS builtins, so it lives in the JS runtime's state
//! and is rooted through its `EdgeVisitable` impl.

use vm_core::{
    DenseString, EdgeVisitable, GcSlot, HandleScope, HandleSlice, Heap, Map, MapInit, MapKind,
    Object, SlotName, StringInterner, Symbol, Tagged, Visitor, smi_slot,
};

pub struct JsKnown {
    pub strings: JsStrings,
    /// The intrinsic `eval` function, recognized by direct-eval calls.
    pub eval_function: GcSlot<vm_core::Object>,
    /// `%Object.prototype.toString%`, kept even if the property is replaced.
    pub object_to_string: GcSlot<vm_core::Object>,
    /// `@@iterator`
    pub iterator_symbol: GcSlot<Symbol>,
    /// `@@toStringTag`
    pub to_string_tag_symbol: GcSlot<Symbol>,
    /// `%Date.prototype%`
    pub date_prototype: GcSlot<Object>,
    /// Map of Date instances: one value slot holding epoch milliseconds.
    pub date_instance_map: GcSlot<Map>,
    /// Map of array-iterator objects (slots: [iterated array, next index])
    pub array_iterator_map: GcSlot<Map>,
    /// `%ArrayIteratorPrototype%` (holds `next`)
    pub array_iterator_prototype: GcSlot<Object>,
    /// Map of iterator-result objects `{ value, done }` (w+, e+, c+)
    pub iterator_result_map: GcSlot<Map>,
    /// Hidden for-in enumerator: slots [level, keys, index, visited]
    /// (ES 14.7.5.9 EnumerateObjectProperties; unreachable from JS)
    pub for_in_enumerator_map: GcSlot<Map>,
}

impl JsKnown {
    /// Create the values that are not interned names (called once, during
    /// `JSRuntime::setup`; the slots are already reachable through the
    /// runtime state).
    pub fn init(&self, heap: &mut Heap, scope: &HandleScope<'_>) {
        for (slot, description) in [
            (&self.iterator_symbol, "Symbol.iterator"),
            (&self.to_string_tag_symbol, "Symbol.toStringTag"),
        ] {
            let description = DenseString::from_utf8(heap, scope, description);
            let symbol = Symbol::new(heap, scope, Some(description));
            slot.init_tagged(symbol.as_tagged(heap));
        }
        // %Date.prototype% and the Date instance map (JS-only objects)
        let object_prototype = scope.handle(heap.known().object_prototype.get(heap).erase());
        let date_prototype_map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype,
            },
            scope,
        );
        let date_prototype =
            scope.handle(heap.new_object(scope, date_prototype_map, HandleSlice::EMPTY));
        self.date_prototype.init(date_prototype.as_tagged(heap));
        let date_prototype_value = scope.handle(date_prototype.as_tagged(heap).erase());
        let date_instance_map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT
                    .union(MapKind::EXTENDABLE)
                    .union(MapKind::DATE),
                value_slot_count: 1,
                descriptors: &[],
                prototype: date_prototype_value,
            },
            scope,
        );
        self.date_instance_map
            .init(date_instance_map.as_tagged(heap));
    }
}

// The slots are only mutated during the runtime's single-threaded setup;
// the collector visits them from the stop-the-world host root callback,
// exactly like the core `WellKnown`.
unsafe impl Send for JsKnown {}
unsafe impl Sync for JsKnown {}

impl Default for JsKnown {
    fn default() -> Self {
        Self {
            strings: JsStrings::default(),
            eval_function: smi_slot(),
            object_to_string: smi_slot(),
            iterator_symbol: smi_slot(),
            to_string_tag_symbol: smi_slot(),
            date_prototype: smi_slot(),
            date_instance_map: smi_slot(),
            array_iterator_map: smi_slot(),
            array_iterator_prototype: smi_slot(),
            iterator_result_map: smi_slot(),
            for_in_enumerator_map: smi_slot(),
        }
    }
}

impl EdgeVisitable for JsKnown {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.strings.visit_edges(visitor);
        visitor.visit(self.eval_function.as_raw());
        visitor.visit(self.object_to_string.as_raw());
        visitor.visit(self.iterator_symbol.as_raw());
        visitor.visit(self.to_string_tag_symbol.as_raw());
        visitor.visit(self.date_prototype.as_raw());
        visitor.visit(self.date_instance_map.as_raw());
        visitor.visit(self.array_iterator_map.as_raw());
        visitor.visit(self.array_iterator_prototype.as_raw());
        visitor.visit(self.iterator_result_map.as_raw());
        visitor.visit(self.for_in_enumerator_map.as_raw());
    }
}

/// Names only JS builtins look up.
pub struct JsStrings {
    /// `done`: iterator result objects (ES 7.4.7)
    pub done: GcSlot<SlotName>,
    /// `join`: `Array.prototype.join` and `toString`
    pub join: GcSlot<SlotName>,
    /// `toISOString`: `Date.prototype.toJSON`
    pub to_iso_string: GcSlot<SlotName>,
    /// constructor names looked up by the for-in primitive walk
    pub number_ctor: GcSlot<SlotName>,
    pub boolean_ctor: GcSlot<SlotName>,
    pub symbol_ctor: GcSlot<SlotName>,
    /// `Proxy.revocable` surface
    pub proxy: GcSlot<SlotName>,
    pub revocable: GcSlot<SlotName>,
    pub revoke: GcSlot<SlotName>,
}

impl JsStrings {
    const NAMES: [(&'static str, fn(&Self) -> &GcSlot<SlotName>); 9] = [
        ("done", |s| &s.done),
        ("join", |s| &s.join),
        ("toISOString", |s| &s.to_iso_string),
        ("Number", |s| &s.number_ctor),
        ("Boolean", |s| &s.boolean_ctor),
        ("Symbol", |s| &s.symbol_ctor),
        ("proxy", |s| &s.proxy),
        ("revocable", |s| &s.revocable),
        ("revoke", |s| &s.revoke),
    ];

    /// Intern every name straight into its root slot: each interning call may
    /// collect, and a partially built local table would lose the names
    /// interned so far.
    pub fn intern_all(&self, heap: &mut Heap, interner: &StringInterner, scope: &HandleScope<'_>) {
        for (text, slot) in Self::NAMES {
            let interned = interner.intern_str(heap, scope, text);
            let value: Tagged<'_, SlotName> = interned.as_tagged(heap).into();
            slot(self).init_tagged(value);
        }
    }
}

impl Default for JsStrings {
    fn default() -> Self {
        Self {
            done: smi_slot(),
            join: smi_slot(),
            to_iso_string: smi_slot(),
            number_ctor: smi_slot(),
            boolean_ctor: smi_slot(),
            symbol_ctor: smi_slot(),
            proxy: smi_slot(),
            revocable: smi_slot(),
            revoke: smi_slot(),
        }
    }
}

impl EdgeVisitable for JsStrings {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.done.as_raw());
        visitor.visit(self.join.as_raw());
        visitor.visit(self.to_iso_string.as_raw());
        visitor.visit(self.number_ctor.as_raw());
        visitor.visit(self.boolean_ctor.as_raw());
        visitor.visit(self.symbol_ctor.as_raw());
        visitor.visit(self.proxy.as_raw());
        visitor.visit(self.revocable.as_raw());
        visitor.visit(self.revoke.as_raw());
    }
}
