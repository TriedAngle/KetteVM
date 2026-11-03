use crate::{
    Coercion, Handle, HandleScope, HandleSlice, Heap, Map, Object, PropertyDescriptor, Tagged,
    ThreadState, VM, Value, VmError,
};

/// Namespace for materializing VM errors as ECMAScript error objects.
pub struct Errors;

impl Errors {
    /// The instance map a VM error materializes as; the `VmError` kind
    /// selects the class.
    fn map_of<'a>(vm: &VM, heap: &'a Heap, err: VmError) -> Tagged<'a, Map> {
        let known = vm.known();
        let map = match err {
            VmError::Arity | VmError::Type | VmError::NotExtensible | VmError::Message(_) => {
                known.type_error_map.get(heap)
            }
            VmError::Overflow
            | VmError::OutOfBounds
            | VmError::InvalidTime
            | VmError::StackOverflow => known.range_error_map.get(heap),
            VmError::Reference => known.reference_error_map.get(heap),
            VmError::Syntax => known.syntax_error_map.get(heap),
        };
        debug_assert!(
            map.as_ref().kind().is_error(),
            "VM error maps must carry MapKind::ERROR"
        );
        map
    }

    /// `Error` object creation, shared by the constructors and engine errors:
    /// `OrdinaryCreateFromConstructor` on `newTarget` when given, an own
    /// non-enumerable `message` when it is not undefined (20.5.1.1 step 3),
    /// and `InstallErrorCause` from `options` (20.5.9.1).
    ///
    /// `Ok(None)` means user code threw and the exception is pending.
    pub fn create<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        new_target: Option<Handle<'_, Value>>,
        map: Handle<'_, Map>,
        message: Handle<'_, Value>,
        options: Handle<'_, Value>,
    ) -> Result<Option<Tagged<'a, Value>>, VmError> {
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
                None => Some(scope.handle(heap.new_object(&scope, map, HandleSlice::EMPTY))),
            };
            let Some(object) = object else {
                return Ok(None);
            };
            let known = vm.known();
            if message.as_tagged(heap) != known.undefined.get(heap) {
                let message = match Object::to_string(vm, heap, state, message)? {
                    Some(message) => scope.handle(message),
                    None => return Ok(None),
                };
                let message_key = scope.handle(known.strings.message.get(heap));
                Object::define_own_property(
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
                let cause_key = scope.handle(known.strings.cause.get(heap));
                let found = match Object::has_property(vm, heap, state, options, cause_key.erase())?
                {
                    Coercion::Threw => return Ok(None),
                    Coercion::Value(found) => scope.handle(found),
                };
                if found.as_tagged(heap).is_truthy(heap) {
                    let cause = match Object::get_property(
                        vm,
                        heap,
                        state,
                        options,
                        options,
                        cause_key.erase(),
                        false,
                    )? {
                        Coercion::Threw => return Ok(None),
                        Coercion::Value(cause) => scope.handle(cause),
                    };
                    Object::define_own_property(
                        heap,
                        &scope,
                        object,
                        cause_key,
                        PropertyDescriptor::method(cause.erase()),
                    )?;
                }
            }
            Ok(Some(object.as_tagged(heap).erase()))
        })
    }

    /// Materialize a VM error as an ECMAScript error object.
    ///
    /// The returned value is anchored to the heap borrow; callers store it
    /// into rooted memory (the pending-exception register) before the next
    /// allocation.
    #[cold]
    #[inline(never)]
    pub fn from_vm_error<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        err: VmError,
    ) -> Result<Tagged<'a, Value>, VmError> {
        state.handle_scope(|scope| {
            let map = scope.handle(Self::map_of(vm, heap, err));
            Self::with_message(vm, heap, state, &scope, map, err.message())
        })
    }

    /// ReferenceError for an unresolvable binding reference (ES 6.2.3.3
    /// GetValue on a baseless reference): `'<name>' is not defined`.
    #[cold]
    #[inline(never)]
    pub fn not_defined<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        name: &str,
    ) -> Result<Tagged<'a, Value>, VmError> {
        state.handle_scope(|scope| {
            let map = scope.handle(vm.known().reference_error_map.get(heap));
            Self::with_message(
                vm,
                heap,
                state,
                &scope,
                map,
                &format!("'{name}' is not defined"),
            )
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
    ) -> Result<Tagged<'a, Value>, VmError> {
        let message = vm.interner().intern_str(heap, scope, message).erase();
        let options = scope.handle(vm.known().undefined.get(heap).erase());
        Ok(Self::create(vm, heap, state, None, map, message, options)?
            .expect("error materialization must not fail"))
    }
}
