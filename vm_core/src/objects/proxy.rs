use crate::{
    Args, Compare, Convert, DenseString, EdgeVisitable, GcSlot, Handle, HandleScope, Header, Heap,
    HeapObject, HostCtx, Key, LanguageMode, Map, Object, ObjectKind, OwnProperty,
    PartialDescriptor, PropertyDescriptor, Prototype, Result, SetApplied, ShouldThrow, SlotName,
    Smi, StoreSemantics, Symbol, Tagged, ThreadState, Transition, VM, Value, Visitor, VmError,
    bail,
};
use core::alloc::Layout;

pub struct Proxy;

#[repr(C)]
pub struct ProxyObject {
    pub header: Header,
    pub target: GcSlot,
    pub handler: GcSlot,
}

pub struct ProxyInit<'a> {
    pub map: Handle<'a, Map>,
    pub target: Handle<'a, Value>,
    pub handler: Handle<'a, Value>,
}

impl ProxyObject {
    pub fn layout_for() -> Layout {
        Layout::new::<Self>()
    }

    /// Whether the proxy has been revoked (handler nulled).
    pub fn is_revoked(&self, heap: &Heap) -> bool {
        self.handler.raw() == heap.known().null.get(heap)
    }

    /// (target, handler) anchored to `heap`; caller checks revocation.
    pub fn parts<'a>(&self, heap: &'a Heap) -> (Tagged<'a, Value>, Tagged<'a, Value>) {
        (self.target.get(heap), self.handler.get(heap))
    }
}

impl HeapObject for ProxyObject {
    const KIND: ObjectKind = ObjectKind::Proxy;
    type Init<'a> = ProxyInit<'a>;

    fn layout_for(_config: &Self::Init<'_>) -> Layout {
        Self::layout_for()
    }

    fn init(&mut self, heap: &Heap, config: &Self::Init<'_>) {
        let host = self.tagged(heap);
        self.header.map.set(heap, host, config.map.as_tagged(heap));
        self.target
            .set(heap, host, config.target.as_tagged(heap).erase());
        self.handler
            .set(heap, host, config.handler.as_tagged(heap).erase());
    }

    fn header(&self) -> &Header {
        &self.header
    }

    fn layout(&self) -> Layout {
        Self::layout_for()
    }
}

impl EdgeVisitable for ProxyObject {
    fn visit_edges(&self, visitor: &mut dyn Visitor) {
        visitor.visit(self.header.map.as_raw());
        visitor.visit(self.target.as_raw());
        visitor.visit(self.handler.as_raw());
    }
}

macro_rules! traps {
    ($( $variant:ident => $name:literal, $field:ident );* $(;)?) => {
        /// The thirteen proxy traps (ES 20.2).
        #[derive(Debug, Copy, Clone, PartialEq, Eq)]
        pub enum Trap {
            $( $variant, )*
        }

        impl Trap {
            /// The trap name used in error messages.
            const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $name, )*
                }
            }

            fn name<'a>(self, heap: &'a Heap) -> Tagged<'a, SlotName> {
                let s = &heap.known().strings;
                match self {
                    $( Self::$variant => s.$field.get(heap), )*
                }
            }
        }
    };
}

traps! {
    Get => "get", get;
    Set => "set", set;
    Has => "has", has;
    DeleteProperty => "deleteProperty", delete_property;
    GetOwnPropertyDescriptor => "getOwnPropertyDescriptor", get_own_property_descriptor;
    DefineProperty => "defineProperty", define_property;
    GetPrototypeOf => "getPrototypeOf", get_prototype_of;
    SetPrototypeOf => "setPrototypeOf", set_prototype_of;
    IsExtensible => "isExtensible", is_extensible;
    PreventExtensions => "preventExtensions", prevent_extensions;
    OwnKeys => "ownKeys", own_keys;
    Apply => "apply", apply;
    Construct => "construct", construct;
}

/// (target, handler) of a proxy, read under a leaf scope. The raw words
/// must be rooted by the caller before any allocation.
fn parts<'a>(
    heap: &'a Heap,
    proxy: Tagged<'a, Value>,
) -> Option<(Tagged<'a, Value>, Tagged<'a, Value>)> {
    let p = proxy.get_as::<ProxyObject>(heap)?;
    Some(p.parts(heap))
}

/// `None` means the trap is undefined/null; `Some` is a callable verified
/// by `call_trap`.
fn get_trap<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    handler: &Handle<'_, Value>,
    trap: Trap,
) -> Result<Option<Handle<'s, Value>>> {
    let name = scope.handle(trap.name(heap).erase());
    let value = Object::get_property(vm, heap, state, *handler, *handler, name, false)?;
    let v = scope.handle(value);
    let nullish = v.as_tagged(heap) == heap.known().undefined.get(heap)
        || v.as_tagged(heap) == heap.known().null.get(heap);
    if nullish { Ok(None) } else { Ok(Some(v)) }
}

fn call_trap<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    trap: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
) -> Result<Tagged<'a, Value>> {
    if !trap.as_tagged(heap).is_callable(heap) {
        bail!(vm, heap, state, VmError::ProxyTrapNotFunction);
    }
    let staged = scope.stage_handles(args);
    let result = scope.handle(HostCtx::enter(
        vm,
        state.runtime(vm, heap),
        &mut *heap,
        state,
        *trap,
        staged,
        None,
    )?);
    Ok(result.as_tagged(heap))
}

/// Revocation check + (target, handler) extraction — the shared entry
/// of every internal method. Both values come back rooted in `scope`.
fn enter_trap<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    trap: Trap,
) -> Result<(Handle<'s, Value>, Handle<'s, Value>)> {
    let (target, handler) = parts(heap, proxy.as_tagged(heap)).expect("caller verified a proxy");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        bail!(vm, heap, state, VmError::ProxyRevoked(trap.as_str()));
    }
    Ok((scope.handle(target), scope.handle(handler)))
}

fn same_property_key(heap: &Heap, a: Tagged<'_, Value>, b: Tagged<'_, Value>) -> bool {
    match (a.classify_key(heap), b.classify_key(heap)) {
        (Some(Key::Element(a)), Some(Key::Element(b))) => a == b,
        (Some(Key::Name(a)), Some(Key::Name(b))) => a.erase().ptr_eq(b.erase()),
        _ => false,
    }
}

fn take_property_key(
    heap: &Heap,
    keys: &mut Vec<Handle<'_, Value>>,
    key: Handle<'_, Value>,
) -> bool {
    let Some(position) = keys
        .iter()
        .position(|item| same_property_key(heap, item.as_tagged(heap), key.as_tagged(heap)))
    else {
        return false;
    };
    keys.remove(position);
    true
}

/// `CreateListFromArrayLike ( trapResultArray, property-key )`
/// [ES 7.3.19](https://tc39.es/ecma262/#sec-createlistfromarraylike)
fn trap_result_keys<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    result: Handle<'_, Value>,
) -> Result<Vec<Handle<'s, Value>>> {
    if result.as_tagged(heap).is_primitive(heap) {
        bail!(vm, heap, state, VmError::CreateListFromArrayLikeNonObject);
    }
    let length = Object::length_of_array_like(vm, heap, state, scope, result)?;
    let mut keys: Vec<Handle<'s, Value>> = Vec::with_capacity(length as usize);
    for index in 0..length {
        let key = scope.handle(Smi::new(index as i64).into_tagged());
        let value = scope.handle(Object::get_property(
            vm,
            heap,
            state,
            result,
            result,
            key.erase(),
            false,
        )?);
        if value.as_tagged(heap).get_as::<DenseString>(heap).is_none()
            && value.as_tagged(heap).get_as::<Symbol>(heap).is_none()
        {
            bail!(vm, heap, state, VmError::ProxyOwnKeysInvalidKey);
        }
        if keys
            .iter()
            .any(|other| same_property_key(heap, other.as_tagged(heap), value.as_tagged(heap)))
        {
            bail!(vm, heap, state, VmError::ProxyOwnKeysDuplicate);
        }
        keys.push(value);
    }
    Ok(keys)
}

fn own_descriptor_h<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<Option<OwnProperty<'s>>> {
    get_own_property_h(vm, heap, state, scope, obj, key)
}

fn get_own_property_h<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<Option<OwnProperty<'s>>> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        return Object::get_own_property(vm, heap, state, scope, *obj, *key);
    }
    let (target, handler) =
        enter_trap(vm, heap, state, scope, obj, Trap::GetOwnPropertyDescriptor)?;
    let Some(t) = get_trap(
        vm,
        heap,
        state,
        scope,
        &handler,
        Trap::GetOwnPropertyDescriptor,
    )?
    else {
        return Object::get_own_property(vm, heap, state, scope, target, *key);
    };
    let trap_key = materialize_trap_key(vm, heap, state, scope, key)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_key],
    )?);
    let undefined = heap.known().undefined.get_handle(heap, scope).erase();
    let is_undefined = result.as_tagged(heap).ptr_eq(undefined.as_tagged(heap));
    if !is_undefined && result.as_tagged(heap).is_primitive(heap) {
        bail!(
            vm,
            heap,
            state,
            VmError::ProxyGetOwnPropertyDescriptorInvalid
        );
    }
    let target_desc = Object::get_own_property(vm, heap, state, scope, target, *key)?;
    if is_undefined {
        let Some(target_desc) = target_desc else {
            return Ok(None);
        };
        if !target_desc.desc.is_configurable() {
            bail!(
                vm,
                heap,
                state,
                VmError::ProxyGetOwnPropertyDescriptorNonConfigurable
            );
        }
        let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
        if !extensible {
            bail!(
                vm,
                heap,
                state,
                VmError::ProxyGetOwnPropertyDescriptorNonExtensible
            );
        }
        return Ok(None);
    }
    let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
    let partial = Object::to_property_descriptor(vm, heap, state, scope, result)?;
    let desc = partial.complete_against(undefined, None);
    let target_partial = target_desc
        .as_ref()
        .map(|d| PartialDescriptor::from(&d.desc));
    let desc_partial = PartialDescriptor::from(&desc);
    if !Transition::is_compatible_property_descriptor(
        heap,
        extensible,
        &desc_partial,
        target_partial.as_ref(),
    ) {
        bail!(
            vm,
            heap,
            state,
            VmError::ProxyGetOwnPropertyDescriptorIncompatible
        );
    }
    if !desc.is_configurable() {
        let Some(target_desc) = target_desc else {
            bail!(
                vm,
                heap,
                state,
                VmError::ProxyGetOwnPropertyDescriptorNewConfigurable
            );
        };
        if target_desc.desc.is_configurable() {
            bail!(
                vm,
                heap,
                state,
                VmError::ProxyGetOwnPropertyDescriptorUnconfigure
            );
        }
        if desc.is_data_descriptor() && !desc.is_writable() && target_desc.desc.is_writable() {
            bail!(
                vm,
                heap,
                state,
                VmError::ProxyGetOwnPropertyDescriptorUnwritable
            );
        }
    }
    Ok(Some(OwnProperty {
        desc,
        location: None,
    }))
}

fn is_extensible_h(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<bool> {
    let cond_12 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_12 {
        return Ok(obj
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_extendable()));
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        bail!(
            vm,
            heap,
            state,
            VmError::ProxyRevoked(Trap::IsExtensible.as_str())
        );
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::IsExtensible)? else {
        return is_extensible_h(vm, heap, state, scope, &target);
    };
    let result = scope.handle(call_trap(vm, heap, state, scope, &t, &[handler, target])?);
    Ok(result.as_tagged(heap).is_truthy(heap))
}

// ---- forwards (trap absent → run the operation on the target) ---------------

fn get_prototype_of_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<Tagged<'a, Value>> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        let proto = obj
            .as_tagged(heap)
            .as_heap_object()
            .expect("[[GetPrototypeOf]] receiver is an object")
            .prototype(heap);
        let proto = scope.handle(proto);
        return Ok(proto.as_tagged(heap));
    }
    let (target, handler) = enter_trap(vm, heap, state, scope, obj, Trap::GetPrototypeOf)?;
    // GetMethod(handler, "getPrototypeOf") is undefined: forward
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::GetPrototypeOf)? else {
        return get_prototype_of_h(vm, heap, state, scope, &target);
    };
    let result = scope.handle(call_trap(vm, heap, state, scope, &t, &[handler, target])?);
    // step 7: Object or null
    let is_null = result
        .as_tagged(heap)
        .ptr_eq(heap.known().null.get(heap).erase());
    if result.as_tagged(heap).is_primitive(heap) && !is_null {
        bail!(vm, heap, state, VmError::ProxyGetPrototypeOfNotObject);
    }
    // steps 8-11: a non-extensible target pins the result
    let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
    if !extensible {
        let target_proto = scope.handle(get_prototype_of_h(vm, heap, state, scope, &target)?);
        let same = {
            let result = result.as_tagged(heap);
            let target_proto = target_proto.as_tagged(heap);
            Compare::same_value(heap, result, target_proto)
        };
        if !same {
            bail!(vm, heap, state, VmError::ProxyGetPrototypeOfMismatch);
        }
    }
    Ok(result.as_tagged(heap))
}

fn set_prototype_of_h(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
    proto: &Handle<'_, Value>,
) -> Result<bool> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        let Some(target) = scope.cast::<Object>(heap, obj.as_tagged(heap)) else {
            bail!(vm, heap, state, VmError::PrototypeReceiverNotObject);
        };
        Object::set_prototype(vm, heap, state, scope, target, *proto)?;
        return Ok(true);
    }
    let (target, handler) = enter_trap(vm, heap, state, scope, obj, Trap::SetPrototypeOf)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::SetPrototypeOf)? else {
        if Proxy::is_proxy(heap, target.as_tagged(heap)) {
            return set_prototype_of_h(vm, heap, state, scope, &target, proto);
        }
        let Some(target_obj) = scope.cast::<Object>(heap, target.as_tagged(heap)) else {
            bail!(vm, heap, state, VmError::PrototypeTargetNotObject);
        };
        Object::set_prototype(vm, heap, state, scope, target_obj, *proto)?;
        return Ok(true);
    };
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, *proto],
    )?);
    if !result.as_tagged(heap).is_truthy(heap) {
        return Ok(false);
    }
    // steps 8-11: a non-extensible target pins its prototype
    let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
    if !extensible {
        let target_proto = scope.handle(get_prototype_of_h(vm, heap, state, scope, &target)?);
        let same = {
            let proto = proto.as_tagged(heap);
            let target_proto = target_proto.as_tagged(heap);
            Compare::same_value(heap, proto, target_proto)
        };
        if !same {
            bail!(vm, heap, state, VmError::ProxySetPrototypeOfMismatch);
        }
    }
    Ok(true)
}

/// Materialize the spec-visible key argument of a proxy trap. Canonical
/// array indices travel internally as Smis (same as map keys), but traps
/// always observe property keys: numbers are stringified there.
fn materialize_trap_key<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    key: &Handle<'_, Value>,
) -> Result<Handle<'s, Value>> {
    let key = *key;
    if key.as_tagged(heap).is_smi() {
        let text = Convert::to_string(vm, heap, state, scope, key)?;
        Ok(scope.handle(text))
    } else {
        Ok(scope.handle(key.as_tagged(heap).erase()))
    }
}

// ---- the internal methods (ES 20.2.5.x) ------------------------------------

fn get_h<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    receiver: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
) -> Result<Tagged<'a, Value>> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::Get)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::Get)? else {
        return Object::get_property(vm, heap, state, target, *receiver, *name, false);
    };
    let trap_name = materialize_trap_key(vm, heap, state, scope, name)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_name, *receiver],
    )?);
    let desc = own_descriptor_h(vm, heap, state, scope, &target, name)?;
    if let Some(d) = desc.as_ref().filter(|d| !d.desc.is_configurable()) {
        let undefined = heap.known().undefined.get(heap).erase();
        match &d.desc {
            PropertyDescriptor::Data { value, .. } if !d.desc.is_writable() => {
                if !Compare::same_value(heap, result.as_tagged(heap), value.as_tagged(heap)) {
                    bail!(vm, heap, state, VmError::ProxyGetMismatch);
                }
            }
            PropertyDescriptor::Accessor { get, .. }
                if get.as_tagged(heap).ptr_eq(undefined)
                    && !result.as_tagged(heap).ptr_eq(undefined) =>
            {
                bail!(vm, heap, state, VmError::ProxyGetAccessorUndefined);
            }
            _ => {}
        }
    }
    Ok(result.as_tagged(heap))
}

#[allow(clippy::too_many_arguments)]
fn set_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
    value: &Handle<'_, Value>,
    receiver: &Handle<'_, Value>,
) -> Result<bool> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::Set)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::Set)? else {
        return match Object::set_property(
            vm,
            heap,
            state,
            target,
            *receiver,
            *name,
            *value,
            StoreSemantics::Shadow,
            false,
            LanguageMode::Sloppy,
        )? {
            SetApplied::Rejected => Ok(false),
            _ => Ok(true),
        };
    };
    let trap_name = materialize_trap_key(vm, heap, state, scope, name)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_name, *value, *receiver],
    )?);
    if !result.as_tagged(heap).is_truthy(heap) {
        return Ok(false);
    }
    let desc = own_descriptor_h(vm, heap, state, scope, &target, name)?;
    if let Some(d) = desc.as_ref().filter(|d| !d.desc.is_configurable()) {
        match &d.desc {
            PropertyDescriptor::Data { value: current, .. } if !d.desc.is_writable() => {
                if !Compare::same_value(heap, value.as_tagged(heap), current.as_tagged(heap)) {
                    bail!(vm, heap, state, VmError::ProxySetMismatch);
                }
            }
            PropertyDescriptor::Accessor { set, .. }
                if set
                    .as_tagged(heap)
                    .ptr_eq(heap.known().undefined.get(heap).erase()) =>
            {
                bail!(vm, heap, state, VmError::ProxySetAccessorNoSetter);
            }
            _ => {}
        }
    }
    Ok(true)
}

fn has_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
) -> Result<bool> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::Has)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::Has)? else {
        return Object::has_property(vm, heap, state, target, *name);
    };
    let trap_name = materialize_trap_key(vm, heap, state, scope, name)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_name],
    )?);
    if result.as_tagged(heap).is_truthy(heap) {
        return Ok(true);
    }
    let desc = own_descriptor_h(vm, heap, state, scope, &target, name)?;
    if let Some(d) = desc {
        if !d.desc.is_configurable() {
            bail!(vm, heap, state, VmError::ProxyHasNonConfigurable);
        }
        let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
        if !extensible {
            bail!(vm, heap, state, VmError::ProxyHasHiddenNonExtensible);
        }
    }
    Ok(false)
}

fn delete_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<bool> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::DeleteProperty)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::DeleteProperty)? else {
        return Object::delete_property(vm, heap, state, target, *key);
    };
    let trap_key = materialize_trap_key(vm, heap, state, scope, key)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_key],
    )?);
    if !result.as_tagged(heap).is_truthy(heap) {
        return Ok(false);
    }
    let desc = own_descriptor_h(vm, heap, state, scope, &target, key)?;
    if let Some(d) = desc {
        if !d.desc.is_configurable() {
            bail!(vm, heap, state, VmError::ProxyDeleteNonConfigurable);
        }
        let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
        if !extensible {
            bail!(vm, heap, state, VmError::ProxyDeleteNonExtensible);
        }
    }
    Ok(true)
}

fn proxy_define_h(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
    partial: PartialDescriptor,
    should_throw: ShouldThrow,
) -> Result<bool> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::DefineProperty)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::DefineProperty)? else {
        return Object::define_own_property(
            vm,
            heap,
            state,
            scope,
            target,
            *name,
            partial,
            should_throw,
        );
    };
    let desc_obj = Object::descriptor_object(heap, scope, &partial)?;
    let trap_name = materialize_trap_key(vm, heap, state, scope, name)?;
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, trap_name, desc_obj],
    )?);
    if !result.as_tagged(heap).is_truthy(heap) {
        return match should_throw {
            ShouldThrow::DontThrow => Ok(false),
            ShouldThrow::Throw => bail!(vm, heap, state, VmError::ProxyDefineFalsish),
        };
    }
    // targetDesc first, then IsExtensible (ES 10.5.6 steps 11-12)
    let desc = own_descriptor_h(vm, heap, state, scope, &target, name)?;
    let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
    let setting_config_false = partial.configurable == Some(false);
    match desc.as_ref() {
        None => {
            if !extensible {
                bail!(vm, heap, state, VmError::ProxyDefineNonExtensible);
            }
            if setting_config_false {
                bail!(vm, heap, state, VmError::ProxyDefineNewConfigurable);
            }
        }
        Some(d) => {
            let target_partial = PartialDescriptor::from(&d.desc);
            let compatible = Transition::is_compatible_property_descriptor(
                heap,
                extensible,
                &partial,
                Some(&target_partial),
            );
            if !compatible {
                bail!(vm, heap, state, VmError::ProxyDefineIncompatible);
            }
            if setting_config_false && d.desc.is_configurable() {
                bail!(vm, heap, state, VmError::ProxyDefineUnconfigure);
            }
            if !d.desc.is_configurable()
                && d.desc.is_data_descriptor()
                && d.desc.is_writable()
                && partial.is_data_descriptor()
                && partial.writable == Some(false)
            {
                bail!(vm, heap, state, VmError::ProxyDefineUnwritable);
            }
        }
    }
    // the trap owns the define; its true result stands
    Ok(true)
}

fn apply_h<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
) -> Result<Tagged<'a, Value>> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::Apply)?;
    let this_arg = match args.first() {
        Some(h) => *h,
        None => heap.known().undefined.get_handle(heap, scope).erase(),
    };
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::Apply)? else {
        let mut all: Vec<Tagged<'_, Value>> = Vec::with_capacity(args.len());
        all.push(this_arg.as_tagged(heap).erase());
        for h in &args[1..] {
            all.push(h.as_tagged(heap).erase());
        }
        let staged = scope.stage(&all);
        return HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            &mut *heap,
            state,
            target,
            staged,
            None,
        );
    };
    // args: (target, thisArg, argumentsList)
    let arr = heap
        .new_array(scope, scope.stage_handles(&args[1..]))
        .as_handle(scope)
        .erase();
    call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, this_arg, arr],
    )
}

fn construct_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
    new_target: &Handle<'_, Value>,
) -> Result<Tagged<'a, Value>> {
    let (target, handler) = enter_trap(vm, heap, state, scope, proxy, Trap::Construct)?;
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::Construct)? else {
        // forward: the full target [[Construct]]
        return Object::construct_forward(vm, heap, state, target, args, *new_target);
    };
    // args: (target, argumentsList, newTarget)
    let arr = heap
        .new_array(scope, scope.stage_handles(args))
        .as_handle(scope)
        .erase();
    let result = scope.handle(call_trap(
        vm,
        heap,
        state,
        scope,
        &t,
        &[handler, target, arr, *new_target],
    )?);
    // invariant: the trap must return an object
    if result.as_tagged(heap).is_primitive(heap) {
        bail!(vm, heap, state, VmError::ProxyConstructNotObject);
    }
    Ok(result.as_tagged(heap))
}

/// Ordinary `[[PreventExtensions]]`: clone the map without the
/// EXTENDABLE bit (maps are shared, so the clone isolates the object).
fn prevent_extensions_ordinary(heap: &mut Heap, scope: &HandleScope<'_>, obj: Handle<'_, Object>) {
    use crate::{MapInit, MapKind};
    let (kind, prototype, rows) = {
        let map = obj.as_tagged(heap).map(heap);
        (
            map.kind(),
            scope.handle(map.prototype.get(heap)),
            // names are rooted here and re-anchored in the allocating
            // closure: a Tagged cannot escape this non-allocating region
            map.descriptors()
                .iter()
                .map(|d| {
                    (
                        scope.handle(d.name(heap)),
                        d.flags(),
                        scope.handle(d.value.get(heap)),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    if !kind.is_extendable() {
        return; // already non-extensible (idempotent)
    }
    heap.allocate_token_enter_heap(Map::layout_for(rows.len()), |token, heap| {
        let obj_ref = obj.as_tagged(heap);
        let new_map = token.allocate::<Map>(MapInit {
            kind: MapKind::new(kind.bits() & !MapKind::EXTENDABLE.bits()),
            value_slot_count: obj_ref.map(heap).value_slot_count(),
            descriptors: &rows,
            prototype,
        });
        Prototype::shape_changed(heap, obj_ref.map(heap));
        obj_ref.header.map.set(heap, obj_ref.erase(), new_map);
    });
}

fn prevent_extensions_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<bool> {
    let cond_20 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_20 {
        let Some(obj) = scope.cast::<Object>(heap, obj.as_tagged(heap)) else {
            bail!(vm, heap, state, VmError::PreventExtensionsTargetNotObject);
        };
        prevent_extensions_ordinary(heap, scope, obj);
        return Ok(true);
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        bail!(
            vm,
            heap,
            state,
            VmError::ProxyRevoked(Trap::PreventExtensions.as_str())
        );
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::PreventExtensions)? else {
        return prevent_extensions_h(vm, heap, state, scope, &target);
    };
    let result = scope.handle(call_trap(vm, heap, state, scope, &t, &[handler, target])?);
    if !result.as_tagged(heap).is_truthy(heap) {
        return Ok(false);
    }
    // invariant: returning true requires a non-extensible target
    let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
    if extensible {
        bail!(vm, heap, state, VmError::ProxyPreventExtensionsTrue);
    }
    Ok(true)
}

fn is_extensible_entry_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<bool> {
    let cond_22 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_22 {
        return Ok(obj
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_extendable()));
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        bail!(
            vm,
            heap,
            state,
            VmError::ProxyRevoked(Trap::IsExtensible.as_str())
        );
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    let Some(t) = get_trap(vm, heap, state, scope, &handler, Trap::IsExtensible)? else {
        return is_extensible_entry_h(vm, heap, state, scope, &target);
    };
    let result = scope.handle(call_trap(vm, heap, state, scope, &t, &[handler, target])?);
    let trap_bool = result.as_tagged(heap).is_truthy(heap);
    let target_bool = is_extensible_h(vm, heap, state, scope, &target)?;
    if trap_bool != target_bool {
        bail!(vm, heap, state, VmError::ProxyExtensibilityMismatch);
    }
    Ok(trap_bool)
}

impl Proxy {
    /// Fast proxy check: one map-kind read.
    #[inline]
    pub fn is_proxy<'a>(heap: &'a Heap, v: Tagged<'a, Value>) -> bool {
        // bit test on the already-loaded map kind, no kind decode/match
        v.as_heap_object()
            .is_some_and(|obj| obj.map(heap).kind().is_proxy())
    }

    /// Whether `v` is a valid ECMAScript receiver ([[ProxyTarget]] /
    /// [[ProxyHandler]] validation, `Reflect.*` / `Object.*` argument
    /// checks)
    pub fn is_js_receiver<'a>(heap: &'a Heap, v: Tagged<'a, Value>) -> bool {
        let Some(obj) = v.as_heap_object() else {
            return false;
        };
        obj.header.map.get(heap).kind().kind().is_js_receiver()
    }

    pub fn allocate<'a>(
        heap: &'a mut Heap,
        scope: &HandleScope<'_>,
        target: Handle<'_, Value>,
        handler: Handle<'_, Value>,
    ) -> Tagged<'a, Value> {
        let target = scope.handle(target.as_tagged(heap));
        let handler = scope.handle(handler.as_tagged(heap));
        let known = heap.known();
        let map = {
            let kind = target
                .as_tagged(heap)
                .as_heap_object()
                .expect("proxy target must be a JSReceiver")
                .header
                .map
                .get(heap)
                .kind();
            if kind.is_constructor() {
                known.proxy_constructor_map.get(heap)
            } else if kind.is_callable() {
                known.proxy_callable_map.get(heap)
            } else {
                known.proxy_map.get(heap)
            }
        };
        let map = scope.handle(map);
        heap.allocate::<ProxyObject>(ProxyInit {
            map,
            target,
            handler,
        })
        .erase()
    }

    pub fn revoke(heap: &mut Heap, proxy: Handle<'_, Value>) {
        let proxy = proxy.as_tagged(heap);
        let Some(p) = proxy.get_as::<ProxyObject>(heap) else {
            return;
        };
        if p.is_revoked(heap) {
            return;
        }
        let host = proxy.erase();
        let null = heap.known().null.get(heap).erase();
        p.target.set(heap, host, null);
        p.handler.set(heap, host, null);
    }

    pub fn get_own_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Result<Option<OwnProperty<'s>>> {
        get_own_property_h(vm, heap, state, scope, &obj, &key)
    }

    /// [[IsExtensible]] through proxies, *without* the trap-must-match
    /// invariant (that check belongs to the `Object.isExtensible` entry
    /// point; other internal methods only read the value).
    pub fn is_extensible(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        obj: Tagged<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| {
            let obj = scope.handle(obj);
            is_extensible_h(vm, heap, state, &scope, &obj)
        })
    }

    /// `[[GetPrototypeOf]]` dispatch: proxies run their `getPrototypeOf`
    /// trap (with full spec validation), ordinary objects read the map's
    /// prototype.
    pub fn get_prototype_of<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| get_prototype_of_h(vm, heap, state, &scope, &obj))
    }

    /// `[[SetPrototypeOf]]` dispatch: proxies run their `setPrototypeOf`
    /// trap (with full spec validation), ordinary objects change the map's
    /// prototype. `false` means the internal method rejected the change.
    pub fn set_prototype_of(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
        proto: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| set_prototype_of_h(vm, heap, state, &scope, &obj, &proto))
    }

    /// Proxy `[[DefineOwnProperty]]` (ES 10.5.6): runs the `defineProperty`
    /// trap with full invariant validation, or re-dispatches on the target
    /// when the trap is absent.
    pub fn define_own_property<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        obj: Handle<'_, Value>,
        name: Handle<'_, Value>,
        partial: PartialDescriptor<'s>,
        should_throw: ShouldThrow,
    ) -> Result<bool> {
        proxy_define_h(vm, heap, state, scope, &obj, &name, partial, should_throw)
    }

    /// Proxy `[[Get]]` (ES 20.2.5.8). `receiver` is the [[Get]] receiver —
    /// the proxy itself at entry points, the *original* receiver when
    /// forwarding through a chain of proxies.
    pub fn get<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        proxy: Handle<'_, Value>,
        receiver: Handle<'_, Value>,
        name: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| get_h(vm, heap, state, &scope, &proxy, &receiver, &name))
    }

    /// Proxy `[[Set]]` (ES 20.2.5.10).
    pub fn set<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        name: Handle<'_, Value>,
        value: Handle<'_, Value>,
        receiver: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| set_h(vm, heap, state, &scope, &proxy, &name, &value, &receiver))
    }

    /// Proxy `[[HasProperty]]` (ES 20.2.5.9).
    pub fn has<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        name: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| has_h(vm, heap, state, &scope, &proxy, &name))
    }

    /// Proxy `[[Delete]]` (ES 20.2.5.4). The strict-mode false→TypeError
    /// translation is the caller's (`delete` sloppy/strict natives).
    pub fn delete<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        key: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| delete_h(vm, heap, state, &scope, &proxy, &key))
    }

    /// Proxy `[[Call]]` (ES 20.2.5.15): the `apply` trap, or a plain call
    /// of the target. `args` includes the receiver (`this`) at index 0.
    pub fn apply<'a>(
        vm: &'a VM,
        heap: &'a mut Heap,
        state: &'a ThreadState,
        proxy: Handle<'_, Value>,
        args: Args,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let args: Vec<Handle<'_, Value>> =
                (0..args.len()).map(|i| args.get_handle(heap, i)).collect();
            apply_h(vm, heap, state, &scope, &proxy, &args)
        })
    }

    /// Proxy `[[Construct]]` (ES 20.2.5.5): the `construct` trap, or a
    /// construct of the target with the original `new.target`. `args` are
    /// the constructor arguments (no synthesized receiver).
    pub fn construct<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        args: Args,
        new_target: Handle<'_, Value>,
    ) -> Result<Tagged<'a, Value>> {
        state.handle_scope(|scope| {
            let args: Vec<Handle<'_, Value>> =
                (0..args.len()).map(|i| args.get_handle(heap, i)).collect();
            construct_h(vm, heap, state, &scope, &proxy, &args, &new_target)
        })
    }

    /// Proxy `[[OwnPropertyKeys]]` (ES 10.5.11), including its invariants.
    pub fn own_keys<'s>(
        vm: &VM,
        heap: &mut Heap,
        state: &ThreadState,
        scope: &'s HandleScope<'_>,
        proxy: Handle<'_, Value>,
    ) -> Result<Vec<Handle<'s, Value>>> {
        let (target, handler) = enter_trap(vm, heap, state, scope, &proxy, Trap::OwnKeys)?;
        let Some(trap) = get_trap(vm, heap, state, scope, &handler, Trap::OwnKeys)? else {
            return Object::own_property_keys(vm, heap, state, scope, target);
        };
        let trap_result = scope.handle(call_trap(
            vm,
            heap,
            state,
            scope,
            &trap,
            &[handler, target],
        )?);
        let trap_result = trap_result_keys(vm, heap, state, scope, trap_result)?;
        let extensible = is_extensible_h(vm, heap, state, scope, &target)?;
        let target_keys = Object::own_property_keys(vm, heap, state, scope, target)?;
        let mut configurable: Vec<Handle<'_, Value>> = Vec::new();
        let mut non_configurable: Vec<Handle<'_, Value>> = Vec::new();
        for key in target_keys {
            let desc = Object::get_own_property(vm, heap, state, scope, target, key)?;
            if desc.is_some_and(|desc| !desc.desc.is_configurable()) {
                non_configurable.push(key);
            } else {
                configurable.push(key);
            }
        }
        if extensible && non_configurable.is_empty() {
            return Ok(trap_result);
        }
        let mut unchecked = trap_result.clone();
        for key in non_configurable {
            if !take_property_key(heap, &mut unchecked, key) {
                bail!(vm, heap, state, VmError::ProxyOwnKeysNonConfigurable);
            }
        }
        if extensible {
            return Ok(trap_result);
        }
        for key in configurable {
            if !take_property_key(heap, &mut unchecked, key) {
                bail!(vm, heap, state, VmError::ProxyOwnKeysNonExtensible);
            }
        }
        if !unchecked.is_empty() {
            bail!(vm, heap, state, VmError::ProxyOwnKeysNonExtensible);
        }
        Ok(trap_result)
    }

    /// Proxy `[[PreventExtensions]]` (ES 20.2.5.3), plus the ordinary path —
    /// the `Object.preventExtensions` implementation.
    pub fn prevent_extensions<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| prevent_extensions_h(vm, heap, state, &scope, &obj))
    }

    /// `Object.isExtensible` (ES 20.1.2.14) including the proxy trap and
    /// its must-match-target invariant (ES 20.2.5.2 step 8).
    pub fn object_is_extensible<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
    ) -> Result<bool> {
        state.handle_scope(|scope| is_extensible_entry_h(vm, heap, state, &scope, &obj))
    }
}
