//! Minimal builtin library: the globals the test262 harness needs.
//!
//! Installed once per VM, before any user code runs: `Number`, `Boolean`,
//! `Error`, `TypeError`, `eval`, plus the wrapper/error maps in
//! `WellKnown` and `.prototype`/`.constructor` plumbing for user
//! functions (interpreter `CreateClosure`).

pub mod array;
pub mod boolean;
pub mod date;
pub mod error;
pub mod function;
pub mod global;
pub mod math;
pub mod number;
pub mod object;
pub mod proxy;
pub mod string;
pub mod symbol;

/// Generate a module's `register` function from native names.
///
/// `field` maps to `ids.field = vm.register_native(field)`; use
/// `field -> function` when the `BuiltinIndices` field and function names
/// differ.
#[macro_export]
macro_rules! register_natives {
    // Fill the runtime's own builtin table.
    ($($field:ident $(-> $function:path)?),* $(,)?) => {
        $crate::register_natives!(@into $crate::BuiltinIndices; $($field $(-> $function)?),*);
    };
    // Fill another id table, e.g. the compiler's `NativeIds`.
    ($ids:ty; $($field:ident $(-> $function:path)?),* $(,)?) => {
        $crate::register_natives!(@into $ids; $($field $(-> $function)?),*);
    };
    (@into $ids:ty; $($field:ident $(-> $function:path)?),* $(,)?) => {
        pub fn register(vm: &mut ::vm_core::VM, ids: &mut $ids) {
            $(
                ids.$field = vm.register_native(
                    $crate::register_natives!(@function $field $(-> $function)?)
                );
            )*
        }
    };
    (@function $field:ident) => { $field };
    (@function $field:ident -> $function:path) => { $function };
}

use js_compiler::native::NativeIds;
use vm_core::{
    DenseString, EdgeVisitable, Float, Handle, HandleScope, HandleSlice, Map, MapInit, MapKind,
    NativeIndex, Object, PropertyDescriptor, Runtime, RuntimeId, SetupError, SlotFlags, SlotName,
    Smi, Symbol, Thread, VM, Value, Visitor,
};

mod control_flow;
mod known;

/// The JS runtime: its state is the runtime itself (one instance per VM).
pub struct JSRuntime {
    pub indices: BuiltinIndices,
    /// Ids of the native functions the compiler emits in `CallNative`.
    pub ids: NativeIds,
    /// Well-known roots only JS uses.
    pub known: known::JsKnown,
}

impl Runtime for JSRuntime {
    const ID: RuntimeId = RuntimeId(1);

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), SetupError> {
        state.ids = control_flow::native_ids(vm);
        state.indices = register_builtin_natives(vm);
        {
            let mut thread = vm.attach();
            thread.handle_scope(|thread, scope| {
                state
                    .known
                    .strings
                    .intern_all(thread.heap(), vm.interner(), &scope);
                state.known.init(thread.heap(), &scope);
            });
        }
        install_builtins(vm, state);
        Ok(())
    }
}

impl Default for JSRuntime {
    fn default() -> Self {
        Self {
            indices: BuiltinIndices::default(),
            known: known::JsKnown::default(),
            ids: NativeIds::default(),
        }
    }
}

impl EdgeVisitable for JSRuntime {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        self.known.visit_edges(visitor);
    }
}

/// Register the native functions behind the builtins.
pub fn register_builtin_natives(vm: &mut VM) -> BuiltinIndices {
    let mut ids = BuiltinIndices::default();
    array::register(vm, &mut ids);
    boolean::register(vm, &mut ids);
    date::register(vm, &mut ids);
    error::register(vm, &mut ids);
    function::register(vm, &mut ids);
    global::register(vm, &mut ids);
    math::register(vm, &mut ids);
    number::register(vm, &mut ids);
    object::register(vm, &mut ids);
    proxy::register(vm, &mut ids);
    string::register(vm, &mut ids);
    symbol::register(vm, &mut ids);
    ids
}

#[derive(Default)]
pub struct BuiltinIndices {
    pub eval: NativeIndex,
    pub string: NativeIndex,
    pub string_value_of: NativeIndex,
    pub string_to_string: NativeIndex,
    pub reference_error: NativeIndex,
    pub range_error: NativeIndex,
    pub syntax_error: NativeIndex,
    pub eval_error: NativeIndex,
    pub uri_error: NativeIndex,
    pub function_to_string: NativeIndex,
    pub object_to_string: NativeIndex,
    pub number: NativeIndex,
    pub number_is_finite: NativeIndex,
    pub number_is_integer: NativeIndex,
    pub number_is_nan: NativeIndex,
    pub number_is_safe_integer: NativeIndex,
    pub number_value_of: NativeIndex,
    pub number_to_string: NativeIndex,
    pub number_to_exponential: NativeIndex,
    pub number_to_locale_string: NativeIndex,
    pub boolean: NativeIndex,
    pub boolean_value_of: NativeIndex,
    pub boolean_to_string: NativeIndex,
    pub error: NativeIndex,
    pub type_error: NativeIndex,
    pub error_is_error: NativeIndex,
    pub error_to_string: NativeIndex,
    pub object: NativeIndex,
    pub object_create: NativeIndex,
    pub object_define_properties: NativeIndex,
    pub object_get_prototype_of: NativeIndex,
    pub object_set_prototype_of: NativeIndex,
    pub array: NativeIndex,
    pub is_nan: NativeIndex,
    pub is_finite: NativeIndex,
    pub parse_int: NativeIndex,
    pub parse_float: NativeIndex,
    pub array_values: NativeIndex,
    pub array_push: NativeIndex,
    pub array_pop: NativeIndex,
    pub array_map: NativeIndex,
    pub array_reduce: NativeIndex,
    pub math_log: NativeIndex,
    pub math_pow: NativeIndex,
    pub number_to_fixed: NativeIndex,
    pub number_to_precision: NativeIndex,
    pub date: NativeIndex,
    pub date_now: NativeIndex,
    pub date_value_of: NativeIndex,
    pub date_get_time: NativeIndex,
    pub date_get_date: NativeIndex,
    pub date_get_day: NativeIndex,
    pub date_get_full_year: NativeIndex,
    pub date_get_hours: NativeIndex,
    pub date_get_milliseconds: NativeIndex,
    pub date_get_minutes: NativeIndex,
    pub date_get_month: NativeIndex,
    pub date_get_seconds: NativeIndex,
    pub date_get_timezone_offset: NativeIndex,
    pub date_get_utc_date: NativeIndex,
    pub date_get_utc_day: NativeIndex,
    pub date_get_utc_full_year: NativeIndex,
    pub date_get_utc_hours: NativeIndex,
    pub date_get_utc_milliseconds: NativeIndex,
    pub date_get_utc_minutes: NativeIndex,
    pub date_get_utc_month: NativeIndex,
    pub date_get_utc_seconds: NativeIndex,
    pub date_get_year: NativeIndex,
    pub date_set_date: NativeIndex,
    pub date_set_full_year: NativeIndex,
    pub date_set_hours: NativeIndex,
    pub date_set_milliseconds: NativeIndex,
    pub date_set_minutes: NativeIndex,
    pub date_set_month: NativeIndex,
    pub date_set_seconds: NativeIndex,
    pub date_set_time: NativeIndex,
    pub date_set_utc_date: NativeIndex,
    pub date_set_utc_full_year: NativeIndex,
    pub date_set_utc_hours: NativeIndex,
    pub date_set_utc_milliseconds: NativeIndex,
    pub date_set_utc_minutes: NativeIndex,
    pub date_set_utc_month: NativeIndex,
    pub date_set_utc_seconds: NativeIndex,
    pub date_set_year: NativeIndex,
    pub date_to_string: NativeIndex,
    pub date_to_iso_string: NativeIndex,
    pub date_to_json: NativeIndex,
    pub date_to_gmt_string: NativeIndex,
    pub date_parse: NativeIndex,
    pub print: NativeIndex,
    pub performance_now: NativeIndex,
    pub array_iterator_next: NativeIndex,
    pub array_iterator_symbol_iterator: NativeIndex,
    pub symbol: NativeIndex,
    pub symbol_description: NativeIndex,
    pub symbol_to_string: NativeIndex,
    pub symbol_value_of: NativeIndex,
    pub symbol_to_primitive: NativeIndex,
    pub symbol_for: NativeIndex,
    pub symbol_key_for: NativeIndex,
    pub object_has_own_property: NativeIndex,
    pub object_is: NativeIndex,
    pub object_property_is_enumerable: NativeIndex,
    pub object_proto_get: NativeIndex,
    pub object_proto_set: NativeIndex,
    pub object_get_own_property_names: NativeIndex,
    pub object_get_own_property_descriptor: NativeIndex,
    pub object_define_property: NativeIndex,
    pub function_bind: NativeIndex,
    pub function_call: NativeIndex,
    pub function_apply: NativeIndex,
    pub function_constructor: NativeIndex,
    pub array_is_array: NativeIndex,
    pub proxy: NativeIndex,
    pub proxy_revocable: NativeIndex,
    pub proxy_revoke: NativeIndex,
    pub object_prevent_extensions: NativeIndex,
    pub object_is_extensible: NativeIndex,
    pub object_seal: NativeIndex,
    pub object_freeze: NativeIndex,
    pub object_is_frozen: NativeIndex,
    pub object_is_sealed: NativeIndex,
    pub math_sqrt: NativeIndex,
    pub math_abs: NativeIndex,
    pub math_floor: NativeIndex,
    pub math_ceil: NativeIndex,
    pub math_trunc: NativeIndex,
    pub math_round: NativeIndex,
    pub math_min: NativeIndex,
    pub math_max: NativeIndex,
    pub math_random: NativeIndex,
    pub console_log: NativeIndex,
    pub array_join: NativeIndex,
    pub array_to_string: NativeIndex,
    pub array_slice: NativeIndex,
    pub array_sort: NativeIndex,
    pub array_at: NativeIndex,
    pub array_includes: NativeIndex,
    pub array_index_of: NativeIndex,
    pub array_last_index_of: NativeIndex,
    pub array_fill: NativeIndex,
    pub array_reverse: NativeIndex,
    pub array_shift: NativeIndex,
    pub array_unshift: NativeIndex,
    pub array_of: NativeIndex,
}

/// Install one NativeError class: constructor, prototype, and [[ErrorData]] map.
type ErrorClass<'s> = (Handle<'s, Object>, Handle<'s, Object>, Handle<'s, Map>);

fn install_error_class<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    index: NativeIndex,
    class: &str,
    parent: Handle<'_, Object>,
) -> ErrorClass<'s> {
    let (constructor, prototype) =
        Object::install_constructor(thread, scope, index, class, parent, 1);
    let name = thread.intern(scope, class);
    // Safety: fresh interned word, rooted below before the define.
    let name: Handle<'_, DenseString> = scope.handle(name.as_tagged(thread.heap()));
    let name_key = thread
        .heap()
        .known()
        .strings
        .name
        .get_handle(thread.heap(), scope);
    Object::define_own_property_ordinary(
        thread.heap(),
        scope,
        prototype,
        name_key,
        PropertyDescriptor::method(name.erase()),
    )
    .expect("bootstrap define");
    let message_key = thread
        .heap()
        .known()
        .strings
        .message
        .get_handle(thread.heap(), scope);
    let empty = scope.handle(
        thread
            .heap()
            .known()
            .strings
            .empty
            .get(thread.heap())
            .erase(),
    );
    Object::define_own_property_ordinary(
        thread.heap(),
        scope,
        prototype,
        message_key,
        PropertyDescriptor::method(empty),
    )
    .expect("bootstrap define");
    let map = thread.heap().allocate_handle::<Map>(
        MapInit {
            kind: MapKind::OBJECT
                .union(MapKind::EXTENDABLE)
                .union(MapKind::ERROR),
            value_slot_count: 0,
            descriptors: &[],
            prototype: prototype.erase(),
        },
        scope,
    );
    (constructor, prototype, map)
}

/// Build the builtin objects and install them on the global object.
pub fn install_builtins(vm: &mut VM, state: &mut JSRuntime) {
    let idx = &state.indices;
    let known = vm.known();
    let mut thread = vm.attach();
    let wks = &known.strings;
    thread.handle_scope(|thread, scope| {
        let heap = thread.heap();
        let wks_empty: Handle<'_, Value> = scope.handle(wks.empty.get(heap).erase());
        let wks_length: Handle<'_, SlotName> = scope.handle(wks.length.get(heap));
        let wks_constructor: Handle<'_, SlotName> = scope.handle(wks.constructor.get(heap));
        let wks_prototype: Handle<'_, SlotName> = scope.handle(wks.prototype.get(heap));

        // `parseInt`/`parseFloat` are single intrinsic function objects shared
        // by the global object and `Number` (ES 21.1.2.13).
        let parse_float_fn = Object::native_function(thread.heap(), &scope, idx.parse_float, 1);
        Object::set_builtin_name(thread, &scope, parse_float_fn, "parseFloat");
        let parse_int_fn = Object::native_function(thread.heap(), &scope, idx.parse_int, 2);
        Object::set_builtin_name(thread, &scope, parse_int_fn, "parseInt");

        // ---- Number ----------------------------------------------------------
        let object_prototype = known.object_prototype.get_handle(thread.heap(), &scope);
        let number_zero: Handle<'_, Value> = scope.handle(Smi::new(0).into_tagged());
        let (number_fn, number_proto) = Object::install_wrapper_constructor(
            thread,
            &scope,
            idx.number,
            "Number",
            object_prototype,
            number_zero,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "valueOf",
            idx.number_value_of,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toString",
            idx.number_to_string,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toExponential",
            idx.number_to_exponential,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toFixed",
            idx.number_to_fixed,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toLocaleString",
            idx.number_to_locale_string,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toPrecision",
            idx.number_to_precision,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_fn,
            "isFinite",
            idx.number_is_finite,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            number_fn,
            "isInteger",
            idx.number_is_integer,
            1,
        );
        Object::install_method(thread, &scope, number_fn, "isNaN", idx.number_is_nan, 1);
        Object::install_method(
            thread,
            &scope,
            number_fn,
            "isSafeInteger",
            idx.number_is_safe_integer,
            1,
        );
        for (name, value) in [("parseFloat", parse_float_fn), ("parseInt", parse_int_fn)] {
            let key = thread.intern(&scope, name);
            // Safety: fresh interned word, rooted below before the define.
            let key = scope.handle(key.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                number_fn,
                key,
                PropertyDescriptor::method(value.erase()),
            )
            .expect("bootstrap define");
        }
        // static data properties on the Number constructor
        let pos_inf = thread
            .heap()
            .allocate_handle::<Float>(f64::INFINITY, &scope);
        let neg_inf = thread
            .heap()
            .allocate_handle::<Float>(f64::NEG_INFINITY, &scope);
        let max_value = thread.heap().allocate_handle::<Float>(f64::MAX, &scope);
        let min_value = thread.heap().allocate_handle::<Float>(5e-324, &scope);
        let number_nan = thread.heap().allocate_handle::<Float>(f64::NAN, &scope);
        let epsilon = thread.heap().allocate_handle::<Float>(f64::EPSILON, &scope);
        let max_safe = thread
            .heap()
            .allocate_handle::<Float>(9_007_199_254_740_991.0, &scope);
        let min_safe = thread
            .heap()
            .allocate_handle::<Float>(-9_007_199_254_740_991.0, &scope);
        for (name, value) in [
            ("POSITIVE_INFINITY", pos_inf),
            ("NEGATIVE_INFINITY", neg_inf),
            ("MAX_VALUE", max_value),
            ("MIN_VALUE", min_value),
            ("NaN", number_nan),
            ("EPSILON", epsilon),
            ("MAX_SAFE_INTEGER", max_safe),
            ("MIN_SAFE_INTEGER", min_safe),
        ] {
            let n = thread.intern(&scope, name);
            // Safety: fresh interned word, rooted below before the define.
            let n = scope.handle(n.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                number_fn,
                n,
                PropertyDescriptor::Data {
                    value: value.erase(),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            )
            .expect("bootstrap define");
        }
        {
            let heap = thread.heap();

            known.number_prototype.init(number_proto.as_tagged(heap));
            let map = heap.allocate::<Map>(MapInit {
                kind: MapKind::OBJECT
                    .union(MapKind::EXTENDABLE)
                    .union(MapKind::PRIMITIVE_WRAPPER),
                value_slot_count: 1,
                descriptors: &[],
                prototype: number_proto.erase(),
            });
            known.number_wrapper_map.init(map);
        }
        let _ = number_fn;

        // ---- Boolean ----------------------------------------------------------
        let boolean_false: Handle<'_, Value> =
            known.false_object.get_handle(thread.heap(), &scope).erase();
        let (_, boolean_proto) = Object::install_wrapper_constructor(
            thread,
            &scope,
            idx.boolean,
            "Boolean",
            object_prototype,
            boolean_false,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            boolean_proto,
            "valueOf",
            idx.boolean_value_of,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            boolean_proto,
            "toString",
            idx.boolean_to_string,
            0,
        );
        {
            let heap = thread.heap();

            known.boolean_prototype.init(boolean_proto.as_tagged(heap));
            let map = heap.allocate::<Map>(MapInit {
                kind: MapKind::OBJECT
                    .union(MapKind::EXTENDABLE)
                    .union(MapKind::PRIMITIVE_WRAPPER),
                value_slot_count: 1,
                descriptors: &[],
                prototype: boolean_proto.erase(),
            });
            known.boolean_wrapper_map.init(map);
        }

        // ---- String -----------------------------------------------------------
        let (_, string_proto) = Object::install_wrapper_constructor(
            thread,
            &scope,
            idx.string,
            "String",
            object_prototype,
            wks_empty,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            string_proto,
            "valueOf",
            idx.string_value_of,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            string_proto,
            "toString",
            idx.string_to_string,
            0,
        );
        {
            let heap = thread.heap();

            known.string_prototype.init(string_proto.as_tagged(heap));
            let map = heap.allocate::<Map>(MapInit {
                kind: MapKind::OBJECT
                    .union(MapKind::EXTENDABLE)
                    .union(MapKind::PRIMITIVE_WRAPPER),
                value_slot_count: 1,
                descriptors: &[],
                prototype: string_proto.erase(),
            });
            known.string_wrapper_map.init(map);
        }

        // ---- Error / TypeError / ReferenceError / RangeError / SyntaxError ------
        let (error_fn, error_proto, error_map) =
            install_error_class(thread, &scope, idx.error, "Error", object_prototype);
        known.error_map.init(error_map.as_tagged(thread.heap()));
        Object::install_method(
            thread,
            &scope,
            error_proto,
            "toString",
            idx.error_to_string,
            0,
        );

        let (_, _, type_error_map) =
            install_error_class(thread, &scope, idx.type_error, "TypeError", error_proto);
        known
            .type_error_map
            .init(type_error_map.as_tagged(thread.heap()));

        let (_, _, reference_error_map) = install_error_class(
            thread,
            &scope,
            idx.reference_error,
            "ReferenceError",
            error_proto,
        );
        known
            .reference_error_map
            .init(reference_error_map.as_tagged(thread.heap()));

        let (_, _, range_error_map) =
            install_error_class(thread, &scope, idx.range_error, "RangeError", error_proto);
        known
            .range_error_map
            .init(range_error_map.as_tagged(thread.heap()));

        let (_, _, syntax_error_map) =
            install_error_class(thread, &scope, idx.syntax_error, "SyntaxError", error_proto);
        known
            .syntax_error_map
            .init(syntax_error_map.as_tagged(thread.heap()));

        let (_, _, eval_error_map) =
            install_error_class(thread, &scope, idx.eval_error, "EvalError", error_proto);
        known
            .eval_error_map
            .init(eval_error_map.as_tagged(thread.heap()));

        let (_, _, uri_error_map) =
            install_error_class(thread, &scope, idx.uri_error, "URIError", error_proto);
        known
            .uri_error_map
            .init(uri_error_map.as_tagged(thread.heap()));

        Object::install_method(thread, &scope, error_fn, "isError", idx.error_is_error, 1);

        // ---- Function (constructor: dynamic bodies via eval, ES 20.2.1) --------
        let function_prototype = known.function_prototype.get_handle(thread.heap(), &scope);
        let function_fn =
            Object::native_constructor(thread.heap(), &scope, idx.function_constructor, 1);
        Object::set_builtin_name(thread, &scope, function_fn, "Function");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            function_prototype,
            wks_constructor,
            PropertyDescriptor::method(function_fn.erase()),
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            function_fn,
            wks_prototype,
            PropertyDescriptor::Data {
                value: function_prototype.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");
        let function_name = thread.intern(&scope, "Function");
        // Safety: fresh interned word, rooted below before the define.
        let function_name = scope.handle(function_name.as_tagged(thread.heap()));
        let global_object = known.global_object.get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global_object,
            function_name,
            PropertyDescriptor::method(function_fn.erase()),
        )
        .expect("bootstrap define");

        {
            let heap = thread.heap();
            let function_prototype = heap.known().function_prototype.get_handle(heap, &scope);
            let kind = MapKind::OBJECT
                .union(MapKind::EXTENDABLE)
                .union(MapKind::CALLABLE)
                .union(MapKind::BOUND);
            let bound_map = heap.allocate::<Map>(MapInit {
                kind,
                value_slot_count: 3,
                descriptors: &[],
                prototype: function_prototype.erase(),
            });
            known.bound_function_map.init(bound_map);
            let bound_constructor_map = heap.allocate::<Map>(MapInit {
                kind: kind.union(MapKind::CONSTRUCTOR),
                value_slot_count: 3,
                descriptors: &[],
                prototype: function_prototype.erase(),
            });
            known.bound_constructor_map.init(bound_constructor_map);
        }

        // ---- Function.prototype toString/call/apply/bind ------------------------
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "toString",
            idx.function_to_string,
            0,
        );
        // call/apply are plain (non-constructor) runtime functions
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "call",
            idx.function_call,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "apply",
            idx.function_apply,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "bind",
            idx.function_bind,
            1,
        );
        // ---- eval -------------------------------------------------------------
        let eval_fn = Object::native_function(thread.heap(), &scope, idx.eval, 1);
        Object::set_builtin_name(thread, &scope, eval_fn, "eval");
        state
            .known
            .eval_function
            .init(eval_fn.as_tagged(thread.heap()));
        let global = known.global_object.get_handle(thread.heap(), &scope);
        let eval_name = thread.intern(&scope, "eval");
        // Safety: fresh interned word, rooted below before the define.
        let eval_name = scope.handle(eval_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            eval_name,
            PropertyDescriptor::method(eval_fn.erase()),
        )
        .expect("bootstrap define");

        // ---- globalThis -------------------------------------------------------
        let global_this_name = thread.intern(&scope, "globalThis");
        let global_this_name = scope.handle(global_this_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            global_this_name,
            PropertyDescriptor::Data {
                value: global.erase(),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        )
        .expect("bootstrap define");

        // ---- Object.prototype.toString/hasOwnProperty/propertyIsEnumerable ------
        let object_prototype = known.object_prototype.get_handle(thread.heap(), &scope);
        let object_to_string_fn =
            Object::native_function(thread.heap(), &scope, idx.object_to_string, 0);
        Object::set_builtin_name(thread, &scope, object_to_string_fn, "toString");
        state
            .known
            .object_to_string
            .init(object_to_string_fn.as_tagged(thread.heap()));
        let object_to_string_name = known.strings.to_string.get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            object_prototype,
            object_to_string_name,
            PropertyDescriptor::method(object_to_string_fn.erase()),
        )
        .expect("bootstrap define");
        Object::install_method(
            thread,
            &scope,
            object_prototype,
            "hasOwnProperty",
            idx.object_has_own_property,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            object_prototype,
            "propertyIsEnumerable",
            idx.object_property_is_enumerable,
            1,
        );
        {
            let get_fn = Object::native_function(thread.heap(), &scope, idx.object_proto_get, 0);
            let set_fn = Object::native_function(thread.heap(), &scope, idx.object_proto_set, 1);
            let get_fn = scope.handle(get_fn.as_tagged(thread.heap()).erase());
            let set_fn = scope.handle(set_fn.as_tagged(thread.heap()).erase());
            let proto_name = thread.intern(&scope, "__proto__");
            // Safety: fresh interned word, rooted below before the define.
            let proto_name = scope.handle(proto_name.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                object_prototype,
                proto_name,
                PropertyDescriptor::Accessor {
                    get: get_fn,
                    set: set_fn,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("bootstrap define");
        }

        // ---- Object / isNaN -----------------------------------------------------
        // %Object.prototype% already exists from bootstrap; link the
        // constructor to it (like Array below)
        let object_prototype = known.object_prototype.get_handle(thread.heap(), &scope);
        let object_fn = Object::native_constructor(thread.heap(), &scope, idx.object, 1);
        Object::set_builtin_name(thread, &scope, object_fn, "Object");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            object_prototype,
            wks_constructor,
            PropertyDescriptor::method(object_fn.erase()),
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            object_fn,
            wks_prototype,
            PropertyDescriptor::Data {
                value: object_prototype.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");
        let object_name = thread.intern(&scope, "Object");
        // Safety: fresh interned word, rooted below before the define.
        let object_name = scope.handle(object_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            object_name,
            PropertyDescriptor::method(object_fn.erase()),
        )
        .expect("bootstrap define");
        Object::install_method(thread, &scope, object_fn, "create", idx.object_create, 2);
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getPrototypeOf",
            idx.object_get_prototype_of,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getOwnPropertyNames",
            idx.object_get_own_property_names,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getOwnPropertyDescriptor",
            idx.object_get_own_property_descriptor,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "defineProperty",
            idx.object_define_property,
            3,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "defineProperties",
            idx.object_define_properties,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "setPrototypeOf",
            idx.object_set_prototype_of,
            2,
        );

        // ---- Array ---------------------------------------------------------------
        // %Array.prototype% already exists from bootstrap (an array object
        // whose prototype is %Object.prototype%); just link the constructor
        let array_fn = Object::native_constructor(thread.heap(), &scope, idx.array, 1);
        Object::set_builtin_name(thread, &scope, array_fn, "Array");
        let array_prototype = known.array_prototype.get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_prototype,
            wks_constructor,
            PropertyDescriptor::method(array_fn.erase()),
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_fn,
            wks_prototype,
            // ES 23.1.2.5: non-writable, non-enumerable, non-configurable.
            PropertyDescriptor::Data {
                value: array_prototype.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");
        let array_name = thread.intern(&scope, "Array");
        // Safety: fresh interned word, rooted below before the define.
        let array_name = scope.handle(array_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            array_name,
            PropertyDescriptor::method(array_fn.erase()),
        )
        .expect("bootstrap define");

        Object::install_method(thread, &scope, array_prototype, "push", idx.array_push, 1);
        Object::install_method(thread, &scope, array_prototype, "pop", idx.array_pop, 0);
        Object::install_method(thread, &scope, array_prototype, "map", idx.array_map, 1);
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "reduce",
            idx.array_reduce,
            1,
        );
        Object::install_method(thread, &scope, array_prototype, "join", idx.array_join, 1);
        Object::install_method(thread, &scope, array_prototype, "slice", idx.array_slice, 2);
        Object::install_method(thread, &scope, array_prototype, "sort", idx.array_sort, 1);
        Object::install_method(thread, &scope, array_prototype, "at", idx.array_at, 1);
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "includes",
            idx.array_includes,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "indexOf",
            idx.array_index_of,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "lastIndexOf",
            idx.array_last_index_of,
            1,
        );
        Object::install_method(thread, &scope, array_prototype, "fill", idx.array_fill, 1);
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "reverse",
            idx.array_reverse,
            0,
        );
        Object::install_method(thread, &scope, array_prototype, "shift", idx.array_shift, 0);
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "unshift",
            idx.array_unshift,
            1,
        );
        Object::install_method(thread, &scope, array_fn, "of", idx.array_of, 0);
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "toString",
            idx.array_to_string,
            0,
        );

        let zero = scope.handle(Smi::new(0));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_prototype,
            wks_length,
            PropertyDescriptor::Data {
                value: zero,
                writable: true,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");

        let date_fn = Object::native_constructor(thread.heap(), &scope, idx.date, 7);
        Object::set_builtin_name(thread, &scope, date_fn, "Date");
        let date_prototype = {
            let heap = thread.heap();
            let prototype = state.known.date_prototype.get(heap).erase();
            scope
                .cast::<Object>(heap, prototype)
                .expect("date prototype is an object")
        };
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            date_prototype,
            wks_constructor,
            PropertyDescriptor::method(date_fn.erase()),
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            date_fn,
            wks_prototype,
            PropertyDescriptor::Data {
                value: date_prototype.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "valueOf",
            idx.date_value_of,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getTime",
            idx.date_get_time,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getDate",
            idx.date_get_date,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getDay",
            idx.date_get_day,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getFullYear",
            idx.date_get_full_year,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getHours",
            idx.date_get_hours,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMilliseconds",
            idx.date_get_milliseconds,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMinutes",
            idx.date_get_minutes,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMonth",
            idx.date_get_month,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getSeconds",
            idx.date_get_seconds,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getTimezoneOffset",
            idx.date_get_timezone_offset,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCDate",
            idx.date_get_utc_date,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCDay",
            idx.date_get_utc_day,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCFullYear",
            idx.date_get_utc_full_year,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCHours",
            idx.date_get_utc_hours,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMilliseconds",
            idx.date_get_utc_milliseconds,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMinutes",
            idx.date_get_utc_minutes,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMonth",
            idx.date_get_utc_month,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCSeconds",
            idx.date_get_utc_seconds,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getYear",
            idx.date_get_year,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setDate",
            idx.date_set_date,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setFullYear",
            idx.date_set_full_year,
            3,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setHours",
            idx.date_set_hours,
            4,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMilliseconds",
            idx.date_set_milliseconds,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMinutes",
            idx.date_set_minutes,
            3,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMonth",
            idx.date_set_month,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setSeconds",
            idx.date_set_seconds,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setTime",
            idx.date_set_time,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCDate",
            idx.date_set_utc_date,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCFullYear",
            idx.date_set_utc_full_year,
            3,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCHours",
            idx.date_set_utc_hours,
            4,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMilliseconds",
            idx.date_set_utc_milliseconds,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMinutes",
            idx.date_set_utc_minutes,
            3,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMonth",
            idx.date_set_utc_month,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCSeconds",
            idx.date_set_utc_seconds,
            2,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setYear",
            idx.date_set_year,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toString",
            idx.date_to_string,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toISOString",
            idx.date_to_iso_string,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toJSON",
            idx.date_to_json,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toGMTString",
            idx.date_to_gmt_string,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toUTCString",
            idx.date_to_gmt_string,
            0,
        );
        let date_parse_fn = Object::native_function(thread.heap(), &scope, idx.date_parse, 1);
        Object::set_builtin_name(thread, &scope, date_parse_fn, "parse");
        let date_parse_name = thread.intern(&scope, "parse");
        // Safety: fresh interned word, rooted below before the define.
        let date_parse_name = scope.handle(date_parse_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            date_fn,
            date_parse_name,
            PropertyDescriptor::method(date_parse_fn.erase()),
        )
        .expect("bootstrap define");
        let date_now_fn = Object::native_function(thread.heap(), &scope, idx.date_now, 0);
        Object::set_builtin_name(thread, &scope, date_now_fn, "now");
        let date_now_name = thread.intern(&scope, "now");
        // Safety: fresh interned word, rooted below before the define.
        let date_now_name = scope.handle(date_now_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            date_fn,
            date_now_name,
            PropertyDescriptor::method(date_now_fn.erase()),
        )
        .expect("bootstrap define");
        let date_name = thread.intern(&scope, "Date");
        // Safety: fresh interned word, rooted below before the define.
        let date_name = scope.handle(date_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            date_name,
            PropertyDescriptor::method(date_fn.erase()),
        )
        .expect("bootstrap define");

        // Array.isArray
        let is_array_fn = Object::native_function(thread.heap(), &scope, idx.array_is_array, 1);
        Object::set_builtin_name(thread, &scope, is_array_fn, "isArray");
        let is_array_name = thread.intern(&scope, "isArray");
        // Safety: fresh interned word, rooted below before the define.
        let is_array_name = scope.handle(is_array_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_fn,
            is_array_name,
            PropertyDescriptor::method(is_array_fn.erase()),
        )
        .expect("bootstrap define");

        // ---- Array iteration (the iterator protocol minimum) ---------------------
        // %IteratorPrototype%: @@iterator (returns the receiver)
        let iterator_proto_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.erase(),
            },
            &scope,
        );
        let iterator_prototype = scope.handle(thread.heap().new_object(
            &scope,
            iterator_proto_map,
            HandleSlice::EMPTY,
        ));
        let sym_iterator_iter =
            Object::native_function(thread.heap(), &scope, idx.array_iterator_symbol_iterator, 0);
        Object::set_builtin_name(thread, &scope, sym_iterator_iter, "[Symbol.iterator]");
        let iterator_name: Handle<'_, SlotName> =
            scope.handle(state.known.iterator_symbol.get(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            iterator_prototype,
            iterator_name,
            PropertyDescriptor::method(sym_iterator_iter.erase()),
        )
        .expect("bootstrap define");

        // %ArrayIteratorPrototype%: next
        let map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: iterator_prototype.erase(),
            },
            &scope,
        );
        let array_iterator_prototype =
            scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(
            thread,
            &scope,
            array_iterator_prototype,
            "next",
            idx.array_iterator_next,
            0,
        );

        // Array.prototype.values === Array.prototype[Symbol.iterator]: a
        // runtime returning a fresh array-iterator object
        let values_fn = Object::native_function(thread.heap(), &scope, idx.array_values, 0);
        Object::set_builtin_name(thread, &scope, values_fn, "values");
        state
            .known
            .array_values
            .init(values_fn.as_tagged(thread.heap()));
        let values_name = known.strings.values.get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_prototype,
            values_name,
            PropertyDescriptor::method(values_fn.erase()),
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            array_prototype,
            iterator_name,
            PropertyDescriptor::method(values_fn.erase()),
        )
        .expect("bootstrap define");

        // array-iterator map: slots [iterated array, next index, kind],
        // prototype %ArrayIteratorPrototype%
        let array_iterator_map = scope.handle(thread.heap().allocate::<Map>(MapInit {
            kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
            value_slot_count: 3,
            descriptors: &[],
            prototype: array_iterator_prototype.erase(),
        }));

        // iterator-result map: { value, done } both {w+, e+, c+}
        let value_name = known.strings.value.get_handle(thread.heap(), &scope);
        let done_name = state.known.strings.done.get_handle(thread.heap(), &scope);
        let flags = SlotFlags::WRITABLE
            .union(SlotFlags::CONFIGURABLE)
            .union(SlotFlags::ENUMERABLE);
        let proto = object_prototype.erase();
        let iterator_result_map =
            thread
                .heap()
                .allocate_token_enter_heap(Map::layout_for(2), |token, _heap| {
                    let descriptors: [(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>); 2] = [
                        (value_name, flags, scope.handle(Smi::new(0).into_tagged())),
                        (done_name, flags, scope.handle(Smi::new(1).into_tagged())),
                    ];
                    scope.handle(token.allocate::<Map>(MapInit {
                        kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                        value_slot_count: 2,
                        descriptors: &descriptors,
                        prototype: proto,
                    }))
                });

        // for-in enumerator map: slots [level, keys, index, visited],
        // prototype Object.prototype (it must survive `x in Object.prototype`
        // style probes without special cases; the object is never exposed)
        let for_in_enumerator_map = scope.handle(thread.heap().allocate::<Map>(MapInit {
            kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
            value_slot_count: 4,
            descriptors: &[],
            prototype: object_prototype.erase(),
        }));
        {
            let heap = thread.heap();

            let js = &state.known;
            js.array_iterator_map
                .init(array_iterator_map.as_tagged(heap));
            js.array_iterator_prototype
                .init(array_iterator_prototype.as_tagged(heap));
            js.iterator_result_map
                .init(iterator_result_map.as_tagged(heap));
            js.for_in_enumerator_map
                .init(for_in_enumerator_map.as_tagged(heap));
        }

        // ---- Math (namespace object; not callable, not a constructor) --------
        let map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.erase(),
            },
            &scope,
        );
        let math_object = scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(thread, &scope, math_object, "sqrt", idx.math_sqrt, 1);
        Object::install_method(thread, &scope, math_object, "log", idx.math_log, 1);
        Object::install_method(thread, &scope, math_object, "pow", idx.math_pow, 2);
        Object::install_method(thread, &scope, math_object, "abs", idx.math_abs, 1);
        Object::install_method(thread, &scope, math_object, "floor", idx.math_floor, 1);
        Object::install_method(thread, &scope, math_object, "ceil", idx.math_ceil, 1);
        Object::install_method(thread, &scope, math_object, "trunc", idx.math_trunc, 1);
        Object::install_method(thread, &scope, math_object, "round", idx.math_round, 1);
        Object::install_method(thread, &scope, math_object, "min", idx.math_min, 2);
        Object::install_method(thread, &scope, math_object, "max", idx.math_max, 2);
        Object::install_method(thread, &scope, math_object, "random", idx.math_random, 0);
        for (name, value) in [
            ("E", std::f64::consts::E),
            ("LN10", std::f64::consts::LN_10),
            ("LN2", std::f64::consts::LN_2),
            ("LOG2E", std::f64::consts::LOG2_E),
            ("LOG10E", std::f64::consts::LOG10_E),
            ("PI", std::f64::consts::PI),
            ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
            ("SQRT2", std::f64::consts::SQRT_2),
        ] {
            let v = thread.heap().allocate_handle::<Float>(value, &scope);
            let n = thread.intern(&scope, name);
            // Safety: fresh interned word, rooted below before the define.
            let n = scope.handle(n.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                math_object,
                n,
                PropertyDescriptor::Data {
                    value: v.erase(),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            )
            .expect("bootstrap define");
        }
        let math_name = thread.intern(&scope, "Math");
        // Safety: fresh interned word, rooted below before the define.
        let math_name = scope.handle(math_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            math_name,
            PropertyDescriptor::method(math_object.erase()),
        )
        .expect("bootstrap define");

        // ---- console (namespace object; shell convenience, not ES) -----------
        let map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.erase(),
            },
            &scope,
        );
        let console_object =
            scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(thread, &scope, console_object, "log", idx.console_log, 0);
        let console_name = thread.intern(&scope, "console");
        // Safety: fresh interned word, rooted below before the define.
        let console_name = scope.handle(console_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            console_name,
            PropertyDescriptor::method(console_object.erase()),
        )
        .expect("bootstrap define");

        let is_nan_fn = Object::native_function(thread.heap(), &scope, idx.is_nan, 1);
        Object::set_builtin_name(thread, &scope, is_nan_fn, "isNaN");
        let is_nan_name = thread.intern(&scope, "isNaN");
        // Safety: fresh interned word, rooted below before the define.
        let is_nan_name = scope.handle(is_nan_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            is_nan_name,
            PropertyDescriptor::method(is_nan_fn.erase()),
        )
        .expect("bootstrap define");

        let is_finite_fn = Object::native_function(thread.heap(), &scope, idx.is_finite, 1);
        Object::set_builtin_name(thread, &scope, is_finite_fn, "isFinite");
        let is_finite_name = thread.intern(&scope, "isFinite");
        // Safety: fresh interned word, rooted below before the define.
        let is_finite_name = scope.handle(is_finite_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            is_finite_name,
            PropertyDescriptor::method(is_finite_fn.erase()),
        )
        .expect("bootstrap define");

        let parse_float_name = thread.intern(&scope, "parseFloat");
        // Safety: fresh interned word, rooted below before the define.
        let parse_float_name = scope.handle(parse_float_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            parse_float_name,
            PropertyDescriptor::method(parse_float_fn.erase()),
        )
        .expect("bootstrap define");

        let parse_int_name = thread.intern(&scope, "parseInt");
        // Safety: fresh interned word, rooted below before the define.
        let parse_int_name = scope.handle(parse_int_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            parse_int_name,
            PropertyDescriptor::method(parse_int_fn.erase()),
        )
        .expect("bootstrap define");

        // ---- print / performance (shell conveniences; not ES) --------------------
        let print_fn = Object::native_function(thread.heap(), &scope, idx.print, 0);
        let print_name = thread.intern(&scope, "print");
        // Safety: fresh interned word, rooted below before the define.
        let print_name = scope.handle(print_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            print_name,
            PropertyDescriptor::method(print_fn.erase()),
        )
        .expect("bootstrap define");

        let map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.erase(),
            },
            &scope,
        );
        let performance_object =
            scope.handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(
            thread,
            &scope,
            performance_object,
            "now",
            idx.performance_now,
            0,
        );
        let performance_name = thread.intern(&scope, "performance");
        // Safety: fresh interned word, rooted below before the define.
        let performance_name = scope.handle(performance_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            performance_name,
            PropertyDescriptor::method(performance_object.erase()),
        )
        .expect("bootstrap define");

        // ---- Symbol -----------------------------------------------------------
        // `new Symbol()` throws from the constructor body (ES 20.4.1.1 step 1),
        // so the function itself carries [[Construct]].
        let symbol_fn = Object::native_constructor(thread.heap(), &scope, idx.symbol, 0);
        Object::set_builtin_name(thread, &scope, symbol_fn, "Symbol");
        let symbol_name = thread.intern(&scope, "Symbol");
        // Safety: fresh interned word, rooted below before the define.
        let symbol_name = scope.handle(symbol_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            symbol_name,
            PropertyDescriptor::method(symbol_fn.erase()),
        )
        .expect("bootstrap define");

        // %Symbol.prototype% is ordinary; Symbol instances use the wrapper
        // map below with [[SymbolData]] in slot 0.
        let symbol_prototype_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                value_slot_count: 0,
                descriptors: &[],
                prototype: object_prototype.erase(),
            },
            &scope,
        );
        let symbol_prototype = scope.handle(thread.heap().new_object(
            &scope,
            symbol_prototype_map,
            HandleSlice::EMPTY,
        ));
        let symbol_wrapper_map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind: MapKind::OBJECT
                    .union(MapKind::EXTENDABLE)
                    .union(MapKind::PRIMITIVE_WRAPPER),
                value_slot_count: 1,
                descriptors: &[],
                prototype: symbol_prototype.erase(),
            },
            &scope,
        );
        {
            let heap = thread.heap();
            known
                .symbol_prototype
                .init(symbol_prototype.as_tagged(heap));
            known
                .symbol_wrapper_map
                .init(symbol_wrapper_map.as_tagged(heap));
        }
        // Symbol.prototype is non-writable, non-enumerable, non-configurable.
        let prototype_name = known.strings.prototype.get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            symbol_fn,
            prototype_name,
            PropertyDescriptor::Data {
                value: symbol_prototype.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )
        .expect("bootstrap define");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            symbol_prototype,
            wks_constructor,
            PropertyDescriptor::method(symbol_fn.erase()),
        )
        .expect("bootstrap define");

        // Symbol.prototype.description is an accessor; the setter is absent.
        {
            let getter = Object::native_function(thread.heap(), &scope, idx.symbol_description, 0);
            Object::set_builtin_name(thread, &scope, getter, "get description");
            let get = scope.handle(getter.as_tagged(thread.heap()).erase());
            let set = known.undefined.get_handle(thread.heap(), &scope).erase();
            let description_name = thread.intern(&scope, "description");
            // Safety: fresh interned word, rooted below before the define.
            let description_name = scope.handle(description_name.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                symbol_prototype,
                description_name,
                PropertyDescriptor::Accessor {
                    get,
                    set,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("bootstrap define");
        }
        Object::install_method(
            thread,
            &scope,
            symbol_prototype,
            "toString",
            idx.symbol_to_string,
            0,
        );
        Object::install_method(
            thread,
            &scope,
            symbol_prototype,
            "valueOf",
            idx.symbol_value_of,
            0,
        );
        // Symbol.prototype[@@toPrimitive] returns the wrapped symbol.
        {
            let to_primitive =
                Object::native_function(thread.heap(), &scope, idx.symbol_to_primitive, 1);
            Object::set_builtin_name(thread, &scope, to_primitive, "[Symbol.toPrimitive]");
            let key: Handle<'_, SlotName> =
                scope.handle(known.to_primitive_symbol.get(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                symbol_prototype,
                key,
                PropertyDescriptor::Data {
                    value: to_primitive.erase(),
                    writable: false,
                    enumerable: false,
                    configurable: true,
                },
            )
            .expect("bootstrap define");
        }
        Object::install_method(thread, &scope, symbol_fn, "for", idx.symbol_for, 1);
        Object::install_method(thread, &scope, symbol_fn, "keyFor", idx.symbol_key_for, 1);

        // The engine's @@iterator/@@toPrimitive/@@toStringTag are the
        // well-known symbols; the other well-known symbols are only ever
        // observed by user code.
        for (name, description) in [
            ("asyncDispose", "Symbol.asyncDispose"),
            ("asyncIterator", "Symbol.asyncIterator"),
            ("dispose", "Symbol.dispose"),
            ("hasInstance", "Symbol.hasInstance"),
            ("isConcatSpreadable", "Symbol.isConcatSpreadable"),
            ("iterator", "Symbol.iterator"),
            ("match", "Symbol.match"),
            ("matchAll", "Symbol.matchAll"),
            ("replace", "Symbol.replace"),
            ("search", "Symbol.search"),
            ("species", "Symbol.species"),
            ("split", "Symbol.split"),
            ("toPrimitive", "Symbol.toPrimitive"),
            ("toStringTag", "Symbol.toStringTag"),
            ("unscopables", "Symbol.unscopables"),
        ] {
            let value = match name {
                "iterator" => state
                    .known
                    .iterator_symbol
                    .get_handle(thread.heap(), &scope)
                    .erase(),
                "toPrimitive" => known
                    .to_primitive_symbol
                    .get_handle(thread.heap(), &scope)
                    .erase(),
                "toStringTag" => {
                    scope.handle(state.known.to_string_tag_symbol.get(thread.heap()).erase())
                }
                _ => {
                    let description = thread.intern(&scope, description);
                    // Safety: fresh interned word, rooted below before the
                    // symbol allocation.
                    let description: Handle<'_, DenseString> =
                        scope.handle(description.as_tagged(thread.heap()));
                    Symbol::new(thread.heap(), &scope, Some(description)).erase()
                }
            };
            let name = thread.intern(&scope, name);
            // Safety: fresh interned word, rooted below before the define.
            let name = scope.handle(name.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                symbol_fn,
                name,
                PropertyDescriptor::Data {
                    value,
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            )
            .expect("bootstrap define");
        }
        // Symbol.prototype[@@toStringTag] = "Symbol".
        let to_string_tag_key: Handle<'_, SlotName> =
            scope.handle(state.known.to_string_tag_symbol.get(thread.heap()));
        let symbol_tag = thread.intern(&scope, "Symbol");
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            symbol_prototype,
            to_string_tag_key,
            PropertyDescriptor::Data {
                value: symbol_tag.erase(),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        )
        .expect("bootstrap define");

        // ---- Proxy ------------------------------------------------------------
        // The Proxy constructor is a runtime function *without* a
        // `.prototype` property (ES 20.2.1: "Proxy.prototype is
        // undefined"); `install_constructor` cannot be used.
        let proxy_fn = Object::native_constructor(thread.heap(), &scope, idx.proxy, 2);
        let two = scope.handle(Smi::new(2));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            proxy_fn,
            wks_length,
            PropertyDescriptor::non_enumerable(two),
        )
        .expect("bootstrap define");
        Object::set_builtin_name(thread, &scope, proxy_fn, "Proxy");
        let proxy_name = thread.intern(&scope, "Proxy");
        // Safety: fresh interned word, rooted below before the define.
        let proxy_name = scope.handle(proxy_name.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            global,
            proxy_name,
            PropertyDescriptor::method(proxy_fn.erase()),
        )
        .expect("bootstrap define");
        // Proxy.revocable: a non-constructor function returning
        // { proxy, revoke }; the revoke closure is the JS template
        // installed by REVOKE_PRELUDE (natives cannot carry state).
        let revocable_fn = Object::native_function(thread.heap(), &scope, idx.proxy_revocable, 2);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            revocable_fn,
            wks_length,
            PropertyDescriptor::non_enumerable(two),
        )
        .expect("bootstrap define");
        Object::set_builtin_name(thread, &scope, revocable_fn, "revocable");
        let revocable_name = state
            .known
            .strings
            .revocable
            .get_handle(thread.heap(), &scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            &scope,
            proxy_fn,
            revocable_name,
            PropertyDescriptor::method(revocable_fn.erase()),
        )
        .expect("bootstrap define");
        // ---- Object extensibility statics --------------------------------------
        Object::install_method(thread, &scope, object_fn, "is", idx.object_is, 2);
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "preventExtensions",
            idx.object_prevent_extensions,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "isExtensible",
            idx.object_is_extensible,
            1,
        );
        Object::install_method(thread, &scope, object_fn, "seal", idx.object_seal, 1);
        Object::install_method(thread, &scope, object_fn, "freeze", idx.object_freeze, 1);
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "isFrozen",
            idx.object_is_frozen,
            1,
        );
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "isSealed",
            idx.object_is_sealed,
            1,
        );

        // ---- value properties of the global object -----------------------------
        let infinity = thread
            .heap()
            .allocate_handle::<Float>(f64::INFINITY, &scope);
        let nan = thread.heap().allocate_handle::<Float>(f64::NAN, &scope);
        let undefined = known.undefined.get_handle(thread.heap(), &scope);
        for name in ["Infinity", "NaN", "undefined"] {
            let n = thread.intern(&scope, name);
            // spec attributes are {writable: false, enumerable: false,
            // configurable: false} (ES 19.1.1); non-configurability is
            // what `delete NaN` observes, writable/enumerable stay per
            //missive until non-writable stores stop throwing in sloppy
            // code
            let value = match name {
                "Infinity" => infinity.erase(),
                "NaN" => nan.erase(),
                _ => undefined.erase(),
            };
            let receiver = global;
            let key = scope.handle(n.as_tagged(thread.heap()));
            Object::define_own_property_ordinary(
                thread.heap(),
                &scope,
                receiver,
                key,
                PropertyDescriptor::Data {
                    value,
                    writable: true,
                    enumerable: true,
                    configurable: false,
                },
            )
            .expect("bootstrap define");
        }
    })
}
