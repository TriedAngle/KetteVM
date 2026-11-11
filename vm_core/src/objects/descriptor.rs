//! ES `ToPropertyDescriptor`: JS descriptor object -> partial descriptor.

use crate::VmError;
use crate::bail;
use crate::{
    Convert, Handle, HandleScope, HandleSlice, Heap, Object, PartialDescriptor, PropertyDescriptor,
    Result, SlotName, ThreadState, VM, Value,
};

impl Object {
    pub fn to_property_descriptor<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        attrs: Handle<'_, Value>,
    ) -> Result<PartialDescriptor<'s>> {
        if attrs.as_tagged(heap).is_primitive(heap) {
            bail!(vm, heap, state, VmError::PropertyDescriptorNotObject);
        }
        let attrs = scope.handle(attrs.as_tagged(heap));
        let strings = &heap.known().strings;
        // ES 6.2.6.4 reads the fields in this exact order.
        let names: [Handle<'_, Value>; 6] = [
            scope.handle(strings.enumerable.get(heap).erase()),
            scope.handle(strings.configurable.get(heap).erase()),
            scope.handle(strings.value.get(heap).erase()),
            scope.handle(strings.writable.get(heap).erase()),
            scope.handle(strings.get.get(heap).erase()),
            scope.handle(strings.set.get(heap).erase()),
        ];
        let mut read =
            |name: Handle<'_, Value>| descriptor_field(vm, heap, state, scope, attrs, name);
        let enumerable = read(names[0])?;
        let configurable = read(names[1])?;
        let value = read(names[2])?;
        let writable = read(names[3])?;
        let get = read(names[4])?;
        let set = read(names[5])?;
        let undefined = heap.known().undefined.get(heap).erase();
        for half in [get, set] {
            if let Some(h) = half
                && !h.as_tagged(heap).ptr_eq(undefined)
                && !h.as_tagged(heap).is_callable(heap)
            {
                bail!(vm, heap, state, VmError::AccessorNotCallable);
            }
        }
        if (get.is_some() || set.is_some()) && (value.is_some() || writable.is_some()) {
            bail!(vm, heap, state, VmError::InvalidPropertyDescriptor);
        }
        Ok(PartialDescriptor {
            value,
            get,
            set,
            writable: writable.map(|v| v.as_tagged(heap).is_truthy(heap)),
            enumerable: enumerable.map(|v| v.as_tagged(heap).is_truthy(heap)),
            configurable: configurable.map(|v| v.as_tagged(heap).is_truthy(heap)),
        })
    }
}

fn descriptor_field<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    attrs: Handle<'_, Value>,
    name: Handle<'_, Value>,
) -> Result<Option<Handle<'s, Value>>> {
    if !Object::has_property(vm, heap, state, attrs, name)? {
        return Ok(None);
    }
    let value = scope.handle(Object::get_property(
        vm, heap, state, attrs, attrs, name, false,
    )?);
    Ok(Some(value))
}

impl Object {
    /// FromPropertyDescriptor (ES 6.2.6.4): descriptor object with present fields.
    pub fn descriptor_object<'s>(
        heap: &mut Heap,
        scope: &'s HandleScope<'_>,
        partial: &PartialDescriptor<'_>,
    ) -> Result<Handle<'s, Value>> {
        let map = heap.known().object_initial_map.get_handle(heap, scope);
        let obj = heap
            .new_object(scope, map, HandleSlice::EMPTY)
            .as_handle(scope);
        let s = &heap.known().strings;
        let names = [
            scope.handle(s.value.get(heap)),
            scope.handle(s.writable.get(heap)),
            scope.handle(s.get.get(heap)),
            scope.handle(s.set.get(heap)),
            scope.handle(s.enumerable.get(heap)),
            scope.handle(s.configurable.get(heap)),
        ];
        let mut fields: Vec<(Handle<'_, SlotName>, Handle<'_, Value>)> = Vec::new();
        if let Some(v) = partial.value {
            fields.push((names[0], v));
        }
        if let Some(b) = partial.writable {
            fields.push((names[1], scope.handle(Convert::boolean(heap, b))));
        }
        if let Some(v) = partial.get {
            fields.push((names[2], v));
        }
        if let Some(v) = partial.set {
            fields.push((names[3], v));
        }
        if let Some(b) = partial.enumerable {
            fields.push((names[4], scope.handle(Convert::boolean(heap, b))));
        }
        if let Some(b) = partial.configurable {
            fields.push((names[5], scope.handle(Convert::boolean(heap, b))));
        }
        for (name, value) in fields {
            Object::define_own_property_ordinary(
                heap,
                scope,
                obj,
                name,
                PropertyDescriptor::data(value),
            )
            .expect("fresh descriptor object accepts data fields");
        }
        Ok(obj.erase())
    }
}
