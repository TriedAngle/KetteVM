//! ES `ToPropertyDescriptor`: JS descriptor object -> partial descriptor.

use crate::{
    Coercion, Handle, HandleScope, Heap, Object, PartialDescriptor, ThreadState, VM, Value, VmError,
};

impl Object {
    pub fn to_property_descriptor<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        attrs: Handle<'_, Value>,
    ) -> Result<Option<PartialDescriptor<'s>>, VmError> {
        if attrs.as_tagged(heap).is_primitive(heap) {
            return Err(VmError::Type);
        }
        let attrs = scope.handle(attrs.as_tagged(heap));
        let strings = &heap.known().strings;
        let names: [Handle<'_, Value>; 6] = [
            scope.handle(strings.value.get(heap).erase()),
            scope.handle(strings.get.get(heap).erase()),
            scope.handle(strings.set.get(heap).erase()),
            scope.handle(strings.writable.get(heap).erase()),
            scope.handle(strings.enumerable.get(heap).erase()),
            scope.handle(strings.configurable.get(heap).erase()),
        ];
        let mut read =
            |name: Handle<'_, Value>| descriptor_field(vm, heap, state, scope, attrs, name);
        let Some(value) = read(names[0])? else {
            return Ok(None);
        };
        let Some(get) = read(names[1])? else {
            return Ok(None);
        };
        let Some(set) = read(names[2])? else {
            return Ok(None);
        };
        let Some(writable) = read(names[3])? else {
            return Ok(None);
        };
        let Some(enumerable) = read(names[4])? else {
            return Ok(None);
        };
        let Some(configurable) = read(names[5])? else {
            return Ok(None);
        };
        if get.is_some() || set.is_some() {
            for half in [get, set] {
                if let Some(h) = half
                    && !h.as_tagged(heap).is_callable(heap)
                {
                    return Err(VmError::Type);
                }
            }
        }
        Ok(Some(PartialDescriptor {
            value,
            get,
            set,
            writable: writable.map(|v| v.as_tagged(heap).is_truthy(heap)),
            enumerable: enumerable.map(|v| v.as_tagged(heap).is_truthy(heap)),
            configurable: configurable.map(|v| v.as_tagged(heap).is_truthy(heap)),
        }))
    }
}

fn descriptor_field<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    attrs: Handle<'_, Value>,
    name: Handle<'_, Value>,
) -> Result<Option<Option<Handle<'s, Value>>>, VmError> {
    match Object::get_property(vm, heap, state, attrs, attrs, name, false)? {
        Coercion::Threw => Ok(None),
        Coercion::Value(v) => {
            let v = scope.handle(v);
            if v.as_tagged(heap)
                .ptr_eq(heap.known().undefined.get(heap).erase())
            {
                Ok(Some(None))
            } else {
                Ok(Some(Some(v)))
            }
        }
    }
}
