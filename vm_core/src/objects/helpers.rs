use crate::{
    DenseString, Float, Handle, HandleScope, HandleSlice, Heap, Map, MapInit, MapKind, NativeIndex,
    Object, PropertyDescriptor, Smi, Tagged, Thread, ThreadState, VM, Value, VmError, WellKnown,
};

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum WrapperKind {
    Number,
    Boolean,
    String,
}

impl WrapperKind {
    /// The wrapper kind of a primitive value, if it has one.
    pub fn classify(heap: &Heap, roots: &WellKnown, v: Tagged<'_, Value>) -> Option<WrapperKind> {
        if v.is_smi() || v.get_as::<Float>(heap).is_some() {
            Some(WrapperKind::Number)
        } else if v.get_as::<DenseString>(heap).is_some() {
            Some(WrapperKind::String)
        } else if v == roots.true_object.get(heap) || v == roots.false_object.get(heap) {
            Some(WrapperKind::Boolean)
        } else {
            None
        }
    }

    /// The intrinsic initial map holding this wrapper kind's
    /// `%X.prototype%` as its prototype.
    pub fn wrapper_map<'a>(self, heap: &'a Heap, roots: &'a WellKnown) -> Tagged<'a, Map> {
        match self {
            WrapperKind::Number => roots.number_wrapper_map.get(heap),
            WrapperKind::Boolean => roots.boolean_wrapper_map.get(heap),
            WrapperKind::String => roots.string_wrapper_map.get(heap),
        }
    }
}

impl Object {
    /// Callable + constructable: `new f()` is legal ([[Call]] and [[Construct]]).
    pub fn native_constructor<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
    ) -> Result<Handle<'s, Object>, VmError> {
        Self::make_native_object(heap, scope, index, true)
    }

    /// Callable only: `new f()` throws TypeError ([[Call]] without [[Construct]]).
    pub fn native_function<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
    ) -> Result<Handle<'s, Object>, VmError> {
        Self::make_native_object(heap, scope, index, false)
    }

    fn make_native_object<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        constructor: bool,
    ) -> Result<Handle<'s, Object>, VmError> {
        let mut kind = MapKind::OBJECT
            .union(MapKind::CALLABLE)
            .union(MapKind::NATIVE)
            .union(MapKind::EXTENDABLE);
        if constructor {
            kind = kind.union(MapKind::CONSTRUCTOR);
        }
        let map = heap.allocate_handle::<Map>(
            MapInit {
                kind,
                value_slot_count: 2,
                descriptors: &[],
                prototype: scope.handle(heap.known().function_prototype.get(heap).erase()),
            },
            scope,
        );
        let empty_context = scope.handle(heap.known().empty_context.get(heap));
        let obj = heap.new_object(
            scope,
            map,
            scope.stage(&[
                Smi::new(index.0 as i64).into_tagged(),
                empty_context.as_tagged(heap).erase(),
            ]),
        );
        Ok(scope.handle(obj))
    }

    pub fn install_constructor<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        name: &str,
        proto_parent: Handle<'_, Object>,
    ) -> Result<(Handle<'s, Object>, Handle<'s, Object>), VmError> {
        Self::install_constructor_with(thread, scope, index, name, proto_parent, None)
    }

    /// Like [`Object::install_constructor`], but the prototype object is
    /// itself a `PRIMITIVE_WRAPPER` carrying `value` in slot 0
    pub fn install_wrapper_constructor<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        name: &str,
        proto_parent: Handle<'_, Object>,
        value: Handle<'_, Value>,
    ) -> Result<(Handle<'s, Object>, Handle<'s, Object>), VmError> {
        Self::install_constructor_with(thread, scope, index, name, proto_parent, Some(value))
    }

    fn install_constructor_with<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        name: &str,
        proto_parent: Handle<'_, Object>,
        wrapper: Option<Handle<'_, Value>>,
    ) -> Result<(Handle<'s, Object>, Handle<'s, Object>), VmError> {
        let name_str = thread.intern(scope, name);
        let fn_obj = Self::native_constructor(thread.heap(), scope, index)?;
        Self::set_builtin_name(thread, scope, fn_obj, name)?;

        // prototype object: fresh extendable object chained to proto_parent
        // (wrapper prototypes also carry their primitive in slot 0)
        let kind = match wrapper {
            Some(_) => MapKind::OBJECT
                .union(MapKind::EXTENDABLE)
                .union(MapKind::PRIMITIVE_WRAPPER),
            None => MapKind::OBJECT.union(MapKind::EXTENDABLE),
        };
        let map = thread.heap().allocate_handle::<Map>(
            MapInit {
                kind,
                value_slot_count: usize::from(wrapper.is_some()),
                descriptors: &[],
                prototype: proto_parent.erase(),
            },
            scope,
        );
        let heap = thread.heap();
        let proto = scope.handle(match wrapper {
            Some(v) => heap.new_object(scope, map, scope.stage(&[v.as_tagged(heap)])),
            None => heap.new_object(scope, map, HandleSlice::EMPTY),
        });

        // proto.constructor = fn; fn.prototype = proto
        // (built-in methods/constructor properties are non-enumerable, ES 20+)
        let constructor_name =
            scope.handle(thread.heap().known().strings.constructor.get(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            scope,
            proto,
            constructor_name,
            PropertyDescriptor::method(fn_obj.erase()),
        )?;
        let prototype_name =
            scope.handle(thread.heap().known().strings.prototype.get(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            scope,
            fn_obj,
            prototype_name,
            PropertyDescriptor::Data {
                value: proto.erase(),
                writable: false,
                enumerable: false,
                configurable: false,
            },
        )?;

        // global.Name = fn
        let global = {
            let (vm, heap, _state) = thread.split();
            scope.handle(vm.known().global_object.get(heap))
        };
        // Safety: fresh interned word, rooted below before any allocation.
        let name = scope.handle(name_str.as_tagged(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            scope,
            global,
            name,
            PropertyDescriptor::method(fn_obj.erase()),
        )?;
        Ok((fn_obj, proto))
    }

    /// `name` of a builtin function: { writable: false, enumerable: false, configurable: true }
    pub fn set_builtin_name(
        thread: &mut Thread,
        scope: &HandleScope<'_>,
        function: Handle<'_, Object>,
        name: &str,
    ) -> Result<(), VmError> {
        let name_str = thread.intern(scope, name);
        // Safety: fresh interned word, rooted below before any allocation.
        let name_str: Handle<'_, DenseString> = scope.handle(name_str.as_tagged(thread.heap()));
        let name_key = scope.handle(thread.heap().known().strings.name.get(thread.heap()));
        Object::define_own_property(
            thread.heap(),
            scope,
            function,
            name_key,
            PropertyDescriptor::non_enumerable(name_str.erase()),
        )?;
        Ok(())
    }

    pub fn install_method(
        thread: &mut Thread,
        scope: &HandleScope<'_>,
        receiver: Handle<'_, Object>,
        name: &str,
        index: NativeIndex,
    ) -> Result<(), VmError> {
        let method = Self::native_function(thread.heap(), scope, index)?;
        let name_str = thread.intern(scope, name);
        // Safety: fresh interned word, rooted below before any allocation.
        let method_name = scope.handle(name_str.as_tagged(thread.heap()));
        Self::set_builtin_name(thread, scope, method, name)?;
        Object::define_own_property(
            thread.heap(),
            scope,
            receiver,
            method_name,
            PropertyDescriptor::method(method.erase()),
        )?;
        Ok(())
    }

    /// `OrdinaryCreateFromConstructor ( newTarget, "%X.prototype%", [[PrimitiveValue]] )`
    /// [ES 10.1.14](https://tc39.es/ecma262/#sec-ordinarycreatefromconstructor)
    /// Wrapper object whose `[[Prototype]]` is `newTarget.prototype`.
    pub fn create_wrapper_from_constructor<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        new_target: Handle<'_, Value>,
        kind: WrapperKind,
        value: Handle<'_, Value>,
    ) -> Result<Option<Tagged<'a, Value>>, VmError> {
        state.handle_scope(|scope| {
            let map = scope.handle(kind.wrapper_map(heap, vm.known()));
            let slots = scope.stage(&[value.as_tagged(heap)]);
            let obj =
                Object::create_from_constructor(vm, heap, state, &scope, new_target, map, slots)?;
            Ok(obj.map(|obj| obj.as_tagged(heap).erase()))
        })
    }
}

impl<'a> Tagged<'a, Value> {
    /// Read slots[0] of a `PRIMITIVE_WRAPPER` receiver — or the receiver
    /// itself when it is the matching unboxed primitive: builtin `this`-values
    /// are never auto-boxed (ES 5.2.3), so `Number.prototype.toString` and
    /// friends must accept raw Smi/Float/string/boolean receivers. A receiver
    /// of the wrong primitive type (and any symbol/null/undefined) is a
    /// TypeError.
    pub fn wrapper_value(
        self,
        heap: &'a Heap,
        roots: &WellKnown,
        kind: WrapperKind,
    ) -> Result<Tagged<'a, Value>, VmError> {
        if WrapperKind::classify(heap, roots, self) == Some(kind) {
            return Ok(self);
        }
        let Some(obj) = self.as_heap_object() else {
            return Err(VmError::Type);
        };
        let map = obj.map(heap);
        if !map.kind().contains(MapKind::PRIMITIVE_WRAPPER) {
            return Err(VmError::Type);
        }
        let value = obj.slots(heap).at(heap, 0);
        if WrapperKind::classify(heap, roots, value) == Some(kind) {
            Ok(value)
        } else {
            Err(VmError::Type)
        }
    }
}
