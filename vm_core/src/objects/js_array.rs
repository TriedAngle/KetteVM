//! Array exotic object length policy: LengthOfArrayLike, ArraySetLength,
//! and the ToUint32/ToNumber pair ArraySetLength needs.

use crate::{
    Convert, DenseString, Handle, HandleScope, Heap, Object, PartialDescriptor, ProxyObject,
    Result, ShouldThrow, Smi, Tagged, ThreadState, VM, Value, VmError, bail,
};

impl Object {
    /// `IsArray ( argument )`
    /// [ES 7.2.2](https://tc39.es/ecma262/#sec-isarray)
    /// Follows proxy targets; a revoked proxy yields `None`.
    pub fn is_array(heap: &Heap, value: Tagged<'_, Value>) -> Option<bool> {
        let mut value = value;
        loop {
            let Some(object) = value.as_heap_object() else {
                return Some(false);
            };
            let kind = object.map(heap).kind();
            if kind.is_array() {
                return Some(true);
            }
            if !kind.is_proxy() {
                return Some(false);
            }
            // Safety: the map kind proves the concrete ProxyObject layout.
            let proxy = unsafe { value.cast::<ProxyObject>() };
            let (target, handler) = proxy.parts(heap);
            if handler == heap.known().null.get(heap) {
                return None;
            }
            value = target;
        }
    }

    /// LengthOfArrayLike ( obj )
    /// [ES 7.3.18](https://tc39.es/ecma262/#sec-lengthofarraylike)
    /// Reads `length` and applies ToLength; arrays and strings take a fast
    /// path.
    pub fn length_of_array_like<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        scope: &HandleScope<'_>,
        obj: Handle<'_, Value>,
    ) -> Result<u64> {
        if let Some(o) = obj.as_tagged(heap).as_heap_object()
            && o.as_ref().is_array_exotic(heap)
        {
            return Ok(o.as_ref().length() as u64);
        }
        if let Some(s) = DenseString::from_receiver(heap, obj.as_tagged(heap)) {
            return Ok(s.len() as u64);
        }
        // TODO: typed-array fast path once [[TypedArrayName]] exists.
        let length_key = heap.known().strings.length.get_handle(heap, scope).erase();
        let length_value = scope.handle(Self::get_property(
            vm, heap, state, obj, obj, length_key, false,
        )?);
        let length = Self::to_numeric(vm, heap, state, length_value)?;
        Ok(length.clamp(0.0, 9_007_199_254_740_991.0) as u64)
    }

    /// AnythingToArrayLength ( value )
    /// [ES 10.4.2.4](https://tc39.es/ecma262/#sec-arraysetlength)
    /// ToUint32 and ToNumber applied separately, so an object value's
    /// `valueOf` runs twice; unequal results are a RangeError.
    pub fn anything_to_array_length(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        value: Handle<'_, Value>,
    ) -> Result<u32> {
        let new_len = Convert::number_to_uint32(Self::to_numeric(vm, heap, state, value)?);
        let number_len = Self::to_numeric(vm, heap, state, value)?;
        if number_len != new_len as f64 {
            bail!(vm, heap, state, VmError::InvalidArrayLength);
        }
        Ok(new_len)
    }

    /// ArraySetLength ( array, propertyDesc )
    /// [ES 10.4.2.4](https://tc39.es/ecma262/#sec-arraysetlength)
    /// Applies a `"length"` descriptor: coerces the new length, deletes
    /// elements when shrinking, and defers `[[Writable]]: false` until after
    /// the deletion pass.
    pub fn array_set_length<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        array: Handle<'_, Object>,
        partial: PartialDescriptor<'s>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        if partial.is_accessor_descriptor()
            || partial.configurable == Some(true)
            || partial.enumerable == Some(true)
        {
            return should_throw.reject(vm, heap, state);
        }
        let length_name = heap.known().strings.length.get_handle(heap, scope);
        let length_value = scope.handle(length_name.as_tagged(heap).erase());
        let array_value = scope.handle(array.as_tagged(heap).erase());
        let Some(value) = partial.value else {
            let current = Self::own_property_ordinary(heap, scope, array_value, length_value);
            return Self::define_own_property_apply(
                Some(vm),
                Some(state),
                heap,
                scope,
                array,
                length_name,
                current,
                &partial,
                should_throw,
            );
        };
        let new_len = Self::anything_to_array_length(vm, heap, state, value)?;
        // the descriptor is read after the coercions: re-entrant code
        // may have changed the length or its writability
        let old_len = array.as_tagged(heap).length();
        let writable = array.as_tagged(heap).map(heap).length_writable(heap);
        let new_len = new_len as usize;
        if new_len >= old_len {
            let value = scope.handle(Smi::new(new_len as i64).into_tagged());
            let coerced = PartialDescriptor {
                value: Some(value),
                ..partial
            };
            let current = Self::own_property_ordinary(heap, scope, array_value, length_value);
            return Self::define_own_property_apply(
                Some(vm),
                Some(state),
                heap,
                scope,
                array,
                length_name,
                current,
                &coerced,
                should_throw,
            );
        }
        if !writable {
            return should_throw.reject(vm, heap, state);
        }
        let shrunk = Self::set_array_length(heap, scope, &array, new_len);
        if partial.writable == Some(false) {
            let attrs = PartialDescriptor {
                writable: Some(false),
                ..PartialDescriptor::default()
            };
            let current = Self::own_property_ordinary(heap, scope, array_value, length_value);
            Self::define_own_property_apply(
                Some(vm),
                Some(state),
                heap,
                scope,
                array,
                length_name,
                current,
                &attrs,
                should_throw,
            )?;
        }
        if shrunk {
            Ok(true)
        } else {
            should_throw.reject(vm, heap, state)
        }
    }
}
