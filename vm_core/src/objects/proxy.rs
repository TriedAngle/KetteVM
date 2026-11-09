use core::alloc::Layout;

use crate::Coercion;
use crate::error::Message;
use crate::{
    Args, Compare, Convert, EdgeVisitable, GcSlot, Handle, HandleScope, Header, Heap, HeapObject,
    HostCtx, LanguageMode, Map, MaybeOwnProperty, Object, ObjectKind, OwnProperty,
    PartialDescriptor, PropertyDescriptor, Prototype, SetApplied, ShouldThrow, SlotName,
    StoreSemantics, Tagged, ThreadState, Transition, VM, Value, Visitor, VmError,
    descriptor_object,
};

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

/// `Ok(Flow::Threw)` means user code threw and the pending exception is
/// set (the caller surfaces the exception sentinel).
pub enum Flow<T> {
    Threw,
    Value(T),
}

/// The thirteen proxy traps (ES 20.2).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Trap {
    Get,
    Set,
    Has,
    DeleteProperty,
    GetOwnPropertyDescriptor,
    DefineProperty,
    GetPrototypeOf,
    SetPrototypeOf,
    IsExtensible,
    PreventExtensions,
    OwnKeys,
    Apply,
    Construct,
}

impl Trap {
    fn name<'a>(self, heap: &'a Heap) -> Tagged<'a, SlotName> {
        let s = &heap.known().strings;
        match self {
            Self::Get => s.get.get(heap),
            Self::Set => s.set.get(heap),
            Self::Has => s.has.get(heap),
            Self::DeleteProperty => s.delete_property.get(heap),
            Self::GetOwnPropertyDescriptor => s.get_own_property_descriptor.get(heap),
            Self::DefineProperty => s.define_property.get(heap),
            Self::GetPrototypeOf => s.get_prototype_of.get(heap),
            Self::SetPrototypeOf => s.set_prototype_of.get(heap),
            Self::IsExtensible => s.is_extensible.get(heap),
            Self::PreventExtensions => s.prevent_extensions.get(heap),
            Self::OwnKeys => s.own_keys.get(heap),
            Self::Apply => s.apply.get(heap),
            Self::Construct => s.construct.get(heap),
        }
    }
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

enum TrapLookup<'s> {
    /// undefined/null: forward to the target.
    None,
    /// A callable... verified by `call_trap`.
    Trap(Handle<'s, Value>),
    Threw,
}

fn get_trap<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    handler: &Handle<'_, Value>,
    trap: Trap,
) -> Result<TrapLookup<'s>, VmError> {
    let name = scope.handle(trap.name(heap).erase());
    match Object::get_property(vm, heap, state, *handler, *handler, name, false)? {
        Coercion::Threw => Ok(TrapLookup::Threw),
        Coercion::Value(v) => {
            let v = scope.handle(v);
            let nullish = v.as_tagged(heap) == heap.known().undefined.get(heap)
                || v.as_tagged(heap) == heap.known().null.get(heap);
            if nullish {
                Ok(TrapLookup::None)
            } else {
                Ok(TrapLookup::Trap(v))
            }
        }
    }
}

fn call_trap<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    trap: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
) -> Result<Coercion<'a>, VmError> {
    if !trap.as_tagged(heap).is_callable(heap) {
        return Err(VmError::Message(Message::ProxyTrapNotFunction));
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
    let exception = heap.known().exception.get(heap);
    if result.as_tagged(heap) == exception {
        Ok(Coercion::Threw)
    } else {
        Ok(Coercion::Value(result.as_tagged(heap)))
    }
}

/// Revocation check + (target, handler) extraction — the shared entry
/// of every internal method. Both values come back rooted in `scope`.
fn enter_trap<'s>(
    scope: &'s HandleScope<'_>,
    heap: &mut Heap,
    proxy: &Handle<'_, Value>,
    trap: Trap,
) -> Result<(Handle<'s, Value>, Handle<'s, Value>), VmError> {
    let (target, handler) = parts(heap, proxy.as_tagged(heap)).expect("caller verified a proxy");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        return Err(revoked_error(trap));
    }
    Ok((scope.handle(target), scope.handle(handler)))
}

fn revoked_error(trap: Trap) -> VmError {
    VmError::Message(match trap {
        Trap::Get => Message::ProxyRevokedGet,
        Trap::Set => Message::ProxyRevokedSet,
        Trap::Has => Message::ProxyRevokedHas,
        Trap::DeleteProperty => Message::ProxyRevokedDeleteProperty,
        Trap::GetOwnPropertyDescriptor => Message::ProxyRevokedGetOwnPropertyDescriptor,
        Trap::DefineProperty => Message::ProxyRevokedDefineProperty,
        Trap::GetPrototypeOf => Message::ProxyRevokedGetPrototypeOf,
        Trap::SetPrototypeOf => Message::ProxyRevokedSetPrototypeOf,
        Trap::IsExtensible => Message::ProxyRevokedIsExtensible,
        Trap::PreventExtensions => Message::ProxyRevokedPreventExtensions,
        Trap::OwnKeys => Message::ProxyRevokedOwnKeys,
        Trap::Apply => Message::ProxyRevokedApply,
        Trap::Construct => Message::ProxyRevokedApply,
    })
}

fn own_descriptor_h<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<MaybeOwnProperty<'s>, VmError> {
    get_own_property_h(vm, heap, state, scope, obj, key)
}

fn get_own_property_h<'s>(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &'s HandleScope<'_>,
    obj: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<MaybeOwnProperty<'s>, VmError> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        return Object::get_own_property(vm, heap, state, scope, *obj, *key);
    }
    let (target, handler) = enter_trap(scope, heap, obj, Trap::GetOwnPropertyDescriptor)?;
    match get_trap(
        vm,
        heap,
        state,
        scope,
        &handler,
        Trap::GetOwnPropertyDescriptor,
    )? {
        TrapLookup::Threw => Ok(MaybeOwnProperty::Threw),
        TrapLookup::None => Object::get_own_property(vm, heap, state, scope, target, *key),
        TrapLookup::Trap(t) => {
            let trap_key = materialize_trap_key(heap, scope, key)?;
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target, trap_key])?;
            let result = match result {
                Coercion::Threw => return Ok(MaybeOwnProperty::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            let undefined = heap.known().undefined.get_handle(heap, scope).erase();
            let is_undefined = result.as_tagged(heap).ptr_eq(undefined.as_tagged(heap));
            if !is_undefined && result.as_tagged(heap).is_primitive(heap) {
                return Err(VmError::Type);
            }
            let target_desc = match Object::get_own_property(vm, heap, state, scope, target, *key)?
            {
                MaybeOwnProperty::Threw => return Ok(MaybeOwnProperty::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            if is_undefined {
                let Some(target_desc) = target_desc else {
                    return Ok(MaybeOwnProperty::Missing);
                };
                if !target_desc.desc.is_configurable() {
                    return Err(VmError::Type);
                }
                let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)?
                else {
                    return Ok(MaybeOwnProperty::Threw);
                };
                if !extensible {
                    return Err(VmError::Type);
                }
                return Ok(MaybeOwnProperty::Missing);
            }
            let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)? else {
                return Ok(MaybeOwnProperty::Threw);
            };
            let Some(partial) = Object::to_property_descriptor(vm, heap, state, scope, result)?
            else {
                return Ok(MaybeOwnProperty::Threw);
            };
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
                return Err(VmError::Type);
            }
            if !desc.is_configurable() {
                let Some(target_desc) = target_desc else {
                    return Err(VmError::Type);
                };
                if target_desc.desc.is_configurable() {
                    return Err(VmError::Type);
                }
                if desc.is_data_descriptor()
                    && !desc.is_writable()
                    && target_desc.desc.is_writable()
                {
                    return Err(VmError::Type);
                }
            }
            Ok(MaybeOwnProperty::Found(OwnProperty {
                desc,
                location: None,
            }))
        }
    }
}

fn is_extensible_h(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<Flow<bool>, VmError> {
    let cond_12 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_12 {
        return Ok(Flow::Value({
            obj.as_tagged(heap)
                .as_heap_object()
                .is_some_and(|o| o.map(heap).kind().is_extendable())
        }));
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        return Err(revoked_error(Trap::IsExtensible));
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    match get_trap(vm, heap, state, scope, &handler, Trap::IsExtensible)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => is_extensible_h(vm, heap, state, scope, &target),
        TrapLookup::Trap(t) => {
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            Ok(Flow::Value(result.as_tagged(heap).is_truthy(heap)))
        }
    }
}

// ---- forwards (trap absent → run the operation on the target) ---------------

fn get_prototype_of_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<Flow<Tagged<'a, Value>>, VmError> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        let proto = obj
            .as_tagged(heap)
            .as_heap_object()
            .expect("[[GetPrototypeOf]] receiver is an object")
            .prototype(heap);
        let proto = scope.handle(proto);
        return Ok(Flow::Value(proto.as_tagged(heap)));
    }
    let (target, handler) = enter_trap(scope, heap, obj, Trap::GetPrototypeOf)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::GetPrototypeOf)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        // GetMethod(handler, "getPrototypeOf") is undefined: forward
        TrapLookup::None => get_prototype_of_h(vm, heap, state, scope, &target),
        TrapLookup::Trap(t) => {
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            // step 7: Object or null
            let is_null = result
                .as_tagged(heap)
                .ptr_eq(heap.known().null.get(heap).erase());
            if result.as_tagged(heap).is_primitive(heap) && !is_null {
                return Err(VmError::Message(Message::ProxyGetPrototypeOfNotObject));
            }
            // steps 8-11: a non-extensible target pins the result
            let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)? else {
                return Ok(Flow::Threw);
            };
            if !extensible {
                let target_proto = match get_prototype_of_h(vm, heap, state, scope, &target)? {
                    Flow::Threw => return Ok(Flow::Threw),
                    Flow::Value(v) => scope.handle(v),
                };
                let same = {
                    let result = result.as_tagged(heap);
                    let target_proto = target_proto.as_tagged(heap);
                    Compare::same_value(heap, result, target_proto)
                };
                if !same {
                    return Err(VmError::Message(Message::ProxyGetPrototypeOfMismatch));
                }
            }
            Ok(Flow::Value(result.as_tagged(heap)))
        }
    }
}

fn set_prototype_of_h(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
    proto: &Handle<'_, Value>,
) -> Result<Flow<bool>, VmError> {
    if !Proxy::is_proxy(heap, obj.as_tagged(heap)) {
        let target = scope
            .cast::<Object>(heap, obj.as_tagged(heap))
            .ok_or(VmError::Type)?;
        Object::set_prototype(heap, scope, target, *proto)?;
        return Ok(Flow::Value(true));
    }
    let (target, handler) = enter_trap(scope, heap, obj, Trap::SetPrototypeOf)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::SetPrototypeOf)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => {
            if Proxy::is_proxy(heap, target.as_tagged(heap)) {
                set_prototype_of_h(vm, heap, state, scope, &target, proto)
            } else {
                let target_obj = scope
                    .cast::<Object>(heap, target.as_tagged(heap))
                    .ok_or(VmError::Type)?;
                Object::set_prototype(heap, scope, target_obj, *proto)?;
                Ok(Flow::Value(true))
            }
        }
        TrapLookup::Trap(t) => {
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target, *proto])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            if !result.as_tagged(heap).is_truthy(heap) {
                return Ok(Flow::Value(false));
            }
            // steps 8-11: a non-extensible target pins its prototype
            let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)? else {
                return Ok(Flow::Threw);
            };
            if !extensible {
                let target_proto = match get_prototype_of_h(vm, heap, state, scope, &target)? {
                    Flow::Threw => return Ok(Flow::Threw),
                    Flow::Value(v) => scope.handle(v),
                };
                let same = {
                    let proto = proto.as_tagged(heap);
                    let target_proto = target_proto.as_tagged(heap);
                    Compare::same_value(heap, proto, target_proto)
                };
                if !same {
                    return Err(VmError::Message(Message::ProxySetPrototypeOfMismatch));
                }
            }
            Ok(Flow::Value(true))
        }
    }
}

/// Materialize the spec-visible key argument of a proxy trap. Canonical
/// array indices travel internally as Smis (same as map keys), but traps
/// always observe property keys: numbers are stringified there.
fn materialize_trap_key<'s>(
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
    key: &Handle<'_, Value>,
) -> Result<Handle<'s, Value>, VmError> {
    let key = *key;
    if key.as_tagged(heap).is_smi() {
        let text = Convert::to_string(heap, scope, key)?;
        Ok(scope.handle(text))
    } else {
        Ok(scope.handle(key.as_tagged(heap).erase()))
    }
}

// ---- the internal methods (ES 20.2.5.x) ------------------------------------

fn get_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    receiver: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
) -> Result<Flow<Tagged<'a, Value>>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::Get)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::Get)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => {
            match Object::get_property(vm, heap, state, target, *receiver, *name, false)? {
                Coercion::Threw => Ok(Flow::Threw),
                Coercion::Value(v) => {
                    let v = scope.handle(v);
                    Ok(Flow::Value(v.as_tagged(heap)))
                }
            }
        }
        TrapLookup::Trap(t) => {
            let trap_name = materialize_trap_key(heap, scope, name)?;
            let result = call_trap(
                vm,
                heap,
                state,
                scope,
                &t,
                &[handler, target, trap_name, *receiver],
            )?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            let desc = match own_descriptor_h(vm, heap, state, scope, &target, name)? {
                MaybeOwnProperty::Threw => return Ok(Flow::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            if let Some(d) = desc.as_ref().filter(|d| !d.desc.is_configurable()) {
                let undefined = heap.known().undefined.get(heap).erase();
                match &d.desc {
                    PropertyDescriptor::Data { value, .. } if !d.desc.is_writable() => {
                        if !Compare::same_value(heap, result.as_tagged(heap), value.as_tagged(heap))
                        {
                            return Err(VmError::Message(Message::ProxyGetMismatch));
                        }
                    }
                    PropertyDescriptor::Accessor { get, .. }
                        if get.as_tagged(heap).ptr_eq(undefined)
                            && !result.as_tagged(heap).ptr_eq(undefined) =>
                    {
                        return Err(VmError::Message(Message::ProxyGetAccessorUndefined));
                    }
                    _ => {}
                }
            }
            Ok(Flow::Value(result.as_tagged(heap)))
        }
    }
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
) -> Result<Flow<bool>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::Set)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::Set)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => match Object::set_property(
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
            Flow::Threw => Ok(Flow::Threw),
            Flow::Value(SetApplied::Rejected) => Ok(Flow::Value(false)),
            Flow::Value(_) => Ok(Flow::Value(true)),
        },
        TrapLookup::Trap(t) => {
            let trap_name = materialize_trap_key(heap, scope, name)?;
            let result = call_trap(
                vm,
                heap,
                state,
                scope,
                &t,
                &[handler, target, trap_name, *value, *receiver],
            )?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            if !result.as_tagged(heap).is_truthy(heap) {
                return Ok(Flow::Value(false));
            }
            let desc = match own_descriptor_h(vm, heap, state, scope, &target, name)? {
                MaybeOwnProperty::Threw => return Ok(Flow::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            if let Some(d) = desc.as_ref().filter(|d| !d.desc.is_configurable()) {
                match &d.desc {
                    PropertyDescriptor::Data { value: current, .. } if !d.desc.is_writable() => {
                        if !Compare::same_value(
                            heap,
                            value.as_tagged(heap),
                            current.as_tagged(heap),
                        ) {
                            return Err(VmError::Message(Message::ProxySetMismatch));
                        }
                    }
                    PropertyDescriptor::Accessor { set, .. }
                        if set
                            .as_tagged(heap)
                            .ptr_eq(heap.known().undefined.get(heap).erase()) =>
                    {
                        return Err(VmError::Message(Message::ProxySetAccessorNoSetter));
                    }
                    _ => {}
                }
            }
            Ok(Flow::Value(true))
        }
    }
}

fn has_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    name: &Handle<'_, Value>,
) -> Result<Flow<bool>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::Has)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::Has)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => match Object::has_property(vm, heap, state, target, *name)? {
            Coercion::Threw => Ok(Flow::Threw),
            Coercion::Value(v) => {
                let v = scope.handle(v);
                Ok(Flow::Value(v.as_tagged(heap).is_truthy(heap)))
            }
        },
        TrapLookup::Trap(t) => {
            let trap_name = materialize_trap_key(heap, scope, name)?;
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target, trap_name])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            if result.as_tagged(heap).is_truthy(heap) {
                return Ok(Flow::Value(true));
            }
            let desc = match own_descriptor_h(vm, heap, state, scope, &target, name)? {
                MaybeOwnProperty::Threw => return Ok(Flow::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            if let Some(d) = desc {
                if !d.desc.is_configurable() {
                    return Err(VmError::Message(Message::ProxyHasHiddenConfigurable));
                }
                let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)?
                else {
                    return Ok(Flow::Threw);
                };
                if !extensible {
                    return Err(VmError::Message(Message::ProxyHasHiddenNonExtensible));
                }
            }
            Ok(Flow::Value(false))
        }
    }
}

fn delete_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    key: &Handle<'_, Value>,
) -> Result<Flow<bool>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::DeleteProperty)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::DeleteProperty)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => Object::delete_property(vm, heap, state, target, *key),
        TrapLookup::Trap(t) => {
            let trap_key = materialize_trap_key(heap, scope, key)?;
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target, trap_key])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            if !result.as_tagged(heap).is_truthy(heap) {
                return Ok(Flow::Value(false));
            }
            let desc = match own_descriptor_h(vm, heap, state, scope, &target, key)? {
                MaybeOwnProperty::Threw => return Ok(Flow::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            if let Some(d) = desc {
                if !d.desc.is_configurable() {
                    return Err(VmError::Message(Message::ProxyDeleteConfigurable));
                }
                let Flow::Value(extensible) = is_extensible_h(vm, heap, state, scope, &target)?
                else {
                    return Ok(Flow::Threw);
                };
                if !extensible {
                    return Err(VmError::Message(Message::ProxyDeleteNonExtensible));
                }
            }
            Ok(Flow::Value(true))
        }
    }
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
) -> Result<Flow<bool>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::DefineProperty)?;

    match get_trap(vm, heap, state, scope, &handler, Trap::DefineProperty)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => Object::define_own_property(
            vm,
            heap,
            state,
            scope,
            target,
            *name,
            partial,
            should_throw,
        ),
        TrapLookup::Trap(t) => {
            let desc_obj = descriptor_object(heap, scope, &partial)?;
            let trap_name = materialize_trap_key(heap, scope, name)?;
            let result = call_trap(
                vm,
                heap,
                state,
                scope,
                &t,
                &[handler, target, trap_name, desc_obj],
            )?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            let cond_17 = result.as_tagged(heap).is_truthy(heap);
            if !cond_17 {
                return Ok(Flow::Value(false));
            }
            // targetDesc first, then IsExtensible (ES 10.5.6 steps 11-12)
            let desc = match own_descriptor_h(vm, heap, state, scope, &target, name)? {
                MaybeOwnProperty::Threw => return Ok(Flow::Threw),
                MaybeOwnProperty::Missing => None,
                MaybeOwnProperty::Found(own) => Some(own),
            };
            let ext = is_extensible_h(vm, heap, state, scope, &target)?;
            let Flow::Value(extensible) = ext else {
                return Ok(Flow::Threw);
            };
            let setting_config_false = partial.configurable == Some(false);
            match desc.as_ref() {
                None => {
                    if !extensible {
                        return Err(VmError::Message(Message::ProxyDefineNonExtensible));
                    }
                    if setting_config_false {
                        return Err(VmError::Message(Message::ProxyDefineNewConfigurable));
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
                        return Err(VmError::Message(Message::ProxyDefineIncompatible));
                    }
                    if setting_config_false && d.desc.is_configurable() {
                        return Err(VmError::Message(Message::ProxyDefineUnconfigure));
                    }
                    if !d.desc.is_configurable()
                        && d.desc.is_data_descriptor()
                        && d.desc.is_writable()
                        && partial.is_data_descriptor()
                        && partial.writable == Some(false)
                    {
                        return Err(VmError::Message(Message::ProxyDefineUnwritable));
                    }
                }
            }
            // the trap owns the define; its true result stands
            Ok(Flow::Value(true))
        }
    }
}

fn apply_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
) -> Result<Flow<Tagged<'a, Value>>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::Apply)?;
    let this_arg = match args.first() {
        Some(h) => *h,
        None => heap.known().undefined.get_handle(heap, scope).erase(),
    };
    match get_trap(vm, heap, state, scope, &handler, Trap::Apply)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => {
            let mut all: Vec<Tagged<'_, Value>> = Vec::with_capacity(args.len());
            all.push(this_arg.as_tagged(heap).erase());
            for h in &args[1..] {
                all.push(h.as_tagged(heap).erase());
            }
            let staged = scope.stage(&all);
            let result = scope.handle(HostCtx::enter(
                vm,
                state.runtime(vm, heap),
                &mut *heap,
                state,
                target,
                staged,
                None,
            )?);
            let exception = heap.known().exception.get(heap);
            if result.as_tagged(heap) == exception {
                Ok(Flow::Threw)
            } else {
                Ok(Flow::Value(result.as_tagged(heap)))
            }
        }
        TrapLookup::Trap(t) => {
            // args: (target, thisArg, argumentsList)
            let arr = heap
                .new_array(scope, scope.stage_handles(&args[1..]))
                .as_handle(scope)
                .erase();
            let result = call_trap(
                vm,
                heap,
                state,
                scope,
                &t,
                &[handler, target, this_arg, arr],
            )?;
            match result {
                Coercion::Threw => Ok(Flow::Threw),
                Coercion::Value(v) => Ok(Flow::Value(v)),
            }
        }
    }
}

fn construct_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    proxy: &Handle<'_, Value>,
    args: &[Handle<'_, Value>],
    new_target: &Handle<'_, Value>,
) -> Result<Flow<Tagged<'a, Value>>, VmError> {
    let (target, handler) = enter_trap(scope, heap, proxy, Trap::Construct)?;
    match get_trap(vm, heap, state, scope, &handler, Trap::Construct)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => {
            // forward: the full target [[Construct]]
            match Object::construct_forward(vm, heap, state, target, args, *new_target)? {
                Coercion::Threw => Ok(Flow::Threw),
                Coercion::Value(v) => Ok(Flow::Value(v)),
            }
        }
        TrapLookup::Trap(t) => {
            // args: (target, argumentsList, newTarget)
            let arr = heap
                .new_array(scope, scope.stage_handles(args))
                .as_handle(scope)
                .erase();
            let result = call_trap(
                vm,
                heap,
                state,
                scope,
                &t,
                &[handler, target, arr, *new_target],
            )?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            // invariant: the trap must return an object
            {
                let cond_19 = result.as_tagged(heap).is_primitive(heap);
                if cond_19 {
                    return Err(VmError::Message(Message::ProxyConstructNotObject));
                }
            }
            Ok(Flow::Value(result.as_tagged(heap)))
        }
    }
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
) -> Result<Flow<bool>, VmError> {
    let cond_20 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_20 {
        let Some(obj) = scope.cast::<Object>(heap, obj.as_tagged(heap)) else {
            return Err(VmError::Type);
        };
        prevent_extensions_ordinary(heap, scope, obj);
        return Ok(Flow::Value(true));
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        return Err(revoked_error(Trap::PreventExtensions));
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    match get_trap(vm, heap, state, scope, &handler, Trap::PreventExtensions)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => prevent_extensions_h(vm, heap, state, scope, &target),
        TrapLookup::Trap(t) => {
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            let cond_21 = result.as_tagged(heap).is_truthy(heap);
            if !cond_21 {
                return Ok(Flow::Value(false));
            }
            // invariant: returning true requires a non-extensible target
            let ext = is_extensible_h(vm, heap, state, scope, &target)?;
            let Flow::Value(extensible) = ext else {
                return Ok(Flow::Threw);
            };
            if extensible {
                return Err(VmError::Message(Message::ProxyPreventExtensionsTrue));
            }
            Ok(Flow::Value(true))
        }
    }
}

fn is_extensible_entry_h<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    obj: &Handle<'_, Value>,
) -> Result<Flow<bool>, VmError> {
    let cond_22 = Proxy::is_proxy(heap, obj.as_tagged(heap));
    if !cond_22 {
        let extensible = obj
            .as_tagged(heap)
            .as_heap_object()
            .is_some_and(|o| o.map(heap).kind().is_extendable());
        return Ok(Flow::Value(extensible));
    }
    let (target, handler) = parts(heap, obj.as_tagged(heap)).expect("checked proxy above");
    let revoked = handler == heap.known().null.get(heap);
    if revoked {
        return Err(revoked_error(Trap::IsExtensible));
    }
    let target = scope.handle(target);
    let handler = scope.handle(handler);
    match get_trap(vm, heap, state, scope, &handler, Trap::IsExtensible)? {
        TrapLookup::Threw => Ok(Flow::Threw),
        TrapLookup::None => is_extensible_entry_h(vm, heap, state, scope, &target),
        TrapLookup::Trap(t) => {
            let result = call_trap(vm, heap, state, scope, &t, &[handler, target])?;
            let result = match result {
                Coercion::Threw => return Ok(Flow::Threw),
                Coercion::Value(v) => scope.handle(v),
            };
            let trap_bool = result.as_tagged(heap).is_truthy(heap);
            let target_bool = is_extensible_h(vm, heap, state, scope, &target)?;
            let Flow::Value(target_bool) = target_bool else {
                return Ok(Flow::Threw);
            };
            if trap_bool != target_bool {
                return Err(VmError::Message(Message::ProxyExtensibilityMismatch));
            }
            Ok(Flow::Value(trap_bool))
        }
    }
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
    ) -> Result<MaybeOwnProperty<'s>, VmError> {
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
    ) -> Result<Flow<bool>, VmError> {
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
    ) -> Result<Flow<Tagged<'a, Value>>, VmError> {
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
    ) -> Result<Flow<bool>, VmError> {
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
    ) -> Result<Flow<bool>, VmError> {
        proxy_define_h(vm, heap, state, scope, &obj, &name, partial, should_throw)
    }

    /// Proxy `[[Get]]` (ES 20.2.5.8). `receiver` is the [[Get]] receiver —
    /// the proxy itself at entry points, the *original* receiver when
    /// forwarding through a chain of proxies.
    pub fn get<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        receiver: Handle<'_, Value>,
        name: Handle<'_, Value>,
    ) -> Result<Flow<Tagged<'a, Value>>, VmError> {
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
    ) -> Result<Flow<bool>, VmError> {
        state.handle_scope(|scope| set_h(vm, heap, state, &scope, &proxy, &name, &value, &receiver))
    }

    /// Proxy `[[HasProperty]]` (ES 20.2.5.9).
    pub fn has<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        name: Handle<'_, Value>,
    ) -> Result<Flow<bool>, VmError> {
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
    ) -> Result<Flow<bool>, VmError> {
        state.handle_scope(|scope| delete_h(vm, heap, state, &scope, &proxy, &key))
    }

    /// Proxy `[[Call]]` (ES 20.2.5.15): the `apply` trap, or a plain call
    /// of the target. `args` includes the receiver (`this`) at index 0.
    pub fn apply<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        proxy: Handle<'_, Value>,
        args: Args,
    ) -> Result<Flow<Tagged<'a, Value>>, VmError> {
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
    ) -> Result<Flow<Tagged<'a, Value>>, VmError> {
        state.handle_scope(|scope| {
            let args: Vec<Handle<'_, Value>> =
                (0..args.len()).map(|i| args.get_handle(heap, i)).collect();
            construct_h(vm, heap, state, &scope, &proxy, &args, &new_target)
        })
    }

    /// Proxy `[[PreventExtensions]]` (ES 20.2.5.3), plus the ordinary path —
    /// the `Object.preventExtensions` implementation.
    pub fn prevent_extensions<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
    ) -> Result<Flow<bool>, VmError> {
        state.handle_scope(|scope| prevent_extensions_h(vm, heap, state, &scope, &obj))
    }

    /// `Object.isExtensible` (ES 20.1.2.14) including the proxy trap and
    /// its must-match-target invariant (ES 20.2.5.2 step 8).
    pub fn object_is_extensible<'a>(
        vm: &VM,
        heap: &'a mut Heap,
        state: &ThreadState,
        obj: Handle<'_, Value>,
    ) -> Result<Flow<bool>, VmError> {
        state.handle_scope(|scope| is_extensible_entry_h(vm, heap, state, &scope, &obj))
    }
}
