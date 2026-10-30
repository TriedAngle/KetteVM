use core::ptr::NonNull;

use crate::{
    AccessorInfo, AccessorInfoInit, CallableInfoInit, CallableInfoObject, Cell, CellInit, Code,
    CodeInit, ContextInit, ContextObject, EdgeVisitable, FixedArray, GcSlot, Handle, HandleData,
    HandleScope, HandleSlice, Heap, MODE_SLOPPY, Map, MapInit, MapKind, NameToIndexHashTable,
    NameToIndexHashTableInit, NativeIndex, Object, ObjectInit, ScopeInfo, ScopeInfoInit,
    ScriptContextTable, ScriptContextTableInit, SlotFlags, SlotName, Smi, StringInterner, Symbol,
    Tagged, Value, Visitor,
};

pub struct WellKnown {
    // vm internal machinery: sentinels, canonical empties, builtin maps
    pub map_map: GcSlot<Map>,
    pub the_hole: GcSlot<Object>,
    /// Never user-visible: a call returning it signals a pending exception.
    pub exception: GcSlot<Object>,
    /// exception map
    pub exception_map: GcSlot<Map>,
    /// Shared empty backing for objects without data slots (never written in
    /// place; the first property store swaps in a fresh array).
    pub empty_fixed_array: GcSlot<FixedArray>,
    /// TODO: Placeholder for now
    pub empty_context: GcSlot<ContextObject>,
    pub smi_map: GcSlot<Map>,
    pub float_map: GcSlot<Map>,
    pub array_map: GcSlot<Map>,
    pub byte_array_map: GcSlot<Map>,
    pub code_map: GcSlot<Map>,
    pub dense_latin1_string_map: GcSlot<Map>,
    pub dense_utf16_string_map: GcSlot<Map>,
    pub symbol_map: GcSlot<Map>,
    pub accessor_pair_map: GcSlot<Map>,
    pub accessor_info_map: GcSlot<Map>,
    pub callable_map: GcSlot<Map>,
    pub handler_table_map: GcSlot<Map>,
    pub context_map: GcSlot<Map>,
    pub scope_info_map: GcSlot<Map>,
    pub script_context_table_map: GcSlot<Map>,
    pub name_index_table_map: GcSlot<Map>,
    pub feedback_vector_map: GcSlot<Map>,
    /// Maps of the prototype-validity machinery: `Cell` and `PrototypeInfo`.
    pub cell_map: GcSlot<Map>,
    pub prototype_info_map: GcSlot<Map>,
    pub handler_map_0: GcSlot<Map>,
    pub handler_map_1: GcSlot<Map>,
    pub handler_map_2: GcSlot<Map>,
    pub handler_map_3: GcSlot<Map>,
    pub invalid_prototype_validity_cell: GcSlot<Cell>,
    /// Shared immutable scope description for contexts without named slots
    /// (block/catch contexts, the empty context)
    pub empty_scope_info: GcSlot<ScopeInfo>,
    // js userspace primitives and object base maps
    pub undefined: GcSlot<Object>,
    pub undefined_map: GcSlot<Map>,
    pub null: GcSlot<Object>,
    pub null_map: GcSlot<Map>,
    pub false_object: GcSlot<Object>,
    pub true_object: GcSlot<Object>,
    pub boolean_map: GcSlot<Map>,
    /// Base map for ECMAScript array objects (elements + length, prototype
    /// %Array.prototype%).
    pub js_array_map: GcSlot<Map>,
    /// Base map for ECMAScript error objects
    pub error_map: GcSlot<Map>,
    /// Per-class error maps (prototype chain carries `.constructor`);
    /// installed by the builtins bootstrap.
    pub type_error_map: GcSlot<Map>,
    pub reference_error_map: GcSlot<Map>,
    pub range_error_map: GcSlot<Map>,
    pub syntax_error_map: GcSlot<Map>,
    /// Wrapper maps for boxed primitives (slots[0] = the primitive value);
    pub number_wrapper_map: GcSlot<Map>,
    pub boolean_wrapper_map: GcSlot<Map>,
    pub string_wrapper_map: GcSlot<Map>,
    pub symbol_wrapper_map: GcSlot<Map>,
    pub number_prototype: GcSlot<Object>,
    pub boolean_prototype: GcSlot<Object>,
    pub string_prototype: GcSlot<Object>,
    pub symbol_prototype: GcSlot<Object>,
    // prototypes
    /// `%Object.prototype%`: root of the ordinary-object prototype hierarchy.
    pub object_prototype: GcSlot<Object>,
    /// `%Array.prototype%`: an array object, parent of all array instance maps.
    pub array_prototype: GcSlot<Object>,
    /// Map of Date instances: ordinary extendable objects with one value
    /// slot holding the epoch-milliseconds Float (prototype %Date.prototype%).
    pub date_instance_map: GcSlot<Map>,
    pub date_prototype: GcSlot<Object>,
    /// `%Error.prototype%`: parent of all error instance maps.
    pub error_prototype: GcSlot<Object>,
    /// `%Function.prototype%` (ES 19.2.3): the canonical empty function — a
    /// callable, non-constructable function whose `[[Prototype]]` is
    /// `%Object.prototype%`; the `[[Prototype]]` of ordinary function objects.
    pub function_prototype: GcSlot<Object>,
    /// The realm global object: global variables are properties on it
    /// (top-level `var`/assignments; lexical script-context globals later).
    pub global_object: GcSlot<Object>,
    /// The intrinsic `eval` function used to recognize direct eval calls.
    pub eval_function: GcSlot<Object>,
    /// Loaded script contexts and their name index.
    pub script_context_table: GcSlot<ScriptContextTable>,
    /// Initial map of `%Object.prototype%`: every fresh `{}` gets it
    /// (extendable, parent = object_prototype). Shared with `global_object`.
    pub object_initial_map: GcSlot<Map>,
    /// Map of Self-style (Kette) objects: extensible, no [[Prototype]]
    /// inheritance — the prototype link is an empty parent pair list.
    /// Function object map
    /// slots[0] = shared CallableInfoObject, slots[1] = closure context.
    pub function_map: GcSlot<Map>,
    /// Callable function objects without [[Construct]] (arrows, methods,
    /// getters and setters).
    pub non_constructor_function_map: GcSlot<Map>,
    /// Constructible class functions whose ordinary [[Call]] path throws.
    pub class_constructor_map: GcSlot<Map>,
    /// The `@@toPrimitive` well-known symbol
    pub to_primitive_symbol: GcSlot<Symbol>,
    /// The `@@iterator` well-known symbol (internal: no Symbol global yet)
    pub iterator_symbol: GcSlot<Symbol>,
    /// The `@@toStringTag` well-known symbol
    pub to_string_tag_symbol: GcSlot<Symbol>,
    /// Inline-cache state sentinel (never user-visible): a feedback slot
    /// holding it permanently takes the slow path
    pub megamorphic_symbol: GcSlot<Symbol>,
    /// Transition-tree key for prototype changes (never user-visible):
    /// a `set_prototype` edge, disambiguated by the child's prototype
    pub prototype_transition_symbol: GcSlot<Symbol>,
    pub holey_transition_symbol: GcSlot<Symbol>,
    pub dictionary_transition_symbol: GcSlot<Symbol>,
    pub number_dictionary_map: GcSlot<Map>,
    /// Map of array-iterator objects (slots: [iterated array, next index])
    pub array_iterator_map: GcSlot<Map>,
    /// %ArrayIteratorPrototype% (holds `next` and @@iterator)
    pub array_iterator_prototype: GcSlot<Object>,
    /// Map of iterator-result objects `{ value, done }` (w+, e+, c+)
    pub iterator_result_map: GcSlot<Map>,
    /// Hidden for-in enumerator: slots [level, keys, index, visited]
    /// (ES 14.7.5.9 EnumerateObjectProperties; unreachable from JS)
    pub for_in_enumerator_map: GcSlot<Map>,
    /// Proxy maps (kind PROXY + capability bits copied from the target;
    /// 0 descriptors, null prototype — a proxy owns nothing directly).
    pub proxy_map: GcSlot<Map>,
    pub proxy_callable_map: GcSlot<Map>,
    pub proxy_constructor_map: GcSlot<Map>,
    pub strings: WellKnownStrings,
}

macro_rules! define_well_known_strings {
    ($($field:ident => $text:literal),* $(,)?) => {
        pub struct WellKnownStrings {
            $(pub $field: GcSlot<SlotName>,)*
        }

        impl WellKnownStrings {
            pub fn uninit() -> Self {
                Self {
                    $($field: smi_slot::<SlotName>(),)*
                }
            }

            /// Intern every well-known string directly into its root slot.
            /// The destination must already be reachable from the root set:
            /// each interning call may allocate and collect, and a partially
            /// built local table would lose the strings interned so far.
            pub fn intern_all(
                &self,
                heap: &mut Heap,
                interner: &StringInterner,
                scope: &HandleScope<'_>,
            ) {
                $(
                    let interned = interner.intern_str(heap, scope, $text);
                    let value: Tagged<'_, SlotName> = interned.as_tagged(heap).into();
                    self.$field.init_tagged(value);
                )*
            }
        }

        impl EdgeVisitable for WellKnownStrings {
            fn visit_edges(&self, visitor: &mut dyn Visitor) {
                $(visitor.visit(self.$field.as_raw());)*
            }
        }
    };
}

define_well_known_strings! {
    empty => "",
    length => "length",
    name => "name",
    message => "message",
    prototype => "prototype",
    constructor => "constructor",
    to_string => "toString",
    value_of => "valueOf",
    next => "next",
    done => "done",
    value => "value",
    values => "values",
    default => "default",
    number => "number",
    string => "string",
    boolean => "boolean",
    number_ctor => "Number",
    boolean_ctor => "Boolean",
    symbol_ctor => "Symbol",
    object => "object",
    function => "function",
    symbol => "symbol",
    undefined => "undefined",
    null => "null",
    true_ => "true",
    false_ => "false",
    // property descriptor object fields (ES 6.2.6)
    get => "get",
    set => "set",
    writable => "writable",
    enumerable => "enumerable",
    configurable => "configurable",
    // proxy traps (ES 20.2) + Proxy.revocable surface
    has => "has",
    delete_property => "deleteProperty",
    own_keys => "ownKeys",
    get_own_property_descriptor => "getOwnPropertyDescriptor",
    define_property => "defineProperty",
    get_prototype_of => "getPrototypeOf",
    set_prototype_of => "setPrototypeOf",
    is_extensible => "isExtensible",
    prevent_extensions => "preventExtensions",
    apply => "apply",
    construct => "construct",
    revocable => "revocable",
    revoke => "revoke",
    proxy => "proxy",
}

fn smi_slot<T>() -> GcSlot<T> {
    // Safety: an encoded Smi is a valid strong word; no GC can observe it
    // before the root slot is part of the live `WellKnown` struct.
    unsafe { GcSlot::from_value(Smi::new(0).encode()) }
}

fn smi_handle<'s, T>(scope: &'s HandleScope<'_>) -> Handle<'s, T> {
    // Safety: the Smi placeholder is a valid strong word; callers only use
    // these handles for bootstrap objects whose real fields are filled in
    // before they become visible to user code.
    scope.handle(unsafe { Tagged::<T>::from_value_unchecked(Smi::new(0).encode()) })
}

impl WellKnown {
    /// Placeholder set: every slot is a smi sentinel. Valid to install
    /// before the real bootstrap phases run.
    pub fn uninit() -> Self {
        Self {
            map_map: smi_slot::<Map>(),
            the_hole: smi_slot::<Object>(),
            exception: smi_slot::<Object>(),
            exception_map: smi_slot::<Map>(),
            empty_fixed_array: smi_slot::<FixedArray>(),
            empty_context: smi_slot::<ContextObject>(),
            smi_map: smi_slot::<Map>(),
            float_map: smi_slot::<Map>(),
            array_map: smi_slot::<Map>(),
            byte_array_map: smi_slot::<Map>(),
            code_map: smi_slot::<Map>(),
            dense_latin1_string_map: smi_slot::<Map>(),
            dense_utf16_string_map: smi_slot::<Map>(),
            symbol_map: smi_slot::<Map>(),
            accessor_pair_map: smi_slot::<Map>(),
            accessor_info_map: smi_slot::<Map>(),
            callable_map: smi_slot::<Map>(),
            handler_table_map: smi_slot::<Map>(),
            context_map: smi_slot::<Map>(),
            scope_info_map: smi_slot::<Map>(),
            script_context_table_map: smi_slot::<Map>(),
            name_index_table_map: smi_slot::<Map>(),
            feedback_vector_map: smi_slot::<Map>(),
            cell_map: smi_slot::<Map>(),
            prototype_info_map: smi_slot::<Map>(),
            number_dictionary_map: smi_slot::<Map>(),
            handler_map_0: smi_slot::<Map>(),
            handler_map_1: smi_slot::<Map>(),
            handler_map_2: smi_slot::<Map>(),
            handler_map_3: smi_slot::<Map>(),
            invalid_prototype_validity_cell: smi_slot::<Cell>(),
            empty_scope_info: smi_slot::<ScopeInfo>(),
            undefined: smi_slot::<Object>(),
            undefined_map: smi_slot::<Map>(),
            null: smi_slot::<Object>(),
            null_map: smi_slot::<Map>(),
            false_object: smi_slot::<Object>(),
            true_object: smi_slot::<Object>(),
            boolean_map: smi_slot::<Map>(),
            js_array_map: smi_slot::<Map>(),
            error_map: smi_slot::<Map>(),
            type_error_map: smi_slot::<Map>(),
            reference_error_map: smi_slot::<Map>(),
            range_error_map: smi_slot::<Map>(),
            syntax_error_map: smi_slot::<Map>(),
            number_wrapper_map: smi_slot::<Map>(),
            boolean_wrapper_map: smi_slot::<Map>(),
            string_wrapper_map: smi_slot::<Map>(),
            symbol_wrapper_map: smi_slot::<Map>(),
            number_prototype: smi_slot::<Object>(),
            boolean_prototype: smi_slot::<Object>(),
            string_prototype: smi_slot::<Object>(),
            symbol_prototype: smi_slot::<Object>(),
            object_prototype: smi_slot::<Object>(),
            array_prototype: smi_slot::<Object>(),
            date_instance_map: smi_slot::<Map>(),
            date_prototype: smi_slot::<Object>(),
            error_prototype: smi_slot::<Object>(),
            function_prototype: smi_slot::<Object>(),
            global_object: smi_slot::<Object>(),
            eval_function: smi_slot::<Object>(),
            script_context_table: smi_slot::<ScriptContextTable>(),
            object_initial_map: smi_slot::<Map>(),
            function_map: smi_slot::<Map>(),
            non_constructor_function_map: smi_slot::<Map>(),
            class_constructor_map: smi_slot::<Map>(),
            to_primitive_symbol: smi_slot::<Symbol>(),
            iterator_symbol: smi_slot::<Symbol>(),
            to_string_tag_symbol: smi_slot::<Symbol>(),
            megamorphic_symbol: smi_slot::<Symbol>(),
            prototype_transition_symbol: smi_slot::<Symbol>(),
            holey_transition_symbol: smi_slot::<Symbol>(),
            dictionary_transition_symbol: smi_slot::<Symbol>(),
            array_iterator_map: smi_slot::<Map>(),
            array_iterator_prototype: smi_slot::<Object>(),
            iterator_result_map: smi_slot::<Map>(),
            for_in_enumerator_map: smi_slot::<Map>(),
            proxy_map: smi_slot::<Map>(),
            proxy_callable_map: smi_slot::<Map>(),
            proxy_constructor_map: smi_slot::<Map>(),
            strings: WellKnownStrings::uninit(),
        }
    }
}

// The slots are only mutated during single-threaded bootstrap or through
// explicit root writes; the collector visits them from the stop-the-world
// host root callback, exactly like `RootHandles`.
unsafe impl Send for WellKnown {}
unsafe impl Sync for WellKnown {}

impl EdgeVisitable for WellKnown {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.map_map.as_raw());
        visitor.visit(self.the_hole.as_raw());
        visitor.visit(self.exception.as_raw());
        visitor.visit(self.exception_map.as_raw());
        visitor.visit(self.empty_fixed_array.as_raw());
        visitor.visit(self.empty_context.as_raw());
        visitor.visit(self.smi_map.as_raw());
        visitor.visit(self.float_map.as_raw());
        visitor.visit(self.array_map.as_raw());
        visitor.visit(self.byte_array_map.as_raw());
        visitor.visit(self.code_map.as_raw());
        visitor.visit(self.dense_latin1_string_map.as_raw());
        visitor.visit(self.dense_utf16_string_map.as_raw());
        visitor.visit(self.symbol_map.as_raw());
        visitor.visit(self.accessor_pair_map.as_raw());
        visitor.visit(self.accessor_info_map.as_raw());
        visitor.visit(self.callable_map.as_raw());
        visitor.visit(self.handler_table_map.as_raw());
        visitor.visit(self.context_map.as_raw());
        visitor.visit(self.scope_info_map.as_raw());
        visitor.visit(self.script_context_table_map.as_raw());
        visitor.visit(self.name_index_table_map.as_raw());
        visitor.visit(self.feedback_vector_map.as_raw());
        visitor.visit(self.cell_map.as_raw());
        visitor.visit(self.prototype_info_map.as_raw());
        visitor.visit(self.handler_map_0.as_raw());
        visitor.visit(self.handler_map_1.as_raw());
        visitor.visit(self.handler_map_2.as_raw());
        visitor.visit(self.handler_map_3.as_raw());
        visitor.visit(self.invalid_prototype_validity_cell.as_raw());
        visitor.visit(self.empty_scope_info.as_raw());
        visitor.visit(self.undefined.as_raw());
        visitor.visit(self.undefined_map.as_raw());
        visitor.visit(self.null.as_raw());
        visitor.visit(self.null_map.as_raw());
        visitor.visit(self.false_object.as_raw());
        visitor.visit(self.true_object.as_raw());
        visitor.visit(self.boolean_map.as_raw());
        visitor.visit(self.js_array_map.as_raw());
        visitor.visit(self.error_map.as_raw());
        visitor.visit(self.type_error_map.as_raw());
        visitor.visit(self.reference_error_map.as_raw());
        visitor.visit(self.range_error_map.as_raw());
        visitor.visit(self.syntax_error_map.as_raw());
        visitor.visit(self.number_wrapper_map.as_raw());
        visitor.visit(self.boolean_wrapper_map.as_raw());
        visitor.visit(self.string_wrapper_map.as_raw());
        visitor.visit(self.symbol_wrapper_map.as_raw());
        visitor.visit(self.number_prototype.as_raw());
        visitor.visit(self.boolean_prototype.as_raw());
        visitor.visit(self.string_prototype.as_raw());
        visitor.visit(self.symbol_prototype.as_raw());
        visitor.visit(self.object_prototype.as_raw());
        visitor.visit(self.array_prototype.as_raw());
        visitor.visit(self.date_instance_map.as_raw());
        visitor.visit(self.date_prototype.as_raw());
        visitor.visit(self.error_prototype.as_raw());
        visitor.visit(self.function_prototype.as_raw());
        visitor.visit(self.global_object.as_raw());
        visitor.visit(self.eval_function.as_raw());
        visitor.visit(self.script_context_table.as_raw());
        visitor.visit(self.object_initial_map.as_raw());
        visitor.visit(self.function_map.as_raw());
        visitor.visit(self.non_constructor_function_map.as_raw());
        visitor.visit(self.class_constructor_map.as_raw());
        visitor.visit(self.to_primitive_symbol.as_raw());
        visitor.visit(self.iterator_symbol.as_raw());
        visitor.visit(self.to_string_tag_symbol.as_raw());
        visitor.visit(self.megamorphic_symbol.as_raw());
        visitor.visit(self.prototype_transition_symbol.as_raw());
        visitor.visit(self.holey_transition_symbol.as_raw());
        visitor.visit(self.dictionary_transition_symbol.as_raw());
        visitor.visit(self.array_iterator_map.as_raw());
        visitor.visit(self.array_iterator_prototype.as_raw());
        visitor.visit(self.iterator_result_map.as_raw());
        visitor.visit(self.for_in_enumerator_map.as_raw());
        visitor.visit(self.proxy_map.as_raw());
        visitor.visit(self.proxy_callable_map.as_raw());
        visitor.visit(self.proxy_constructor_map.as_raw());
        visitor.visit(self.number_dictionary_map.as_raw());
        self.strings.visit_edges(visitor);
    }
}

fn alloc_map<'s>(heap: &mut Heap, scope: &'s HandleScope<'_>, kind: MapKind) -> Handle<'s, Map> {
    let prototype = scope.handle(heap.known().null.get(heap).erase());
    heap.allocate::<Map>(MapInit {
        kind,
        value_slot_count: 0,
        descriptors: &[],
        prototype,
    })
    .as_handle(scope)
}

/// A builtin map whose `value_slot_count` doubles as per-kind metadata (the
/// `DataHandler` data count).
fn alloc_map_with_slots<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: MapKind,
    value_slot_count: usize,
) -> Handle<'s, Map> {
    let prototype = scope.handle(heap.known().null.get(heap).erase());
    heap.allocate::<Map>(MapInit {
        kind,
        value_slot_count,
        descriptors: &[],
        prototype,
    })
    .as_handle(scope)
}

fn alloc_parent_map<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: MapKind,
    parent: Handle<'_, Object>,
) -> Handle<'s, Map> {
    alloc_parent_map_with_slots(heap, scope, kind, parent, 0)
}

fn alloc_parent_map_with_slots<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    kind: MapKind,
    parent: Handle<'_, Object>,
    value_slot_count: usize,
) -> Handle<'s, Map> {
    heap.allocate::<Map>(MapInit {
        kind,
        value_slot_count,
        descriptors: &[],
        prototype: parent.erase(),
    })
    .as_handle(scope)
}

fn alloc_object<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    map: Handle<'_, Map>,
) -> Handle<'s, Object> {
    heap.new_object(scope, map, HandleSlice::EMPTY)
        .as_handle(scope)
}

pub fn bootstrap_basics(heap: &mut Heap) {
    let data = HandleData::new(Smi::new(0).into_tagged());
    let scope = unsafe { HandleScope::from_raw(NonNull::from(&data)) };
    let known = heap.known();

    let map_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::MAP,
            value_slot_count: 0,
            descriptors: &[],
            prototype: scope.handle(Smi::new(0)),
        })
        .as_handle(&scope);
    map_map.as_tagged(heap).header.map.set(
        heap,
        map_map.as_tagged(heap).erase(),
        map_map.as_tagged(heap),
    );
    known.map_map.init(map_map.as_tagged(heap));

    let the_hole_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::ODDBALL,
            value_slot_count: 0,
            descriptors: &[],
            prototype: scope.handle(Smi::new(0)),
        })
        .as_handle(&scope);
    let the_hole = heap
        .allocate::<Object>(ObjectInit {
            map: the_hole_map,
            slots: smi_handle::<FixedArray>(&scope),
            elements: smi_handle::<FixedArray>(&scope),
            length: 0,
        })
        .as_handle(&scope);

    let null_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::ODDBALL,
            value_slot_count: 0,
            descriptors: &[],
            prototype: scope.handle(Smi::new(0)),
        })
        .as_handle(&scope);
    let null = heap
        .allocate::<Object>(ObjectInit {
            map: null_map,
            slots: smi_handle::<FixedArray>(&scope),
            elements: smi_handle::<FixedArray>(&scope),
            length: 0,
        })
        .as_handle(&scope);

    // Publish the sentinels before anything reads them through `known()`
    // (`clear()` takes the heap and fetches the hole from there).
    known.the_hole.init(the_hole.as_tagged(heap));
    known.null.init(null.as_tagged(heap));
    known.null_map.init(null_map.as_tagged(heap));

    map_map.as_tagged(heap).transitions.clear(heap);
    the_hole_map.as_tagged(heap).transitions.clear(heap);
    null_map.as_tagged(heap).transitions.clear(heap);
    // these three maps were initialized before the real hole existed, so
    // their prototype_info holds the Smi placeholder; re-clear it now
    map_map.as_tagged(heap).prototype_info.clear(heap);
    the_hole_map.as_tagged(heap).prototype_info.clear(heap);
    null_map.as_tagged(heap).prototype_info.clear(heap);
    the_hole_map.as_tagged(heap).prototype.set(
        heap,
        the_hole_map.as_tagged(heap).erase(),
        null.as_tagged(heap).erase(),
    );
    null_map.as_tagged(heap).prototype.set(
        heap,
        null_map.as_tagged(heap).erase(),
        null.as_tagged(heap).erase(),
    );

    let cell_map = alloc_map(heap, &scope, MapKind::CELL);
    let prototype_info_map = alloc_map(heap, &scope, MapKind::PROTOTYPE_INFO);
    known.cell_map.init(cell_map.as_tagged(heap));
    known
        .prototype_info_map
        .init(prototype_info_map.as_tagged(heap));
    let invalid_prototype_validity_cell = heap
        .allocate::<Cell>(CellInit {
            value: Value::CLEARED,
        })
        .as_handle(&scope);
    known
        .invalid_prototype_validity_cell
        .init(invalid_prototype_validity_cell.as_tagged(heap));

    // builtin maps: every allocation init path looks these up, so they must
    // exist before any other object is created (incl. interned strings)
    let smi_map = alloc_map(heap, &scope, MapKind::OBJECT);
    let float_map = alloc_map(heap, &scope, MapKind::FLOAT);
    let array_map = alloc_map(heap, &scope, MapKind::FIXED_ARRAY);
    let byte_array_map = alloc_map(heap, &scope, MapKind::FIXED_BYTE_ARRAY);
    let code_map = alloc_map(heap, &scope, MapKind::CODE);
    let dense_latin1_string_map =
        alloc_map(heap, &scope, MapKind::DENSE_STRING.union(MapKind::LATIN1));
    let dense_utf16_string_map = alloc_map(heap, &scope, MapKind::DENSE_STRING);
    let symbol_map = alloc_map(heap, &scope, MapKind::SYMBOL);
    let accessor_pair_map = alloc_map(heap, &scope, MapKind::ACCESSOR_PAIR);
    let accessor_info_map = alloc_map(heap, &scope, MapKind::ACCESSOR_INFO);
    let callable_map = alloc_map(heap, &scope, MapKind::CALLABLE_INFO);
    let handler_table_map = alloc_map(heap, &scope, MapKind::HANDLER_TABLE);
    let context_map = alloc_map(heap, &scope, MapKind::CONTEXT);
    let scope_info_map = alloc_map(heap, &scope, MapKind::SCOPE_INFO);
    let script_context_table_map = alloc_map(heap, &scope, MapKind::SCRIPT_CONTEXT_TABLE);
    let name_index_table_map = alloc_map(heap, &scope, MapKind::NAME_INDEX_TABLE);
    let feedback_vector_map = alloc_map(heap, &scope, MapKind::FEEDBACK_VECTOR);
    let handler_map_0 = alloc_map_with_slots(heap, &scope, MapKind::DATA_HANDLER, 0);
    let handler_map_1 = alloc_map_with_slots(heap, &scope, MapKind::DATA_HANDLER, 1);
    let handler_map_2 = alloc_map_with_slots(heap, &scope, MapKind::DATA_HANDLER, 2);
    let handler_map_3 = alloc_map_with_slots(heap, &scope, MapKind::DATA_HANDLER, 3);
    let number_dictionary_map = alloc_map(heap, &scope, MapKind::NUMBER_DICTIONARY);

    let function_map = heap
        .allocate::<Map>(MapInit {
            // TODO: arrow/generator functions get a non-constructor map
            // once the compiler distinguishes them
            kind: MapKind::OBJECT
                .union(MapKind::CALLABLE)
                .union(MapKind::CONSTRUCTOR)
                .union(MapKind::EXTENDABLE),
            value_slot_count: 2,
            descriptors: &[],
            prototype: null.erase(),
        })
        .as_handle(&scope);
    let non_constructor_function_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::OBJECT
                .union(MapKind::CALLABLE)
                .union(MapKind::EXTENDABLE),
            value_slot_count: 2,
            descriptors: &[],
            prototype: null.erase(),
        })
        .as_handle(&scope);
    let class_constructor_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::OBJECT
                .union(MapKind::CALLABLE)
                .union(MapKind::CONSTRUCTOR)
                .union(MapKind::EXTENDABLE)
                .union(MapKind::CLASS_CONSTRUCTOR),
            // slots: [callable info, closure context, instance-field array]
            value_slot_count: 3,
            descriptors: &[],
            prototype: null.erase(),
        })
        .as_handle(&scope);

    known.smi_map.init(smi_map.as_tagged(heap));
    known.float_map.init(float_map.as_tagged(heap));
    known.array_map.init(array_map.as_tagged(heap));
    known.byte_array_map.init(byte_array_map.as_tagged(heap));
    known.code_map.init(code_map.as_tagged(heap));
    known
        .dense_latin1_string_map
        .init(dense_latin1_string_map.as_tagged(heap));
    known
        .dense_utf16_string_map
        .init(dense_utf16_string_map.as_tagged(heap));
    known.symbol_map.init(symbol_map.as_tagged(heap));
    known
        .accessor_pair_map
        .init(accessor_pair_map.as_tagged(heap));
    known
        .accessor_info_map
        .init(accessor_info_map.as_tagged(heap));
    known.callable_map.init(callable_map.as_tagged(heap));
    known
        .handler_table_map
        .init(handler_table_map.as_tagged(heap));
    known.context_map.init(context_map.as_tagged(heap));
    known.scope_info_map.init(scope_info_map.as_tagged(heap));
    known
        .script_context_table_map
        .init(script_context_table_map.as_tagged(heap));
    known
        .name_index_table_map
        .init(name_index_table_map.as_tagged(heap));
    known
        .feedback_vector_map
        .init(feedback_vector_map.as_tagged(heap));
    known.handler_map_0.init(handler_map_0.as_tagged(heap));
    known
        .number_dictionary_map
        .init(number_dictionary_map.as_tagged(heap));
    known.handler_map_1.init(handler_map_1.as_tagged(heap));
    known.handler_map_2.init(handler_map_2.as_tagged(heap));
    known.handler_map_3.init(handler_map_3.as_tagged(heap));
    known.function_map.init(function_map.as_tagged(heap));
    known
        .non_constructor_function_map
        .init(non_constructor_function_map.as_tagged(heap));
    known
        .class_constructor_map
        .init(class_constructor_map.as_tagged(heap));
}

pub fn intern_well_known_strings(heap: &mut Heap, interner: &StringInterner) {
    let data = HandleData::new(heap.known().the_hole.get(heap).erase());
    let scope = unsafe { HandleScope::from_raw(NonNull::from(&data)) };
    heap.known().strings.intern_all(heap, interner, &scope);
}

pub fn bootstrap_well_known(heap: &mut Heap) {
    let known = heap.known();
    debug_assert!(
        !known.the_hole.get(heap).is_smi(),
        "bootstrap_basics must run before bootstrap_well_known"
    );

    let data = HandleData::new(known.the_hole.get(heap).erase());
    let scope = unsafe { HandleScope::from_raw(NonNull::from(&data)) };

    let object_prototype_map = alloc_map(
        heap,
        &scope,
        MapKind::OBJECT
            .union(MapKind::EXTENDABLE)
            .union(MapKind::IMMUTABLE_PROTO),
    );
    let empty_slots = heap
        .allocate::<FixedArray>(HandleSlice::EMPTY)
        .as_handle(&scope);
    known.empty_fixed_array.init(empty_slots.as_tagged(heap));
    let object_prototype = alloc_object(heap, &scope, object_prototype_map);

    let array_prototype_map = alloc_parent_map(
        heap,
        &scope,
        MapKind::ARRAY.union(MapKind::EXTENDABLE),
        object_prototype,
    );
    let array_prototype = alloc_object(heap, &scope, array_prototype_map);
    let length_name = scope.handle(known.strings.length.get(heap));
    let array_length_info = heap
        .allocate::<AccessorInfo>(AccessorInfoInit {
            getter: Some(NativeIndex(bytecode::BuiltinFn::ArrayLengthGet as usize)),
            setter: Some(NativeIndex(bytecode::BuiltinFn::ArrayLengthSet as usize)),
        })
        .as_handle(&scope);
    let array_length_descriptor: [(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>); 1] = [(
        length_name,
        SlotFlags::ACCESSOR.union(SlotFlags::WRITABLE),
        scope.handle(array_length_info.as_tagged(heap).erase()),
    )];
    let js_array_map = heap
        .allocate::<Map>(MapInit {
            kind: MapKind::ARRAY.union(MapKind::EXTENDABLE),
            value_slot_count: 0,
            descriptors: &array_length_descriptor,
            prototype: array_prototype.erase(),
        })
        .as_handle(&scope);

    let date_prototype_map = alloc_parent_map(
        heap,
        &scope,
        MapKind::OBJECT.union(MapKind::EXTENDABLE),
        object_prototype,
    );
    let date_prototype = alloc_object(heap, &scope, date_prototype_map);
    // Date instances carry the [[DateValue]] in one in-object value slot
    // (date.rs stores it via `new_object`); declaring it here keeps the
    // map's layout in sync so later property stores transition correctly.
    // The DATE flag is inherited by every transition map, so adding
    // properties to a Date does not lose its internal slot.
    let date_instance_map = alloc_parent_map_with_slots(
        heap,
        &scope,
        MapKind::OBJECT
            .union(MapKind::EXTENDABLE)
            .union(MapKind::DATE),
        date_prototype,
        1,
    );

    let error_prototype_map = alloc_parent_map(
        heap,
        &scope,
        MapKind::OBJECT.union(MapKind::EXTENDABLE),
        object_prototype,
    );
    let error_prototype = alloc_object(heap, &scope, error_prototype_map);

    let error_map = alloc_parent_map(
        heap,
        &scope,
        MapKind::OBJECT
            .union(MapKind::EXTENDABLE)
            .union(MapKind::ERROR),
        error_prototype,
    );

    let object_initial_map = alloc_parent_map(
        heap,
        &scope,
        MapKind::OBJECT.union(MapKind::EXTENDABLE),
        object_prototype,
    );
    let global_object = alloc_object(heap, &scope, object_initial_map);
    let script_names = heap
        .allocate_handle::<NameToIndexHashTable>(NameToIndexHashTableInit { capacity: 16 }, &scope);
    let script_context_table = heap.allocate_handle::<ScriptContextTable>(
        ScriptContextTableInit {
            capacity: 0,
            names: script_names,
        },
        &scope,
    );

    let undefined_map = alloc_parent_map(heap, &scope, MapKind::ODDBALL, object_prototype);
    let boolean_map = alloc_parent_map(heap, &scope, MapKind::ODDBALL, object_prototype);

    let undefined = alloc_object(heap, &scope, undefined_map);
    let false_object = alloc_object(heap, &scope, boolean_map);
    let true_object = alloc_object(heap, &scope, boolean_map);

    let exception_map = alloc_parent_map(heap, &scope, MapKind::ODDBALL, object_prototype);
    let exception = alloc_object(heap, &scope, exception_map);

    known
        .to_primitive_symbol
        .init_tagged(Symbol::new(heap, &scope, b"Symbol.toPrimitive").as_tagged(heap));
    known
        .iterator_symbol
        .init_tagged(Symbol::new(heap, &scope, b"Symbol.iterator").as_tagged(heap));
    known
        .to_string_tag_symbol
        .init_tagged(Symbol::new(heap, &scope, b"Symbol.toStringTag").as_tagged(heap));
    // IC state sentinel (never user-visible): a feedback slot holding it
    // permanently takes the slow path
    known
        .megamorphic_symbol
        .init_tagged(Symbol::new(heap, &scope, b"<megamorphic>").as_tagged(heap));
    // transition-tree key for prototype changes (never user-visible)
    known
        .prototype_transition_symbol
        .init_tagged(Symbol::new(heap, &scope, b"<set-prototype>").as_tagged(heap));
    known
        .holey_transition_symbol
        .init_tagged(Symbol::new(heap, &scope, b"<promote-holey>").as_tagged(heap));
    // transition-tree key for sparse-array normalization (never user-visible)
    known
        .dictionary_transition_symbol
        .init_tagged(Symbol::new(heap, &scope, b"<normalize-dictionary>").as_tagged(heap));

    let empty_scope_info = heap
        .allocate::<ScopeInfo>(ScopeInfoInit {
            names: HandleSlice::EMPTY,
            infos: &[],
            flags: 0,
        })
        .as_handle(&scope);
    let empty_context = heap
        .allocate::<ContextObject>(ContextInit {
            outer: None,
            slots: empty_slots,
            scope_info: empty_scope_info,
        })
        .as_handle(&scope);

    // %Function.prototype% (ES 19.2.3): the canonical empty function — a
    // callable, non-constructable function whose [[Prototype]] is
    // %Object.prototype%; the [[Prototype]] of ordinary function objects
    // (patched onto function_map at the end). The body is a single
    // `Return` (bytecode::Opcode::Return as u8) so calling it yields
    // undefined.
    let function_prototype_map = alloc_parent_map_with_slots(
        heap,
        &scope,
        MapKind::OBJECT
            .union(MapKind::CALLABLE)
            .union(MapKind::EXTENDABLE),
        object_prototype,
        2, // [callable info, context], like function_map
    );
    let empty_code = heap
        .allocate::<Code>(CodeInit { bytes: &[1] })
        .as_handle(&scope);
    let empty_info = heap
        .allocate::<CallableInfoObject>(CallableInfoInit {
            code: empty_code,
            constants: empty_slots,
            register_count: 0,
            handlers: None,
            expected_slots: 0,
            mode: MODE_SLOPPY,
        })
        .as_handle(&scope);
    let function_prototype = heap
        .new_object(
            &scope,
            function_prototype_map,
            scope.stage(&[
                empty_info.as_tagged(heap).erase(),
                empty_context.as_tagged(heap).erase(),
            ]),
        )
        .as_handle(&scope);

    known.undefined.init(undefined.as_tagged(heap));
    known.undefined_map.init(undefined_map.as_tagged(heap));
    known.false_object.init(false_object.as_tagged(heap));
    known.true_object.init(true_object.as_tagged(heap));
    known.boolean_map.init(boolean_map.as_tagged(heap));
    known.exception.init(exception.as_tagged(heap));
    known
        .object_prototype
        .init(object_prototype.as_tagged(heap));
    known.array_prototype.init(array_prototype.as_tagged(heap));
    known.date_prototype.init(date_prototype.as_tagged(heap));
    known
        .date_instance_map
        .init(date_instance_map.as_tagged(heap));
    known.error_prototype.init(error_prototype.as_tagged(heap));
    known
        .function_prototype
        .init(function_prototype.as_tagged(heap));
    known.error_map.init(error_map.as_tagged(heap));

    known.type_error_map.init(error_map.as_tagged(heap));
    known.reference_error_map.init(error_map.as_tagged(heap));
    known.range_error_map.init(error_map.as_tagged(heap));
    known.syntax_error_map.init(error_map.as_tagged(heap));
    known.number_wrapper_map.init(error_map.as_tagged(heap));
    known.boolean_wrapper_map.init(error_map.as_tagged(heap));
    known.string_wrapper_map.init(error_map.as_tagged(heap));
    known.symbol_wrapper_map.init(error_map.as_tagged(heap));
    known
        .symbol_prototype
        .init(object_prototype.as_tagged(heap));
    known.exception_map.init(exception_map.as_tagged(heap));
    known.js_array_map.init(js_array_map.as_tagged(heap));

    let proxy_map = alloc_map(heap, &scope, MapKind::PROXY);
    let proxy_callable_map = alloc_map(heap, &scope, MapKind::PROXY.union(MapKind::CALLABLE));
    let proxy_constructor_map = alloc_map(
        heap,
        &scope,
        MapKind::PROXY
            .union(MapKind::CALLABLE)
            .union(MapKind::CONSTRUCTOR),
    );
    known.proxy_map.init(proxy_map.as_tagged(heap));
    known
        .proxy_callable_map
        .init(proxy_callable_map.as_tagged(heap));
    known
        .proxy_constructor_map
        .init(proxy_constructor_map.as_tagged(heap));

    known.empty_context.init(empty_context.as_tagged(heap));
    known
        .empty_scope_info
        .init(empty_scope_info.as_tagged(heap));
    known.global_object.init(global_object.as_tagged(heap));
    known.eval_function.init(known.undefined.get(heap));
    known
        .script_context_table
        .init(script_context_table.as_tagged(heap));
    known
        .object_initial_map
        .init(object_initial_map.as_tagged(heap));

    let null = known.null.get(heap);
    let null_erased = null.erase();
    let null_object = null.as_ref();
    let empty_fixed_array = known.empty_fixed_array.get(heap);
    null_object.slots.set(heap, null_erased, empty_fixed_array);
    null_object
        .elements
        .set(heap, null_erased, empty_fixed_array);
    // ordinary function objects' [[Prototype]] is %Function.prototype%
    // (ES 19.2.3.1): function_map was created with a null placeholder
    // in bootstrap_basics
    let function_map = known.function_map.get(heap);
    function_map.prototype.set(
        heap,
        function_map.erase(),
        function_prototype.as_tagged(heap).erase(),
    );
    let non_constructor_function_map = known.non_constructor_function_map.get(heap);
    non_constructor_function_map.prototype.set(
        heap,
        non_constructor_function_map.erase(),
        function_prototype.as_tagged(heap).erase(),
    );
    let class_constructor_map = known.class_constructor_map.get(heap);
    class_constructor_map.prototype.set(
        heap,
        class_constructor_map.erase(),
        function_prototype.as_tagged(heap).erase(),
    );
}
