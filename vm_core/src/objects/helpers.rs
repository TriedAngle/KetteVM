use crate::{
    CallableInfoInit, CallableInfoObject, DenseString, Float, FunctionKind, Handle, HandleScope,
    HandleSlice, Heap, Map, MapInit, MapKind, NativeIndex, Object, PropertyDescriptor, Prototype,
    Result, SlotFlags, SlotName, Smi, Tagged, Thread, ThreadState, VM, Value, WellKnown,
    mode_from_strict,
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
        length: u32,
    ) -> Handle<'s, Object> {
        Self::make_native_object(heap, scope, index, true, length)
    }

    /// Callable only: `new f()` throws TypeError ([[Call]] without [[Construct]]).
    pub fn native_function<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        length: u32,
    ) -> Handle<'s, Object> {
        Self::make_native_object(heap, scope, index, false, length)
    }

    /// Drop `EXTENDABLE` without touching any descriptor. Used for
    /// intrinsics that must be created extensible and then frozen.
    pub fn make_non_extensible(heap: &mut Heap, scope: &HandleScope<'_>, obj: Handle<'_, Object>) {
        let (kind, value_slot_count, prototype, descriptors) = {
            let map = obj.as_tagged(heap).map(heap);
            let kind = map.kind();
            if !kind.is_extendable() {
                return;
            }
            let descriptors: Vec<(Handle<'_, SlotName>, SlotFlags, Handle<'_, Value>)> = map
                .descriptors()
                .iter()
                .map(|d| {
                    (
                        scope.handle(d.name(heap)),
                        d.flags(),
                        scope.handle(d.value.get(heap)),
                    )
                })
                .collect();
            (
                kind,
                map.value_slot_count(),
                scope.handle(map.prototype.get(heap)),
                descriptors,
            )
        };
        let new_map = heap.allocate_handle::<Map>(
            MapInit {
                kind: MapKind::new(kind.bits() & !MapKind::EXTENDABLE.bits()),
                value_slot_count,
                descriptors: &descriptors,
                prototype,
            },
            scope,
        );
        let old_map = obj.as_tagged(heap).map(heap);
        Prototype::shape_changed(heap, old_map);
        obj.as_tagged(heap).header.map.set(
            heap,
            obj.as_tagged(heap).erase(),
            new_map.as_tagged(heap),
        );
    }

    fn make_native_object<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        constructor: bool,
        length: u32,
    ) -> Handle<'s, Object> {
        let info = heap.allocate_handle::<CallableInfoObject>(
            CallableInfoInit::Native {
                index,
                mode: mode_from_strict(true),
            },
            scope,
        );
        info.as_tagged(heap).set_metadata_full(
            heap,
            None,
            0,
            length as usize,
            FunctionKind::Normal,
        );
        let map = if constructor {
            heap.known().function_map.get_handle(heap, scope)
        } else {
            heap.known()
                .non_constructor_function_map
                .get_handle(heap, scope)
        };
        let empty_context = heap.known().empty_context.get_handle(heap, scope);
        let obj = heap.new_object(
            scope,
            map,
            scope.stage(&[
                info.as_tagged(heap).erase(),
                empty_context.as_tagged(heap).erase(),
            ]),
        );
        let obj = scope.handle(obj);
        let length_name = heap.known().strings.length.get_handle(heap, scope);
        Object::define_own_property_ordinary(
            heap,
            scope,
            obj,
            length_name,
            PropertyDescriptor::Data {
                value: scope.handle(Smi::new(length as i64).into_tagged()),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        )
        .expect("install builtin length");
        obj
    }

    pub fn install_constructor<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        name: &str,
        proto_parent: Handle<'_, Object>,
        length: u32,
    ) -> (Handle<'s, Object>, Handle<'s, Object>) {
        Self::install_constructor_with(thread, scope, index, name, proto_parent, None, length)
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
        length: u32,
    ) -> (Handle<'s, Object>, Handle<'s, Object>) {
        Self::install_constructor_with(
            thread,
            scope,
            index,
            name,
            proto_parent,
            Some(value),
            length,
        )
    }

    fn install_constructor_with<'s>(
        thread: &mut Thread,
        scope: &'s HandleScope<'_>,
        index: NativeIndex,
        name: &str,
        proto_parent: Handle<'_, Object>,
        wrapper: Option<Handle<'_, Value>>,
        length: u32,
    ) -> (Handle<'s, Object>, Handle<'s, Object>) {
        let name_str = thread.intern(scope, name);
        let fn_obj = Self::native_constructor(thread.heap(), scope, index, length);
        Self::set_builtin_name(thread, scope, fn_obj, name);

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
        let constructor_name = heap.known().strings.constructor.get_handle(heap, scope);
        Object::define_own_property_ordinary(
            heap,
            scope,
            proto,
            constructor_name,
            PropertyDescriptor::method(fn_obj.erase()),
        )
        .expect("install constructor name");
        let prototype_name = heap.known().strings.prototype.get_handle(heap, scope);
        Object::define_own_property_ordinary(
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
        )
        .expect("install constructor prototype");

        // global.Name = fn
        let global = {
            let heap = thread.heap();
            heap.known().global_object.get_handle(heap, scope)
        };
        // Safety: fresh interned word, rooted below before any allocation.
        let name = scope.handle(name_str.as_tagged(thread.heap()));
        Object::define_own_property_ordinary(
            thread.heap(),
            scope,
            global,
            name,
            PropertyDescriptor::method(fn_obj.erase()),
        )
        .expect("install global constructor");
        (fn_obj, proto)
    }

    /// `name` of a builtin function: { writable: false, enumerable: false, configurable: true }
    pub fn set_builtin_name(
        thread: &mut Thread,
        scope: &HandleScope<'_>,
        function: Handle<'_, Object>,
        name: &str,
    ) {
        let name_str = thread.intern(scope, name);
        // Safety: fresh interned word, rooted below before any allocation.
        let name_str: Handle<'_, DenseString> = scope.handle(name_str.as_tagged(thread.heap()));
        let name_key = thread
            .heap()
            .known()
            .strings
            .name
            .get_handle(thread.heap(), scope);
        Object::define_own_property_ordinary(
            thread.heap(),
            scope,
            function,
            name_key,
            PropertyDescriptor::non_enumerable(name_str.erase()),
        )
        .expect("install builtin name");
        let (_vm, heap, _state) = thread.split();
        if let Some(info) = function.as_tagged(heap).callable_info(heap) {
            info.as_ref()
                .set_name(heap, Some(name_str.as_tagged(heap).erase()));
        }
    }

    pub fn install_method(
        thread: &mut Thread,
        scope: &HandleScope<'_>,
        receiver: Handle<'_, Object>,
        name: &str,
        index: NativeIndex,
        length: u32,
    ) {
        let method = Self::native_function(thread.heap(), scope, index, length);
        let name_str = thread.intern(scope, name);
        let method_name = scope.handle(name_str.as_tagged(thread.heap()));
        Self::set_builtin_name(thread, scope, method, name);
        Object::define_own_property_ordinary(
            thread.heap(),
            scope,
            receiver,
            method_name,
            PropertyDescriptor::method(method.erase()),
        )
        .expect("install builtin method");
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
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let map = scope.handle(kind.wrapper_map(heap, vm.known()));
            let slots = scope.stage(&[value.as_tagged(heap)]);
            let obj =
                Object::create_from_constructor(vm, heap, state, &scope, new_target, map, slots)?;
            Ok(obj.as_tagged(heap).erase())
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
    ) -> Option<Tagged<'a, Value>> {
        if WrapperKind::classify(heap, roots, self) == Some(kind) {
            return Some(self);
        }
        let obj = self.as_heap_object()?;
        let map = obj.map(heap);
        if !map.kind().contains(MapKind::PRIMITIVE_WRAPPER) {
            return None;
        }
        let value = obj.slots(heap).at(heap, 0);
        (WrapperKind::classify(heap, roots, value) == Some(kind)).then_some(value)
    }
}
