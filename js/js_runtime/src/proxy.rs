//! ES 28.2: the Proxy constructor and Proxy.revocable.

use vm_core::HostCtx;
use vm_core::Object;
use vm_core::PropertyDescriptor;
use vm_core::error::Message;
use vm_core::proxy::Proxy;
use vm_core::raise_runtime;
use vm_core::rt_try;
use vm_core::{Args, Handle, HandleSlice, Smi, Tagged, Value, VmError};

use crate::JSRuntime;

/// `new Proxy(target, handler)` (ES 20.2.1.1): both must be JSReceivers;
/// the map's capability bits mirror the target's so callability is
/// observable (`typeof`, future `Call`/`Construct` dispatch).
pub fn proxy_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let construct = new_target.is_some();
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if !construct {
        return raise_runtime(vm, heap, state, VmError::Message(Message::ProxyRequiresNew));
    }
    let target = args.get_handle(heap, 1);
    let handler = args.get_handle(heap, 2);
    let ok = Proxy::is_js_receiver(heap, target.as_tagged(heap))
        && Proxy::is_js_receiver(heap, handler.as_tagged(heap));
    if !ok {
        return raise_runtime(
            vm,
            heap,
            state,
            VmError::Message(Message::ProxyBadTargetOrHandler),
        );
    }
    state.handle_scope(|scope| Proxy::allocate(heap, &scope, target, handler))
}

/// `Proxy.revocable(target, handler)` (ES 20.2.2.1): returns
/// `{ proxy, revoke }`; the revoke closure is the JS template installed
/// by REVOKE_PRELUDE (it keeps the idempotence flag and calls the
/// hidden `__revokeProxy` runtime).
pub fn proxy_revocable<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let known = &ctx.runtime::<JSRuntime>().known;
    let idx = &ctx.runtime::<JSRuntime>().indices;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let target = args.get_handle(heap, 1);
    let handler = args.get_handle(heap, 2);
    let ok = Proxy::is_js_receiver(heap, target.as_tagged(heap))
        && Proxy::is_js_receiver(heap, handler.as_tagged(heap));
    if !ok {
        return raise_runtime(
            vm,
            heap,
            state,
            VmError::Message(Message::ProxyBadTargetOrHandler),
        );
    }
    state.handle_scope(|scope| {
        let proxy = scope.handle(Proxy::allocate(heap, &scope, target, handler));
        // revoke = `%__revokeProxy%` bound to the proxy (ES 20.2.4: a
        // built-in function with [[RevocableProxy]]; the revocation itself
        // is idempotent, so no done-flag is needed). A bound function has
        // no `prototype` own property, like a built-in.
        let target = rt_try!(
            vm,
            heap,
            state,
            Object::native_function(heap, &scope, idx.proxy_revoke)
        );
        let bound_args =
            scope.handle(heap.new_array(&scope, scope.stage(&[proxy.as_tagged(heap)])));
        let map = vm.known().bound_function_map.get_handle(heap, &scope);
        let slots = scope.stage(&[
            target.as_tagged(heap).erase(),
            vm.known().undefined.get(heap).erase(),
            bound_args.as_tagged(heap).erase(),
        ]);
        let bound = scope.handle(heap.new_object(&scope, map, slots));
        let prototype = vm
            .known()
            .function_prototype
            .get_handle(heap, &scope)
            .erase();
        rt_try!(
            vm,
            heap,
            state,
            Object::set_prototype(heap, &scope, bound, prototype)
        );
        // name "revoke", length 0 (the bound argument), neither enumerable
        let name_key = vm.known().strings.name.get_handle(heap, &scope);
        let name_value = scope.handle(
            vm.interner()
                .intern_str(heap, &scope, "revoke")
                .as_tagged(heap)
                .erase(),
        );
        let builtin_attrs = PropertyDescriptor::Data {
            value: name_value,
            writable: false,
            enumerable: false,
            configurable: true,
        };
        let _ = Object::define_own_property(heap, &scope, bound, name_key, builtin_attrs);
        let length_key = vm.known().strings.length.get_handle(heap, &scope);
        let zero: Handle<'_, Value> = scope.handle(Smi::new(0));
        let length_attrs = PropertyDescriptor::Data {
            value: zero,
            writable: false,
            enumerable: false,
            configurable: true,
        };
        let _ = Object::define_own_property(heap, &scope, bound, length_key, length_attrs);
        let revoke = bound.as_tagged(heap).erase();
        let revoke = scope.handle(revoke);
        let map = vm.known().object_initial_map.get_handle(heap, &scope);
        let obj = heap
            .new_object(&scope, map, HandleSlice::EMPTY)
            .as_handle(&scope);
        let proxy_name = known.strings.proxy.get_handle(heap, &scope);
        let _ = Object::define_own_property(
            heap,
            &scope,
            obj,
            proxy_name,
            PropertyDescriptor::data(proxy.erase()),
        );
        let revoke_name = known.strings.revoke.get_handle(heap, &scope);
        let _ = Object::define_own_property(
            heap,
            &scope,
            obj,
            revoke_name,
            PropertyDescriptor::data(revoke.erase()),
        );
        obj.as_tagged(heap).erase()
    })
}

/// Hidden `__revokeProxy(p)`: nulls the proxy's target/handler slots
/// (idempotent — a null handler already means revoked). Called only by
/// the REVOKE_PRELUDE closure, which guards it with a done-flag.
pub fn proxy_revoke<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, .. } = ctx;
    let proxy = args.get_handle(heap, 1);
    Proxy::revoke(heap, proxy);
    vm.known().undefined.get(heap).erase()
}
