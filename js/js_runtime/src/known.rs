//! The well-known roots that only the JS runtime uses.
//!
//! The core [`WellKnown`](vm_core::WellKnown) keeps the heap schema every
//! front end shares (maps, sentinels, the object prototype); everything here
//! is reachable only from JS builtins, so it lives in the JS runtime's state
//! and is rooted through its `EdgeVisitable` impl.

use vm_core::{
    EdgeVisitable, GcSlot, HandleScope, Heap, SlotName, StringInterner, Tagged, Visitor, smi_slot,
};

#[derive(Default)]
pub struct JsKnown {
    pub strings: JsStrings,
}

// The slots are only mutated during the runtime's single-threaded setup;
// the collector visits them from the stop-the-world host root callback,
// exactly like the core `WellKnown`.
unsafe impl Send for JsKnown {}
unsafe impl Sync for JsKnown {}

impl EdgeVisitable for JsKnown {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.strings.visit_edges(visitor);
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
}

impl JsStrings {
    const NAMES: [(&'static str, fn(&Self) -> &GcSlot<SlotName>); 3] = [
        ("done", |s| &s.done),
        ("join", |s| &s.join),
        ("toISOString", |s| &s.to_iso_string),
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
        }
    }
}

impl EdgeVisitable for JsStrings {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.done.as_raw());
        visitor.visit(self.join.as_raw());
        visitor.visit(self.to_iso_string.as_raw());
    }
}
