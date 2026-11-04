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

use array::{
    array_constructor, array_is_array, array_iterator_next, array_iterator_symbol_iterator,
    array_join, array_map, array_pop, array_push, array_reduce, array_slice, array_sort,
    array_to_string, array_values,
};
use boolean::{boolean_constructor, boolean_to_string, boolean_value_of};
use date::{
    date_constructor, date_get_date, date_get_day, date_get_full_year, date_get_hours,
    date_get_milliseconds, date_get_minutes, date_get_month, date_get_seconds, date_get_time,
    date_get_timezone_offset, date_get_utc_date, date_get_utc_day, date_get_utc_full_year,
    date_get_utc_hours, date_get_utc_milliseconds, date_get_utc_minutes, date_get_utc_month,
    date_get_utc_seconds, date_get_year, date_now, date_parse, date_set_date, date_set_full_year,
    date_set_hours, date_set_milliseconds, date_set_minutes, date_set_month, date_set_seconds,
    date_set_time, date_set_utc_date, date_set_utc_full_year, date_set_utc_hours,
    date_set_utc_milliseconds, date_set_utc_minutes, date_set_utc_month, date_set_utc_seconds,
    date_set_year, date_to_gmt_string, date_to_iso_string, date_to_json, date_to_string,
    date_value_of,
};
use error::{
    error_constructor, error_is_error, error_to_string, eval_error_constructor,
    range_error_constructor, reference_error_constructor, syntax_error_constructor,
    type_error_constructor, uri_error_constructor,
};
use function::{
    function_apply, function_bind, function_call, function_constructor, function_to_string,
};
use global::{console_log, eval_runtime, is_nan, performance_now, print};
use js_compiler::native::NativeIds;
use math::{
    math_abs, math_ceil, math_floor, math_log, math_max, math_min, math_pow, math_random,
    math_round, math_sqrt, math_trunc,
};
use number::{
    number_constructor, number_to_fixed, number_to_precision, number_to_string, number_value_of,
};
use object::{
    object_constructor, object_create, object_define_property, object_freeze,
    object_get_own_property_descriptor, object_get_own_property_names, object_get_prototype_of,
    object_has_own_property, object_is, object_is_extensible, object_prevent_extensions,
    object_property_is_enumerable, object_proto_get, object_proto_set, object_seal,
    object_set_prototype_of, object_to_string,
};
use proxy::{proxy_constructor, proxy_revocable, proxy_revoke};
use string::{string_constructor, string_to_string, string_value_of};
use symbol::{
    symbol_constructor, symbol_for, symbol_key_for, symbol_prototype_description,
    symbol_prototype_to_primitive, symbol_prototype_to_string, symbol_prototype_value_of,
};

use vm_core::{
    DenseString, EdgeVisitable, Float, Handle, HandleScope, HandleSlice, Map, MapInit, MapKind,
    NativeIndex, Object, PropertyDescriptor, Runtime, RuntimeId, SlotFlags, SlotName, Smi, Symbol,
    Tagged, Thread, VM, Value, Visitor, VmError,
};

mod known;
mod native;

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

    fn setup(vm: &mut VM, state: &mut Self) -> Result<(), VmError> {
        state.ids = native::register(vm);
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
        install_builtins(vm, state)
    }
}

impl Default for JSRuntime {
    fn default() -> Self {
        Self {
            indices: BuiltinIndices::default(),
            known: known::JsKnown::default(),
            ids: NativeIds {
                get_iterator: NativeIndex(0),
                iterator_next: NativeIndex(0),
                iterator_done: NativeIndex(0),
                iterator_value: NativeIndex(0),
                has_property: NativeIndex(0),
                copy_data_properties: NativeIndex(0),
                create_private_name: NativeIndex(0),
                private_get: NativeIndex(0),
                private_set: NativeIndex(0),
                private_in: NativeIndex(0),
                set_class_fields: NativeIndex(0),
                init_instance_fields: NativeIndex(0),
                require_object_coercible: NativeIndex(0),
                delete_property_operator: NativeIndex(0),
                delete_identifier_sloppy: NativeIndex(0),
                delete_super_property: NativeIndex(0),
                for_in_enumerate: NativeIndex(0),
                for_in_next: NativeIndex(0),
                set_function_name: NativeIndex(0),
                install_accessor: NativeIndex(0),
                define_own_property: NativeIndex(0),
                set_prototype: NativeIndex(0),
                throw_if_not_constructor_or_null: NativeIndex(0),
                throw_if_not_object_or_null: NativeIndex(0),
                throw_super_not_called_if_hole: NativeIndex(0),
                throw_super_already_called_if_not_hole: NativeIndex(0),
                construct_super: NativeIndex(0),
                construct_super_all_args: NativeIndex(0),
                construct_super_via: NativeIndex(0),
                load_dynamic_name: NativeIndex(0),
                store_dynamic_name: NativeIndex(0),
                declare_eval_var: NativeIndex(0),
                declare_eval_function: NativeIndex(0),
                resolve_direct_eval: NativeIndex(0),
                store_repl_global_init: NativeIndex(0),
                validate_eval_declarations: NativeIndex(0),
                declare_globals: NativeIndex(0),
                throw_const_assign: NativeIndex(0),
                create_rest_parameter: NativeIndex(0),
                super_get_property: NativeIndex(0),
                super_set_property: NativeIndex(0),
            },
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
    BuiltinIndices {
        eval: vm.register_native(eval_runtime),
        string: vm.register_native(string_constructor),
        string_value_of: vm.register_native(string_value_of),
        string_to_string: vm.register_native(string_to_string),
        reference_error: vm.register_native(reference_error_constructor),
        range_error: vm.register_native(range_error_constructor),
        syntax_error: vm.register_native(syntax_error_constructor),
        eval_error: vm.register_native(eval_error_constructor),
        uri_error: vm.register_native(uri_error_constructor),
        function_to_string: vm.register_native(function_to_string),
        object_to_string: vm.register_native(object_to_string),
        number: vm.register_native(number_constructor),
        number_value_of: vm.register_native(number_value_of),
        number_to_string: vm.register_native(number_to_string),
        boolean: vm.register_native(boolean_constructor),
        boolean_value_of: vm.register_native(boolean_value_of),
        boolean_to_string: vm.register_native(boolean_to_string),
        error: vm.register_native(error_constructor),
        type_error: vm.register_native(type_error_constructor),
        error_is_error: vm.register_native(error_is_error),
        error_to_string: vm.register_native(error_to_string),
        object: vm.register_native(object_constructor),
        object_create: vm.register_native(object_create),
        object_get_prototype_of: vm.register_native(object_get_prototype_of),
        object_set_prototype_of: vm.register_native(object_set_prototype_of),
        array: vm.register_native(array_constructor),
        is_nan: vm.register_native(is_nan),
        array_values: vm.register_native(array_values),
        array_push: vm.register_native(array_push),
        array_pop: vm.register_native(array_pop),
        array_map: vm.register_native(array_map),
        array_reduce: vm.register_native(array_reduce),
        math_log: vm.register_native(math_log),
        math_pow: vm.register_native(math_pow),
        number_to_fixed: vm.register_native(number_to_fixed),
        number_to_precision: vm.register_native(number_to_precision),
        date: vm.register_native(date_constructor),
        date_now: vm.register_native(date_now),
        date_value_of: vm.register_native(date_value_of),
        date_get_time: vm.register_native(date_get_time),
        date_get_date: vm.register_native(date_get_date),
        date_get_day: vm.register_native(date_get_day),
        date_get_full_year: vm.register_native(date_get_full_year),
        date_get_hours: vm.register_native(date_get_hours),
        date_get_milliseconds: vm.register_native(date_get_milliseconds),
        date_get_minutes: vm.register_native(date_get_minutes),
        date_get_month: vm.register_native(date_get_month),
        date_get_seconds: vm.register_native(date_get_seconds),
        date_get_timezone_offset: vm.register_native(date_get_timezone_offset),
        date_get_utc_date: vm.register_native(date_get_utc_date),
        date_get_utc_day: vm.register_native(date_get_utc_day),
        date_get_utc_full_year: vm.register_native(date_get_utc_full_year),
        date_get_utc_hours: vm.register_native(date_get_utc_hours),
        date_get_utc_milliseconds: vm.register_native(date_get_utc_milliseconds),
        date_get_utc_minutes: vm.register_native(date_get_utc_minutes),
        date_get_utc_month: vm.register_native(date_get_utc_month),
        date_get_utc_seconds: vm.register_native(date_get_utc_seconds),
        date_get_year: vm.register_native(date_get_year),
        date_set_date: vm.register_native(date_set_date),
        date_set_full_year: vm.register_native(date_set_full_year),
        date_set_hours: vm.register_native(date_set_hours),
        date_set_milliseconds: vm.register_native(date_set_milliseconds),
        date_set_minutes: vm.register_native(date_set_minutes),
        date_set_month: vm.register_native(date_set_month),
        date_set_seconds: vm.register_native(date_set_seconds),
        date_set_time: vm.register_native(date_set_time),
        date_set_utc_date: vm.register_native(date_set_utc_date),
        date_set_utc_full_year: vm.register_native(date_set_utc_full_year),
        date_set_utc_hours: vm.register_native(date_set_utc_hours),
        date_set_utc_milliseconds: vm.register_native(date_set_utc_milliseconds),
        date_set_utc_minutes: vm.register_native(date_set_utc_minutes),
        date_set_utc_month: vm.register_native(date_set_utc_month),
        date_set_utc_seconds: vm.register_native(date_set_utc_seconds),
        date_set_year: vm.register_native(date_set_year),
        date_to_string: vm.register_native(date_to_string),
        date_to_iso_string: vm.register_native(date_to_iso_string),
        date_to_json: vm.register_native(date_to_json),
        date_to_gmt_string: vm.register_native(date_to_gmt_string),
        date_parse: vm.register_native(date_parse),
        print: vm.register_native(print),
        performance_now: vm.register_native(performance_now),
        array_iterator_next: vm.register_native(array_iterator_next),
        array_iterator_symbol_iterator: vm.register_native(array_iterator_symbol_iterator),
        symbol: vm.register_native(symbol_constructor),
        symbol_description: vm.register_native(symbol_prototype_description),
        symbol_to_string: vm.register_native(symbol_prototype_to_string),
        symbol_value_of: vm.register_native(symbol_prototype_value_of),
        symbol_to_primitive: vm.register_native(symbol_prototype_to_primitive),
        symbol_for: vm.register_native(symbol_for),
        symbol_key_for: vm.register_native(symbol_key_for),
        object_has_own_property: vm.register_native(object_has_own_property),
        object_is: vm.register_native(object_is),
        object_property_is_enumerable: vm.register_native(object_property_is_enumerable),
        object_proto_get: vm.register_native(object_proto_get),
        object_proto_set: vm.register_native(object_proto_set),
        object_get_own_property_names: vm.register_native(object_get_own_property_names),
        object_get_own_property_descriptor: vm.register_native(object_get_own_property_descriptor),
        object_define_property: vm.register_native(object_define_property),
        function_bind: vm.register_native(function_bind),
        function_call: vm.register_native(function_call),
        function_apply: vm.register_native(function_apply),
        function_constructor: vm.register_native(function_constructor),
        array_is_array: vm.register_native(array_is_array),
        proxy: vm.register_native(proxy_constructor),
        proxy_revocable: vm.register_native(proxy_revocable),
        proxy_revoke: vm.register_native(proxy_revoke),
        object_prevent_extensions: vm.register_native(object_prevent_extensions),
        object_is_extensible: vm.register_native(object_is_extensible),
        object_seal: vm.register_native(object_seal),
        object_freeze: vm.register_native(object_freeze),
        math_sqrt: vm.register_native(math_sqrt),
        math_abs: vm.register_native(math_abs),
        math_floor: vm.register_native(math_floor),
        math_ceil: vm.register_native(math_ceil),
        math_trunc: vm.register_native(math_trunc),
        math_round: vm.register_native(math_round),
        math_min: vm.register_native(math_min),
        math_max: vm.register_native(math_max),
        math_random: vm.register_native(math_random),
        console_log: vm.register_native(console_log),
        array_join: vm.register_native(array_join),
        array_to_string: vm.register_native(array_to_string),
        array_slice: vm.register_native(array_slice),
        array_sort: vm.register_native(array_sort),
    }
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
    pub number_value_of: NativeIndex,
    pub number_to_string: NativeIndex,
    pub boolean: NativeIndex,
    pub boolean_value_of: NativeIndex,
    pub boolean_to_string: NativeIndex,
    pub error: NativeIndex,
    pub type_error: NativeIndex,
    pub error_is_error: NativeIndex,
    pub error_to_string: NativeIndex,
    pub object: NativeIndex,
    pub object_create: NativeIndex,
    pub object_get_prototype_of: NativeIndex,
    pub object_set_prototype_of: NativeIndex,
    pub array: NativeIndex,
    pub is_nan: NativeIndex,
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
}

/// Install one NativeError class: constructor, prototype, and [[ErrorData]] map.
fn install_error_class<'s>(
    thread: &mut Thread,
    scope: &'s HandleScope<'_>,
    index: NativeIndex,
    class: &str,
    parent: Handle<'_, Object>,
) -> Result<(Handle<'s, Object>, Handle<'s, Object>, Handle<'s, Map>), VmError> {
    let (constructor, prototype) =
        Object::install_constructor(thread, scope, index, class, parent)?;
    let name = thread.intern(scope, class);
    // Safety: fresh interned word, rooted below before the define.
    let name: Handle<'_, DenseString> = scope.handle(name.as_tagged(thread.heap()));
    let name_key = thread
        .heap()
        .known()
        .strings
        .name
        .get_handle(thread.heap(), &scope);
    Object::define_own_property(
        thread.heap(),
        scope,
        prototype,
        name_key,
        PropertyDescriptor::method(name.erase()),
    )?;
    let message_key = thread
        .heap()
        .known()
        .strings
        .message
        .get_handle(thread.heap(), &scope);
    let empty = scope.handle(
        thread
            .heap()
            .known()
            .strings
            .empty
            .get(thread.heap())
            .erase(),
    );
    Object::define_own_property(
        thread.heap(),
        scope,
        prototype,
        message_key,
        PropertyDescriptor::method(empty),
    )?;
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
    Ok((constructor, prototype, map))
}

/// Build the builtin objects and install them on the global object.
pub fn install_builtins(vm: &mut VM, state: &mut JSRuntime) -> Result<(), VmError> {
    let idx = &state.indices;
    let known = vm.known();
    let mut thread = vm.attach();
    let wks = &known.strings;
    let roots = vm.roots();
    thread.handle_scope(|thread, scope| {
        let heap = thread.heap();
        let wks_empty: Handle<'_, Value> = scope.handle(wks.empty.get(heap).erase());
        let wks_length: Handle<'_, SlotName> = scope.handle(wks.length.get(heap));
        let wks_constructor: Handle<'_, SlotName> = scope.handle(wks.constructor.get(heap));
        let wks_prototype: Handle<'_, SlotName> = scope.handle(wks.prototype.get(heap));

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
        )?;
        Object::install_method(thread, &scope, number_proto, "valueOf", idx.number_value_of)?;
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toString",
            idx.number_to_string,
        )?;
        Object::install_method(thread, &scope, number_proto, "toFixed", idx.number_to_fixed)?;
        Object::install_method(
            thread,
            &scope,
            number_proto,
            "toPrecision",
            idx.number_to_precision,
        )?;
        // static data properties on the Number constructor
        let pos_inf = thread
            .heap()
            .allocate_handle::<Float>(f64::INFINITY, &scope);
        let neg_inf = thread
            .heap()
            .allocate_handle::<Float>(f64::NEG_INFINITY, &scope);
        let max_value = thread.heap().allocate_handle::<Float>(f64::MAX, &scope);
        let min_value = thread
            .heap()
            .allocate_handle::<Float>(f64::MIN_POSITIVE, &scope);
        let number_nan = thread.heap().allocate_handle::<Float>(f64::NAN, &scope);
        for (name, value) in [
            ("POSITIVE_INFINITY", pos_inf),
            ("NEGATIVE_INFINITY", neg_inf),
            ("MAX_VALUE", max_value),
            ("MIN_VALUE", min_value),
            ("NaN", number_nan),
        ] {
            let n = thread.intern(&scope, name);
            // Safety: fresh interned word, rooted below before the define.
            let n = scope.handle(n.as_tagged(thread.heap()));
            Object::define_own_property(
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
            )?;
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
        )?;
        Object::install_method(
            thread,
            &scope,
            boolean_proto,
            "valueOf",
            idx.boolean_value_of,
        )?;
        Object::install_method(
            thread,
            &scope,
            boolean_proto,
            "toString",
            idx.boolean_to_string,
        )?;
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
        )?;
        Object::install_method(thread, &scope, string_proto, "valueOf", idx.string_value_of)?;
        Object::install_method(
            thread,
            &scope,
            string_proto,
            "toString",
            idx.string_to_string,
        )?;
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
            install_error_class(thread, &scope, idx.error, "Error", object_prototype)?;
        known.error_map.init(error_map.as_tagged(thread.heap()));
        Object::install_method(thread, &scope, error_proto, "toString", idx.error_to_string)?;

        let (_, _, type_error_map) =
            install_error_class(thread, &scope, idx.type_error, "TypeError", error_proto)?;
        known
            .type_error_map
            .init(type_error_map.as_tagged(thread.heap()));

        let (_, _, reference_error_map) = install_error_class(
            thread,
            &scope,
            idx.reference_error,
            "ReferenceError",
            error_proto,
        )?;
        known
            .reference_error_map
            .init(reference_error_map.as_tagged(thread.heap()));

        let (_, _, range_error_map) =
            install_error_class(thread, &scope, idx.range_error, "RangeError", error_proto)?;
        known
            .range_error_map
            .init(range_error_map.as_tagged(thread.heap()));

        let (_, _, syntax_error_map) =
            install_error_class(thread, &scope, idx.syntax_error, "SyntaxError", error_proto)?;
        known
            .syntax_error_map
            .init(syntax_error_map.as_tagged(thread.heap()));

        let (_, _, eval_error_map) =
            install_error_class(thread, &scope, idx.eval_error, "EvalError", error_proto)?;
        known
            .eval_error_map
            .init(eval_error_map.as_tagged(thread.heap()));

        let (_, _, uri_error_map) =
            install_error_class(thread, &scope, idx.uri_error, "URIError", error_proto)?;
        known
            .uri_error_map
            .init(uri_error_map.as_tagged(thread.heap()));

        Object::install_method(thread, &scope, error_fn, "isError", idx.error_is_error)?;

        // ---- Function (constructor: dynamic bodies via eval, ES 20.2.1) --------
        let function_prototype = known.function_prototype.get_handle(thread.heap(), &scope);
        let function_fn =
            Object::native_constructor(thread.heap(), &scope, idx.function_constructor)?;
        Object::set_builtin_name(thread, &scope, function_fn, "Function")?;
        Object::define_own_property(
            thread.heap(),
            &scope,
            function_prototype,
            wks_constructor,
            PropertyDescriptor::method(function_fn.erase()),
        )?;
        Object::define_own_property(
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
        )?;
        let function_name = thread.intern(&scope, "Function");
        // Safety: fresh interned word, rooted below before the define.
        let function_name = scope.handle(function_name.as_tagged(thread.heap()));
        let global_object = known.global_object.get_handle(thread.heap(), &scope);
        Object::define_own_property(
            thread.heap(),
            &scope,
            global_object,
            function_name,
            PropertyDescriptor::method(function_fn.erase()),
        )?;

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
        )?;
        // call/apply are plain (non-constructor) runtime functions
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "call",
            idx.function_call,
        )?;
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "apply",
            idx.function_apply,
        )?;
        Object::install_method(
            thread,
            &scope,
            function_prototype,
            "bind",
            idx.function_bind,
        )?;
        // ---- eval -------------------------------------------------------------
        let eval_fn = Object::native_function(thread.heap(), &scope, idx.eval)?;
        Object::set_builtin_name(thread, &scope, eval_fn, "eval")?;
        state
            .known
            .eval_function
            .init(eval_fn.as_tagged(thread.heap()));
        let global = known.global_object.get_handle(thread.heap(), &scope);
        let eval_name = thread.intern(&scope, "eval");
        // Safety: fresh interned word, rooted below before the define.
        let eval_name = scope.handle(eval_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            eval_name,
            PropertyDescriptor::method(eval_fn.erase()),
        )?;

        // ---- globalThis -------------------------------------------------------
        let global_this_name = thread.intern(&scope, "globalThis");
        let global_this_name = scope.handle(global_this_name.as_tagged(thread.heap()));
        Object::define_own_property(
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
        )?;

        // ---- Object.prototype.toString/hasOwnProperty/propertyIsEnumerable ------
        let object_prototype = known.object_prototype.get_handle(thread.heap(), &scope);
        let object_to_string_fn =
            Object::native_function(thread.heap(), &scope, idx.object_to_string)?;
        state
            .known
            .object_to_string
            .init(object_to_string_fn.as_tagged(thread.heap()));
        let object_to_string_name = known.strings.to_string.get_handle(thread.heap(), &scope);
        Object::define_own_property(
            thread.heap(),
            &scope,
            object_prototype,
            object_to_string_name,
            PropertyDescriptor::method(object_to_string_fn.erase()),
        )?;
        Object::install_method(
            thread,
            &scope,
            object_prototype,
            "hasOwnProperty",
            idx.object_has_own_property,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_prototype,
            "propertyIsEnumerable",
            idx.object_property_is_enumerable,
        )?;
        {
            let get_fn = Object::native_function(thread.heap(), &scope, idx.object_proto_get)?;
            let set_fn = Object::native_function(thread.heap(), &scope, idx.object_proto_set)?;
            let get_fn = scope.handle(get_fn.as_tagged(thread.heap()).erase());
            let set_fn = scope.handle(set_fn.as_tagged(thread.heap()).erase());
            let proto_name = thread.intern(&scope, "__proto__");
            // Safety: fresh interned word, rooted below before the define.
            let proto_name = scope.handle(proto_name.as_tagged(thread.heap()));
            Object::define_own_property(
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
            )?;
        }

        // ---- Object / isNaN -----------------------------------------------------
        // %Object.prototype% already exists from bootstrap; link the
        // constructor to it (like Array below)
        let object_prototype = known.object_prototype.get_handle(thread.heap(), &scope);
        let object_fn = Object::native_constructor(thread.heap(), &scope, idx.object)?;
        Object::set_builtin_name(thread, &scope, object_fn, "Object")?;
        Object::define_own_property(
            thread.heap(),
            &scope,
            object_prototype,
            wks_constructor,
            PropertyDescriptor::method(object_fn.erase()),
        )?;
        Object::define_own_property(
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
        )?;
        let object_name = thread.intern(&scope, "Object");
        // Safety: fresh interned word, rooted below before the define.
        let object_name = scope.handle(object_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            object_name,
            PropertyDescriptor::method(object_fn.erase()),
        )?;
        Object::install_method(thread, &scope, object_fn, "create", idx.object_create)?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getPrototypeOf",
            idx.object_get_prototype_of,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getOwnPropertyNames",
            idx.object_get_own_property_names,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "getOwnPropertyDescriptor",
            idx.object_get_own_property_descriptor,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "defineProperty",
            idx.object_define_property,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "setPrototypeOf",
            idx.object_set_prototype_of,
        )?;

        // ---- Array ---------------------------------------------------------------
        // %Array.prototype% already exists from bootstrap (an array object
        // whose prototype is %Object.prototype%); just link the constructor
        let array_fn = Object::native_constructor(thread.heap(), &scope, idx.array)?;
        Object::set_builtin_name(thread, &scope, array_fn, "Array")?;
        let array_prototype = known.array_prototype.get_handle(thread.heap(), &scope);
        Object::define_own_property(
            thread.heap(),
            &scope,
            array_prototype,
            wks_constructor,
            PropertyDescriptor::method(array_fn.erase()),
        )?;
        Object::define_own_property(
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
        )?;
        let array_name = thread.intern(&scope, "Array");
        // Safety: fresh interned word, rooted below before the define.
        let array_name = scope.handle(array_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            array_name,
            PropertyDescriptor::method(array_fn.erase()),
        )?;

        Object::install_method(thread, &scope, array_prototype, "push", idx.array_push)?;
        Object::install_method(thread, &scope, array_prototype, "pop", idx.array_pop)?;
        Object::install_method(thread, &scope, array_prototype, "map", idx.array_map)?;
        Object::install_method(thread, &scope, array_prototype, "reduce", idx.array_reduce)?;
        Object::install_method(thread, &scope, array_prototype, "join", idx.array_join)?;
        Object::install_method(thread, &scope, array_prototype, "slice", idx.array_slice)?;
        Object::install_method(thread, &scope, array_prototype, "sort", idx.array_sort)?;
        Object::install_method(
            thread,
            &scope,
            array_prototype,
            "toString",
            idx.array_to_string,
        )?;

        let zero = scope.handle(Smi::new(0));
        Object::define_own_property(
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
        )?;

        let date_fn = Object::native_constructor(thread.heap(), &scope, idx.date)?;
        Object::set_builtin_name(thread, &scope, date_fn, "Date")?;
        let date_prototype = {
            let heap = thread.heap();
            let prototype = state.known.date_prototype.get(heap).erase();
            scope
                .cast::<Object>(heap, prototype)
                .expect("date prototype is an object")
        };
        Object::define_own_property(
            thread.heap(),
            &scope,
            date_prototype,
            wks_constructor,
            PropertyDescriptor::method(date_fn.erase()),
        )?;
        Object::define_own_property(
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
        )?;
        Object::install_method(thread, &scope, date_prototype, "valueOf", idx.date_value_of)?;
        Object::install_method(thread, &scope, date_prototype, "getTime", idx.date_get_time)?;
        Object::install_method(thread, &scope, date_prototype, "getDate", idx.date_get_date)?;
        Object::install_method(thread, &scope, date_prototype, "getDay", idx.date_get_day)?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getFullYear",
            idx.date_get_full_year,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getHours",
            idx.date_get_hours,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMilliseconds",
            idx.date_get_milliseconds,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMinutes",
            idx.date_get_minutes,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getMonth",
            idx.date_get_month,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getSeconds",
            idx.date_get_seconds,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getTimezoneOffset",
            idx.date_get_timezone_offset,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCDate",
            idx.date_get_utc_date,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCDay",
            idx.date_get_utc_day,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCFullYear",
            idx.date_get_utc_full_year,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCHours",
            idx.date_get_utc_hours,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMilliseconds",
            idx.date_get_utc_milliseconds,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMinutes",
            idx.date_get_utc_minutes,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCMonth",
            idx.date_get_utc_month,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "getUTCSeconds",
            idx.date_get_utc_seconds,
        )?;
        Object::install_method(thread, &scope, date_prototype, "getYear", idx.date_get_year)?;
        Object::install_method(thread, &scope, date_prototype, "setDate", idx.date_set_date)?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setFullYear",
            idx.date_set_full_year,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setHours",
            idx.date_set_hours,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMilliseconds",
            idx.date_set_milliseconds,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMinutes",
            idx.date_set_minutes,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setMonth",
            idx.date_set_month,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setSeconds",
            idx.date_set_seconds,
        )?;
        Object::install_method(thread, &scope, date_prototype, "setTime", idx.date_set_time)?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCDate",
            idx.date_set_utc_date,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCFullYear",
            idx.date_set_utc_full_year,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCHours",
            idx.date_set_utc_hours,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMilliseconds",
            idx.date_set_utc_milliseconds,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMinutes",
            idx.date_set_utc_minutes,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCMonth",
            idx.date_set_utc_month,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "setUTCSeconds",
            idx.date_set_utc_seconds,
        )?;
        Object::install_method(thread, &scope, date_prototype, "setYear", idx.date_set_year)?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toString",
            idx.date_to_string,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toISOString",
            idx.date_to_iso_string,
        )?;
        Object::install_method(thread, &scope, date_prototype, "toJSON", idx.date_to_json)?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toGMTString",
            idx.date_to_gmt_string,
        )?;
        Object::install_method(
            thread,
            &scope,
            date_prototype,
            "toUTCString",
            idx.date_to_gmt_string,
        )?;
        let date_parse_fn = Object::native_function(thread.heap(), &scope, idx.date_parse)?;
        Object::set_builtin_name(thread, &scope, date_parse_fn, "parse")?;
        let date_parse_name = thread.intern(&scope, "parse");
        // Safety: fresh interned word, rooted below before the define.
        let date_parse_name = scope.handle(date_parse_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            date_fn,
            date_parse_name,
            PropertyDescriptor::method(date_parse_fn.erase()),
        )?;
        let date_now_fn = Object::native_function(thread.heap(), &scope, idx.date_now)?;
        Object::set_builtin_name(thread, &scope, date_now_fn, "now")?;
        let date_now_name = thread.intern(&scope, "now");
        // Safety: fresh interned word, rooted below before the define.
        let date_now_name = scope.handle(date_now_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            date_fn,
            date_now_name,
            PropertyDescriptor::method(date_now_fn.erase()),
        )?;
        let date_name = thread.intern(&scope, "Date");
        // Safety: fresh interned word, rooted below before the define.
        let date_name = scope.handle(date_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            date_name,
            PropertyDescriptor::method(date_fn.erase()),
        )?;

        // Array.isArray
        let is_array_fn = Object::native_function(thread.heap(), &scope, idx.array_is_array)?;
        Object::set_builtin_name(thread, &scope, is_array_fn, "isArray")?;
        let is_array_name = thread.intern(&scope, "isArray");
        // Safety: fresh interned word, rooted below before the define.
        let is_array_name = scope.handle(is_array_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            array_fn,
            is_array_name,
            PropertyDescriptor::method(is_array_fn.erase()),
        )?;

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
            Object::native_function(thread.heap(), &scope, idx.array_iterator_symbol_iterator)?;
        let iterator_name: Handle<'_, SlotName> =
            scope.handle(state.known.iterator_symbol.get(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            iterator_prototype,
            iterator_name,
            PropertyDescriptor::method(sym_iterator_iter.erase()),
        )?;

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
            roots.create_handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(
            thread,
            &scope,
            array_iterator_prototype,
            "next",
            idx.array_iterator_next,
        )?;

        // Array.prototype.values === Array.prototype[Symbol.iterator]: a
        // runtime returning a fresh array-iterator object
        let values_fn = Object::native_function(thread.heap(), &scope, idx.array_values)?;
        let values_name = known.strings.values.get_handle(thread.heap(), &scope);
        Object::define_own_property(
            thread.heap(),
            &scope,
            array_prototype,
            values_name,
            PropertyDescriptor::method(values_fn.erase()),
        )?;
        Object::define_own_property(
            thread.heap(),
            &scope,
            array_prototype,
            iterator_name,
            PropertyDescriptor::method(values_fn.erase()),
        )?;

        // array-iterator map: slots [iterated array, next index, kind],
        // prototype %ArrayIteratorPrototype%
        let array_iterator_map = roots.create_handle(thread.heap().allocate::<Map>(MapInit {
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
        // Safety: fresh root-slot word, rooted below before any allocation.
        let proto =
            scope.handle(unsafe { Tagged::<Value>::from_value_unchecked(object_prototype.raw()) });
        let iterator_result_map =
            thread
                .heap()
                .allocate_token_enter_heap(Map::layout_for(2), |token, _heap| {
                    let descriptors: [(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>); 2] = [
                        (value_name, flags, scope.handle(Smi::new(0).into_tagged())),
                        (done_name, flags, scope.handle(Smi::new(1).into_tagged())),
                    ];
                    roots.create_handle(token.allocate::<Map>(MapInit {
                        kind: MapKind::OBJECT.union(MapKind::EXTENDABLE),
                        value_slot_count: 2,
                        descriptors: &descriptors,
                        prototype: proto,
                    }))
                });

        // for-in enumerator map: slots [level, keys, index, visited],
        // prototype Object.prototype (it must survive `x in Object.prototype`
        // style probes without special cases; the object is never exposed)
        let for_in_enumerator_map = roots.create_handle(thread.heap().allocate::<Map>(MapInit {
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
        let math_object =
            roots.create_handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(thread, &scope, math_object, "sqrt", idx.math_sqrt)?;
        Object::install_method(thread, &scope, math_object, "log", idx.math_log)?;
        Object::install_method(thread, &scope, math_object, "pow", idx.math_pow)?;
        Object::install_method(thread, &scope, math_object, "abs", idx.math_abs)?;
        Object::install_method(thread, &scope, math_object, "floor", idx.math_floor)?;
        Object::install_method(thread, &scope, math_object, "ceil", idx.math_ceil)?;
        Object::install_method(thread, &scope, math_object, "trunc", idx.math_trunc)?;
        Object::install_method(thread, &scope, math_object, "round", idx.math_round)?;
        Object::install_method(thread, &scope, math_object, "min", idx.math_min)?;
        Object::install_method(thread, &scope, math_object, "max", idx.math_max)?;
        Object::install_method(thread, &scope, math_object, "random", idx.math_random)?;
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
            Object::define_own_property(
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
            )?;
        }
        let math_name = thread.intern(&scope, "Math");
        // Safety: fresh interned word, rooted below before the define.
        let math_name = scope.handle(math_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            math_name,
            PropertyDescriptor::method(math_object.erase()),
        )?;

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
            roots.create_handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(thread, &scope, console_object, "log", idx.console_log)?;
        let console_name = thread.intern(&scope, "console");
        // Safety: fresh interned word, rooted below before the define.
        let console_name = scope.handle(console_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            console_name,
            PropertyDescriptor::method(console_object.erase()),
        )?;

        let is_nan_fn = Object::native_function(thread.heap(), &scope, idx.is_nan)?;
        Object::set_builtin_name(thread, &scope, is_nan_fn, "isNaN")?;
        let is_nan_name = thread.intern(&scope, "isNaN");
        // Safety: fresh interned word, rooted below before the define.
        let is_nan_name = scope.handle(is_nan_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            is_nan_name,
            PropertyDescriptor::method(is_nan_fn.erase()),
        )?;

        // ---- print / performance (shell conveniences; not ES) --------------------
        let print_fn = Object::native_function(thread.heap(), &scope, idx.print)?;
        let print_name = thread.intern(&scope, "print");
        // Safety: fresh interned word, rooted below before the define.
        let print_name = scope.handle(print_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            print_name,
            PropertyDescriptor::method(print_fn.erase()),
        )?;

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
            roots.create_handle(thread.heap().new_object(&scope, map, HandleSlice::EMPTY));
        Object::install_method(
            thread,
            &scope,
            performance_object,
            "now",
            idx.performance_now,
        )?;
        let performance_name = thread.intern(&scope, "performance");
        // Safety: fresh interned word, rooted below before the define.
        let performance_name = scope.handle(performance_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            performance_name,
            PropertyDescriptor::method(performance_object.erase()),
        )?;

        // ---- Symbol -----------------------------------------------------------
        // `new Symbol()` throws from the constructor body (ES 20.4.1.1 step 1),
        // so the function itself carries [[Construct]].
        let symbol_fn = Object::native_constructor(thread.heap(), &scope, idx.symbol)?;
        Object::set_builtin_name(thread, &scope, symbol_fn, "Symbol")?;
        let symbol_name = thread.intern(&scope, "Symbol");
        // Safety: fresh interned word, rooted below before the define.
        let symbol_name = scope.handle(symbol_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            symbol_name,
            PropertyDescriptor::method(symbol_fn.erase()),
        )?;

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
        let symbol_prototype = roots.create_handle(thread.heap().new_object(
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
        Object::define_own_property(
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
        )?;
        Object::define_own_property(
            thread.heap(),
            &scope,
            symbol_prototype,
            wks_constructor,
            PropertyDescriptor::method(symbol_fn.erase()),
        )?;

        // Symbol.prototype.description is an accessor; the setter is absent.
        {
            let getter = Object::native_function(thread.heap(), &scope, idx.symbol_description)?;
            Object::set_builtin_name(thread, &scope, getter, "get description")?;
            let get = scope.handle(getter.as_tagged(thread.heap()).erase());
            let set = known.undefined.get_handle(thread.heap(), &scope).erase();
            let description_name = thread.intern(&scope, "description");
            // Safety: fresh interned word, rooted below before the define.
            let description_name = scope.handle(description_name.as_tagged(thread.heap()));
            Object::define_own_property(
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
            )?;
        }
        Object::install_method(
            thread,
            &scope,
            symbol_prototype,
            "toString",
            idx.symbol_to_string,
        )?;
        Object::install_method(
            thread,
            &scope,
            symbol_prototype,
            "valueOf",
            idx.symbol_value_of,
        )?;
        // Symbol.prototype[@@toPrimitive] returns the wrapped symbol.
        {
            let to_primitive =
                Object::native_function(thread.heap(), &scope, idx.symbol_to_primitive)?;
            Object::set_builtin_name(thread, &scope, to_primitive, "[Symbol.toPrimitive]")?;
            let key: Handle<'_, SlotName> =
                scope.handle(known.to_primitive_symbol.get(thread.heap()));
            Object::define_own_property(
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
            )?;
        }
        Object::install_method(thread, &scope, symbol_fn, "for", idx.symbol_for)?;
        Object::install_method(thread, &scope, symbol_fn, "keyFor", idx.symbol_key_for)?;

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
            Object::define_own_property(
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
            )?;
        }
        // Symbol.prototype[@@toStringTag] = "Symbol".
        let to_string_tag_key: Handle<'_, SlotName> =
            scope.handle(state.known.to_string_tag_symbol.get(thread.heap()));
        let symbol_tag = thread.intern(&scope, "Symbol");
        Object::define_own_property(
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
        )?;

        // ---- Proxy ------------------------------------------------------------
        // The Proxy constructor is a runtime function *without* a
        // `.prototype` property (ES 20.2.1: "Proxy.prototype is
        // undefined"); `install_constructor` cannot be used.
        let proxy_fn = Object::native_constructor(thread.heap(), &scope, idx.proxy)?;
        let two = scope.handle(Smi::new(2));
        Object::define_own_property(
            thread.heap(),
            &scope,
            proxy_fn,
            wks_length,
            PropertyDescriptor::non_enumerable(two),
        )?;
        Object::set_builtin_name(thread, &scope, proxy_fn, "Proxy")?;
        let proxy_name = thread.intern(&scope, "Proxy");
        // Safety: fresh interned word, rooted below before the define.
        let proxy_name = scope.handle(proxy_name.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            &scope,
            global,
            proxy_name,
            PropertyDescriptor::method(proxy_fn.erase()),
        )?;
        // Proxy.revocable: a non-constructor function returning
        // { proxy, revoke }; the revoke closure is the JS template
        // installed by REVOKE_PRELUDE (natives cannot carry state).
        let revocable_fn = Object::native_function(thread.heap(), &scope, idx.proxy_revocable)?;
        Object::define_own_property(
            thread.heap(),
            &scope,
            revocable_fn,
            wks_length,
            PropertyDescriptor::non_enumerable(two),
        )?;
        Object::set_builtin_name(thread, &scope, revocable_fn, "revocable")?;
        let revocable_name = state
            .known
            .strings
            .revocable
            .get_handle(thread.heap(), &scope);
        Object::define_own_property(
            thread.heap(),
            &scope,
            proxy_fn,
            revocable_name,
            PropertyDescriptor::method(revocable_fn.erase()),
        )?;
        // ---- Object extensibility statics --------------------------------------
        Object::install_method(thread, &scope, object_fn, "is", idx.object_is)?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "preventExtensions",
            idx.object_prevent_extensions,
        )?;
        Object::install_method(
            thread,
            &scope,
            object_fn,
            "isExtensible",
            idx.object_is_extensible,
        )?;
        Object::install_method(thread, &scope, object_fn, "seal", idx.object_seal)?;
        Object::install_method(thread, &scope, object_fn, "freeze", idx.object_freeze)?;

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
            // Safety: fresh root-slot words, rooted below before any
            // allocation.
            let receiver =
                scope.handle(unsafe { Tagged::<Object>::from_value_unchecked(global.raw()) });
            let key = scope.handle(n.as_tagged(thread.heap()));
            Object::define_own_property(
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
            )?;
        }
        Ok(())
    })
}
