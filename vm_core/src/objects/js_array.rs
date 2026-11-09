//! Array exotic object length policy: LengthOfArrayLike, ArraySetLength,
//! and the ToUint32/ToNumber pair ArraySetLength needs.

use crate::proxy::Flow;
use crate::{
    Coercion, Convert, DenseString, Handle, HandleScope, Heap, Object, PartialDescriptor,
    ShouldThrow, Smi, ThreadState, VM, Value, VmError,
};

impl Object {
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
    ) -> Result<Option<u64>, VmError> {
        if let Some(o) = obj.as_tagged(heap).as_heap_object()
            && o.as_ref().is_array(heap)
        {
            return Ok(Some(o.as_ref().length() as u64));
        }
        if let Some(s) = DenseString::from_receiver(heap, obj.as_tagged(heap)) {
            return Ok(Some(s.len() as u64));
        }
        // TODO: typed-array fast path once [[TypedArrayName]] exists.
        let length_key = heap.known().strings.length.get_handle(heap, scope).erase();
        let length_value = match Self::get_property(vm, heap, state, obj, obj, length_key, false) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => return Ok(None),
            Err(err) => return Err(err),
        };
        let length = match Self::to_numeric(vm, heap, state, length_value) {
            Ok(Some(n)) => n.clamp(0.0, 9_007_199_254_740_991.0) as u64,
            Ok(None) => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(Some(length))
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
    ) -> Result<Option<u32>, VmError> {
        let new_len = match Self::to_numeric(vm, heap, state, value)? {
            Some(n) => Convert::number_to_uint32(n),
            None => return Ok(None),
        };
        let number_len = match Self::to_numeric(vm, heap, state, value)? {
            Some(n) => n,
            None => return Ok(None),
        };
        if number_len != new_len as f64 {
            return Err(VmError::Overflow);
        }
        Ok(Some(new_len))
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
    ) -> Result<Flow<bool>, VmError> {
        if partial.is_accessor_descriptor()
            || partial.configurable == Some(true)
            || partial.enumerable == Some(true)
        {
            return should_throw.reject();
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
        let Some(new_len) = Self::anything_to_array_length(vm, heap, state, value)? else {
            return Ok(Flow::Threw);
        };
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
            return should_throw.reject();
        }
        let shrunk = Self::set_array_length(heap, scope, &array, new_len)?;
        if partial.writable == Some(false) {
            let attrs = PartialDescriptor {
                writable: Some(false),
                ..PartialDescriptor::default()
            };
            let current = Self::own_property_ordinary(heap, scope, array_value, length_value);
            match Self::define_own_property_apply(
                Some(vm),
                Some(state),
                heap,
                scope,
                array,
                length_name,
                current,
                &attrs,
                should_throw,
            )? {
                Flow::Threw => return Ok(Flow::Threw),
                Flow::Value(_) => {}
            }
        }
        if shrunk {
            Ok(Flow::Value(true))
        } else {
            should_throw.reject()
        }
    }
}
