use vm_core::NativeIndex;

/// Ids of the native functions this front end's bytecode calls directly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeIds {
    /// (obj) -> iterator -- GetProperty(obj, @@iterator) + Call (ES 8.5.4)
    pub get_iterator: NativeIndex,
    /// (iterator) -> result object -- Call(GetProperty(iter, "next"), iter)
    pub iterator_next: NativeIndex,
    /// (result) -> bool -- ToBoolean(Get(result, "done"))
    pub iterator_done: NativeIndex,
    /// (result) -> value -- Get(result, "value")
    pub iterator_value: NativeIndex,
    /// (key, obj) -> bool -- HasProperty (the `in` operator, ES 14.11.2)
    pub has_property: NativeIndex,
    /// (excluded..., target, source) -- CopyDataProperties with an
    /// exclusion list (object rest, ES 8.5.1); `excluded` has count - 2
    /// entries
    pub copy_data_properties: NativeIndex,
    /// (description) -> Symbol -- a fresh private name
    pub create_private_name: NativeIndex,
    /// (obj, key) -> value -- PrivateGet (ES 7.3.30), TypeError if absent
    pub private_get: NativeIndex,
    /// (obj, key, value) -- PrivateSet (ES 7.3.31), TypeError if absent
    pub private_set: NativeIndex,
    /// (key, obj) -> bool -- `#x in obj` own-private presence
    pub private_in: NativeIndex,
    /// (ctor, fields) -- attach the instance-field array to the class
    /// constructor's hidden slot
    pub set_class_fields: NativeIndex,
    /// (ctor, instance) -> instance -- run each field initializer with the
    /// instance as receiver, [[DefineOwnProperty]] the results (ES 7.3.33)
    pub init_instance_fields: NativeIndex,
    /// (value) -> value -- RequireObjectCoercible (ES 7.2.1): TypeError on
    /// null/undefined (object destructuring sources)
    pub require_object_coercible: NativeIndex,
    /// `delete obj.key`: (target, key) -> bool; a refused delete in strict
    /// code is a TypeError
    pub delete_property_operator: NativeIndex,
    /// (name) -> bool -- sloppy `delete x` on an unresolved (global-object)
    /// name: GlobalEnvironmentRecord.DeleteBinding; declared bindings are
    /// statically known and never reach here
    pub delete_identifier_sloppy: NativeIndex,
    /// (key) -> never returns -- `delete super.x` (ES 13.5.1.2 step 4.c):
    /// ReferenceError in both language modes; the key is coerced first
    pub delete_super_property: NativeIndex,
    /// (obj) -> enumerator | undefined -- for-in head (ES 14.7.5.6):
    /// undefined for a null/undefined subject (zero iterations), else a
    /// hidden enumerator walking the prototype chain lazily
    pub for_in_enumerate: NativeIndex,
    /// (enumerator) -> key-string | undefined -- the next enumerable
    /// string key (ES 14.7.5.9 EnumerateObjectProperties), or undefined
    /// when the walk is exhausted; deleted-before-visited and
    /// shadowed keys are skipped inside
    pub for_in_next: NativeIndex,
    /// (fn, key, prefix) -> fn -- ES 8.4.4 SetFunctionName: redefine the
    /// closure's `name` ({w-, e-, c+}); the prefix discriminant is
    /// 0 none, 1 "get ", 2 "set " (a Smi)
    pub set_function_name: NativeIndex,
    /// (target, key, closure, flags) -- accessor member installation: define
    /// one accessor half, merging with an existing pair under the same key
    /// (ES 14.3.10); flags bit 0 marks the getter half, PropertyFlags bits
    /// carry enumerability
    pub install_accessor: NativeIndex,
    /// (obj, key, value, flags) -- [[DefineOwnProperty]] with exact
    /// attributes (class member installation); define sites are
    /// strict-mode code, so a rejected define throws TypeError. flags are
    /// PropertyFlags bits (the Accessor bit: the value is an AccessorPair)
    pub define_own_property: NativeIndex,
    /// (obj, proto) -> obj -- [[SetPrototypeOf]] (class prototype wiring)
    pub set_prototype: NativeIndex,
    /// (value) -> value -- TypeError unless the value is null or a
    /// constructor (class extends validation, ES 15.7.14)
    pub throw_if_not_constructor_or_null: NativeIndex,
    /// (value) -> value -- TypeError unless the value is an object or null
    /// (superCtor.prototype validation)
    pub throw_if_not_object_or_null: NativeIndex,
    /// (value) -> value -- ReferenceError on the hole: `this` access before
    /// super() in a derived constructor (ES 10.2.2)
    pub throw_super_not_called_if_hole: NativeIndex,
    /// (value) -> value -- ReferenceError unless the hole: InitializeThisBinding
    /// guard, super() may run exactly once (ES 10.2.2)
    pub throw_super_already_called_if_not_hole: NativeIndex,
    /// (args...) -> instance -- super(...): construct the frame's super
    /// constructor with the frame's new.target (ES 15.4.3); derived parents
    /// get the hole receiver
    pub construct_super: NativeIndex,
    /// () -> instance -- super() forwarding the frame's full argument list
    /// (synthesized default derived constructors, ES 15.7.13)
    pub construct_super_all_args: NativeIndex,
    /// (args..., closure, new_target) -> instance -- arrow-delegated
    /// super(): the constructor closure and its new.target ride the tail
    /// of the argument window (threaded through .this_function)
    pub construct_super_via: NativeIndex,
    /// (name) -> value -- direct-eval name load: walk the frame context
    /// chain by name (slots, then context extension objects); unresolved
    /// names fall back to the global object
    pub load_dynamic_name: NativeIndex,
    /// (value, name) -> value -- direct-eval name store: write through to
    /// the nearest context slot or extension property, else the global object
    pub store_dynamic_name: NativeIndex,
    /// (name) -> undefined -- sloppy direct-eval var declaration: create
    /// the binding in the caller's variable environment when absent
    pub declare_eval_var: NativeIndex,
    /// (name, value) -> undefined -- sloppy direct-eval function declaration
    pub declare_eval_function: NativeIndex,
    /// (callee, source) -> callable -- resolve a direct eval call; returns
    /// the callee unchanged unless it is the intrinsic eval
    pub resolve_direct_eval: NativeIndex,
    /// (value, name) -> value -- initialize a REPL top-level lexical
    pub store_repl_global_init: NativeIndex,
    /// (declarations) -> undefined -- validate all sloppy eval declarations
    /// before any binding is created
    pub validate_eval_declarations: NativeIndex,
    /// (declarations) -> undefined -- validate and install one script's
    /// global var/function declarations
    pub declare_globals: NativeIndex,
    /// () -> never -- assignment to a const binding
    pub throw_const_assign: NativeIndex,
    /// (first) -> array -- a fresh array of the frame's arguments from
    /// formal index `first` (a Smi)
    pub create_rest_parameter: NativeIndex,
    /// (mapped, index0, slot0, ...) -> object -- the frame's arguments
    /// object (ES 10.4.4.6 / 10.4.4.7); `mapped` is a Smi flag and the
    /// pairs name argument indices bound to function-context slots
    pub create_arguments: NativeIndex,
    /// (home, recv, key) -> value -- super.x load: GetSuperBase of the home
    /// object, walked with the split receiver/lookup-start (ES 15.4.2)
    pub super_get_property: NativeIndex,
    /// (home, recv, key, value, semantics) -> value -- super.x store (ES
    /// 15.4.4); semantics is SUPER_STORE_WRITE_THROUGH or 0 (shadow), a Smi
    pub super_set_property: NativeIndex,
}
