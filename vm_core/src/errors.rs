use std::borrow::Cow;

use crate::Result;
use crate::{
    GcSlot, Handle, HandleScope, HandleSlice, Heap, Map, Object, PropertyDescriptor, Tagged,
    ThreadState, Thrown, VM, Value,
};

macro_rules! vm_errors {
    ($( $variant:ident => $kind:ident, $text:literal ; )*) => {
        /// An engine-generated error: selects the error class and message text.
        ///
        /// This is a constructor selector, not an error channel. VM operations
        /// report thrown guest exceptions as `Err(Thrown)`; `VmError` only names
        /// the object [`Errors::throw`] should materialize.
        #[derive(Debug, Copy, Clone, PartialEq, Eq)]
        pub enum VmError {
            $( $variant, )*
            /// `Cannot perform '<trap>' on a proxy that has been revoked`
            ProxyRevoked(&'static str),
        }

        impl VmError {
            pub const fn kind(self) -> ErrorKind {
                match self {
                    Self::ProxyRevoked(_) => ErrorKind::Type,
                    $( Self::$variant => ErrorKind::$kind, )*
                }
            }

            pub fn text(self) -> Cow<'static, str> {
                match self {
                    Self::ProxyRevoked(trap) => Cow::Owned(format!(
                        "cannot perform '{trap}' on a proxy that has been revoked"
                    )),
                    $( Self::$variant => Cow::Borrowed($text), )*
                }
            }

            /// Replace the single `%` placeholder in a parameterized text.
            pub fn text_with(self, arg: &str) -> Cow<'static, str> {
                let text = self.text();
                if text.contains('%') {
                    Cow::Owned(text.replace('%', arg))
                } else {
                    text
                }
            }
        }
    };
}

/// The ECMAScript error class an engine error materializes as.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Error,
    Eval,
    Range,
    Reference,
    Syntax,
    Type,
    Uri,
}

vm_errors! {
    InvalidOperandType => Type, "invalid operand type";
    NotCallable => Type, "value is not callable";
    NotExtensible => Type, "object is not extensible";
    InvalidGlobalDeclaration => Type, "invalid global declaration";
    InvalidGlobalDeclarations => Type, "invalid global declarations";
    ConstAssign => Type, "assignment to a constant variable";
    CannotConvertToNumber => Type, "cannot convert value to a number";
    CannotConvertToPrimitive => Type, "cannot convert value to a primitive";
    CannotConvertObjectToPrimitive => Type, "cannot convert object to a primitive";
    CannotConvertToObject => Type, "cannot convert value to an object";
    CannotConvertNullishToObject => Type, "cannot convert null or undefined to an object";
    CannotConvertSymbolToString => Type, "cannot convert a Symbol to a string";
    CannotSetProperty => Type, "cannot set property";
    CannotSetPropertyOfNullish => Type, "cannot set property on null or undefined";
    CannotReadPropertyOfNullish => Type, "cannot read property of null or undefined";
    CannotCheckPropertyOfNullish => Type, "cannot check property of null or undefined";
    CannotDeletePropertyOfNullish => Type, "cannot delete property of null or undefined";
    CannotGetPropertyOfNullish => Type, "cannot get property of null or undefined";
    CannotDefineProperty => Type, "cannot define property";
    CannotDefineOnPrimitive => Type, "cannot define property on a primitive";
    CannotRedefineProperty => Type, "cannot redefine property";
    CyclicPrototype => Type, "cyclic __proto__ value";
    ImmutablePrototype => Type, "prototype is immutable";
    InvalidPropertyKey => Type, "invalid property key";
    InvalidSuperCall => Type, "invalid super call";
    NewTargetNotObject => Type, "new.target is not an object";
    ClassConstructorCalled => Type, "class constructor cannot be invoked without new";
    DerivedConstructorReturnedNonObject => Type, "derived constructor must return an object";
    WrongArgumentCount => Type, "wrong number of arguments";
    RevokedProxy => Type, "revoked proxy";
    PrototypeReceiverNotObject => Type, "prototype receiver is not an object";
    PrototypeTargetNotObject => Type, "prototype target is not an object";
    PreventExtensionsTargetNotObject => Type, "preventExtensions target is not an object";
    FunctionPrototypeNotObject => Type, "Function.prototype is not an object";
    PropertyDescriptorNotObject => Type, "property descriptor must be an object";
    InvalidPropertyDescriptor => Type, "property descriptor cannot be both data and accessor";
    AccessorNotCallable => Type, "accessor descriptor get/set must be callable or undefined";
    SymbolToPrimitiveNotCallable => Type, "Symbol.toPrimitive is not callable";
    SymbolToPrimitiveNotPrimitive => Type, "Symbol.toPrimitive did not return a primitive";
    InstanceofRightNotCallable => Type, "right-hand side of instanceof is not callable";
    CreateListFromArrayLikeNonObject => Type, "CreateListFromArrayLike called on a non-object";
    IncompatibleBooleanReceiver => Type, "Boolean.prototype method called on an incompatible receiver";
    IncompatibleNumberReceiver => Type, "Number.prototype method called on an incompatible receiver";
    IncompatibleStringReceiver => Type, "String.prototype method called on an incompatible receiver";
    IncompatibleSymbolReceiver => Type, "Symbol.prototype method called on an incompatible receiver";
    ProxyDefineFalsish => Type, "proxy defineProperty trap returned falsish";
    ProxyGetOwnPropertyDescriptorInvalid => Type, "proxy getOwnPropertyDescriptor trap must return an object or undefined";
    ProxyGetOwnPropertyDescriptorNonConfigurable => Type, "proxy getOwnPropertyDescriptor trap may not hide a non-configurable property";
    ProxyGetOwnPropertyDescriptorNonExtensible => Type, "proxy getOwnPropertyDescriptor trap may not hide a property of a non-extensible target";
    ProxyGetOwnPropertyDescriptorIncompatible => Type, "proxy getOwnPropertyDescriptor trap returned an incompatible descriptor";
    ProxyGetOwnPropertyDescriptorNewConfigurable => Type, "proxy getOwnPropertyDescriptor trap may not claim a non-configurable new property";
    ProxyGetOwnPropertyDescriptorUnconfigure => Type, "proxy getOwnPropertyDescriptor trap may not make a configurable property non-configurable";
    ProxyGetOwnPropertyDescriptorUnwritable => Type, "proxy getOwnPropertyDescriptor trap may not make a non-configurable property non-writable";
    IndexOutOfBounds => Range, "index out of bounds";
    InvalidArrayLength => Range, "Invalid array length";
    InvalidTimeValue => Range, "Invalid time value";
    StackOverflow => Range, "maximum call stack size exceeded";
    UninitializedVariable => Reference, "cannot access variable before initialization";
    NotDefined => Reference, "'%' is not defined";
    InvalidDeclaration => Syntax, "invalid or conflicting declaration";
    ProxyBadTargetOrHandler => Type, "cannot create proxy with a non-object target or handler";
    ProxyConstructNotObject => Type, "proxy construct trap must return an object";
    ProxyDefineIncompatible => Type, "proxy defineProperty trap returned an incompatible descriptor";
    ProxyDefineNewConfigurable => Type, "proxy defineProperty trap may not claim a non-configurable new property";
    ProxyDefineNonExtensible => Type, "proxy defineProperty trap may not add a property to a non-extensible target";
    ProxyDefineUnconfigure => Type, "proxy defineProperty trap may not make a configurable property non-configurable";
    ProxyDefineUnwritable => Type, "proxy defineProperty trap may not make a non-configurable property non-writable";
    ProxyDeleteNonConfigurable => Type, "proxy deleteProperty trap may not delete a non-configurable property";
    ProxyDeleteNonExtensible => Type, "proxy deleteProperty trap may not delete a property of a non-extensible target";
    ProxyExtensibilityMismatch => Type, "proxy isExtensible trap must match the target's extensibility";
    ProxyGetAccessorUndefined => Type, "proxy get trap must return undefined for an accessor without a getter";
    ProxyGetMismatch => Type, "proxy get trap must match a non-writable, non-configurable property";
    ProxyGetPrototypeOfMismatch => Type, "proxy getPrototypeOf trap must match the target's prototype";
    ProxyGetPrototypeOfNotObject => Type, "proxy getPrototypeOf trap must return an object or null";
    ProxyHasHiddenNonExtensible => Type, "proxy has trap may not hide a property of a non-extensible target";
    ProxyHasNonConfigurable => Type, "proxy has trap may not hide a non-configurable property";
    ProxyOwnKeysDuplicate => Type, "proxy ownKeys trap returned duplicate keys";
    ProxyOwnKeysInvalidKey => Type, "proxy ownKeys trap returned a non-property-key";
    ProxyOwnKeysNonConfigurable => Type, "proxy ownKeys trap omitted a non-configurable property";
    ProxyOwnKeysNonExtensible => Type, "proxy ownKeys trap omitted a property of a non-extensible target";
    ProxyPreventExtensionsTrue => Type, "proxy preventExtensions trap returned true for an extensible target";
    ProxyRequiresNew => Type, "constructor Proxy requires 'new'";
    ProxySetAccessorNoSetter => Type, "proxy set trap may not report success for an accessor without a setter";
    ProxySetMismatch => Type, "proxy set trap must match a non-writable, non-configurable property";
    ProxySetPrototypeOfMismatch => Type, "proxy setPrototypeOf trap may not change a non-extensible target's prototype";
    ProxySetPrototypeOfFalse => Type, "proxy setPrototypeOf trap returned false";
    ProxyTrapNotFunction => Type, "proxy trap is not a function";
}

/// Namespace for materializing VM errors as ECMAScript error objects.
pub struct Errors;

impl Errors {
    /// `Error` object creation, shared by the constructors and engine errors:
    /// `OrdinaryCreateFromConstructor` on `newTarget` when given, an own
    /// non-enumerable `message` when it is not undefined (20.5.1.1 step 3),
    /// and `InstallErrorCause` from `options` (20.5.9.1).
    pub fn create<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        new_target: Option<Handle<'_, Value>>,
        map: Handle<'_, Map>,
        message: Handle<'_, Value>,
        options: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let object = match new_target {
                Some(new_target) => Object::create_from_constructor(
                    vm,
                    heap,
                    state,
                    &scope,
                    new_target,
                    map,
                    HandleSlice::EMPTY,
                )?,
                None => scope.handle(heap.new_object(&scope, map, HandleSlice::EMPTY)),
            };
            let known = vm.known();
            if message.as_tagged(heap) != known.undefined.get(heap) {
                let message = scope.handle(Object::to_string(vm, heap, state, message)?);
                let message_key = known.strings.message.get_handle(heap, &scope);
                Object::define_own_property_ordinary(
                    heap,
                    &scope,
                    object,
                    message_key,
                    PropertyDescriptor::method(message.erase()),
                )?;
            }
            // InstallErrorCause: `options` must be an Object and
            // `HasProperty(options, "cause")` walks its prototype chain.
            if !options.as_tagged(heap).is_primitive(heap) {
                let cause_key = known.strings.cause.get_handle(heap, &scope);
                let found = Object::has_property(vm, heap, state, options, cause_key.erase())?;
                if found {
                    let cause = Object::get_property(
                        vm,
                        heap,
                        state,
                        options,
                        options,
                        cause_key.erase(),
                        false,
                    )?;
                    let cause = scope.handle(cause);
                    Object::define_own_property_ordinary(
                        heap,
                        &scope,
                        object,
                        cause_key,
                        PropertyDescriptor::method(cause.erase()),
                    )?;
                }
            }
            Ok(object.as_tagged(heap).erase())
        })
    }

    fn slot(vm: &VM, kind: ErrorKind) -> &GcSlot<Map> {
        match kind {
            ErrorKind::Error => &vm.known().error_map,
            ErrorKind::Eval => &vm.known().eval_error_map,
            ErrorKind::Range => &vm.known().range_error_map,
            ErrorKind::Reference => &vm.known().reference_error_map,
            ErrorKind::Syntax => &vm.known().syntax_error_map,
            ErrorKind::Type => &vm.known().type_error_map,
            ErrorKind::Uri => &vm.known().uri_error_map,
        }
    }

    /// Materialize `err` and make it the pending exception.
    #[cold]
    #[inline(never)]
    pub fn throw(vm: &VM, heap: &mut Heap, state: &ThreadState, err: VmError) -> Thrown {
        let text = err.text();
        debug_assert!(
            !text.contains('%'),
            "parameterized error requires throw_with/raise_with"
        );
        Self::throw_with_slot(vm, heap, state, err.kind(), text.as_ref())
    }

    /// Materialize `err` with one `%` argument and make it pending.
    #[cold]
    #[inline(never)]
    pub fn throw_with(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        err: VmError,
        arg: &str,
    ) -> Thrown {
        let text = err.text_with(arg);
        Self::throw_with_slot(vm, heap, state, err.kind(), text.as_ref())
    }

    /// Materialize `err` and return the exception sentinel.
    #[cold]
    #[inline(never)]
    pub fn raise<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        err: VmError,
    ) -> Tagged<'a, Value> {
        Self::throw(vm, heap, state, err);
        vm.known().exception.get(heap).erase()
    }

    /// Materialize `err` with one `%` argument and return the sentinel.
    #[cold]
    #[inline(never)]
    pub fn raise_with<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        err: VmError,
        arg: &str,
    ) -> Tagged<'a, Value> {
        Self::throw_with(vm, heap, state, err, arg);
        vm.known().exception.get(heap).erase()
    }

    #[cold]
    #[inline(never)]
    fn throw_with_slot(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        kind: ErrorKind,
        message: &str,
    ) -> Thrown {
        state.handle_scope(|scope| {
            let map = Self::slot(vm, kind).get_handle(heap, &scope);
            let ex = Self::with_message(vm, heap, state, &scope, map, message);
            state.set_pending_exception(ex);
            Thrown
        })
    }

    /// Materialize an error object of `map`'s class with a host-side message.
    #[cold]
    #[inline(never)]
    fn with_message<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        scope: &HandleScope<'_>,
        map: Handle<'_, Map>,
        message: &str,
    ) -> Tagged<'a, Value> {
        let message = vm.interner().intern_str(heap, scope, message).erase();
        let options = vm.known().undefined.get_handle(heap, scope).erase();
        Self::create(vm, heap, state, None, map, message, options)
            .expect("error materialization must not fail")
    }
}
