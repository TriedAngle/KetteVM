//! ES 28.2: the Proxy constructor, Proxy.revocable, and the
//! revoke-closure prelude.

use vm_core::Coercion;
use vm_core::HostCtx;
use vm_core::Object;
use vm_core::PropertyDescriptor;
use vm_core::error::Message;
use vm_core::proxy::Proxy;
use vm_core::raise_runtime;
use vm_core::rt_try;
use vm_core::{Args, Handle, HandleSlice, Tagged, Value, VmError};

/// `new Proxy(target, handler)` (ES 20.2.1.1): both must be JSReceivers;
/// the map's capability bits mirror the target's so callability is
/// observable (`typeof`, future `Call`/`Construct` dispatch).
pub fn proxy_constructor<'a>(
    nctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let construct = new_target.is_some();
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
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
    state.handle_scope(|scope| {
        Proxy::allocate(
            heap,
            &scope,
            // Safety: rooted argument words, consumed by the allocation.
            unsafe { Tagged::<Value>::from_value_unchecked(target.raw()) },
            unsafe { Tagged::<Value>::from_value_unchecked(handler.raw()) },
        )
    })
}

/// `Proxy.revocable(target, handler)` (ES 20.2.2.1): returns
/// `{ proxy, revoke }`; the revoke closure is the JS template installed
/// by REVOKE_PRELUDE (it keeps the idempotence flag and calls the
/// hidden `__revokeProxy` runtime).
pub fn proxy_revocable<'a>(
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = nctx;
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
        let proxy = scope.handle(Proxy::allocate(
            heap,
            &scope,
            // Safety: rooted argument words, consumed by the allocation.
            unsafe { Tagged::<Value>::from_value_unchecked(target.raw()) },
            unsafe { Tagged::<Value>::from_value_unchecked(handler.raw()) },
        ));
        // Function.prototype.__makeRevoke (installed by REVOKE_PRELUDE)
        // Safety: fresh interned word, consumed by the lookup.
        let name = vm
            .interner()
            .intern_str(heap, &scope, "__makeRevoke")
            .erase();
        let proto = scope.handle(vm.known().function_prototype.get(heap).erase());
        let make_revoke = match rt_try!(
            vm,
            heap,
            state,
            Object::get_property(vm, heap, state, proto, proto, name, false)
        ) {
            Coercion::Threw => {
                return vm.known().exception.get(heap).erase();
            }
            Coercion::Value(v) => scope.handle(v),
        };
        // Safety: fresh rooted-slot words staged for the call.
        let exception = vm.known().exception.get(heap).raw();
        let staged = scope.stage(&[
            vm.known().undefined.get(heap).erase(),
            proxy.as_tagged(heap).erase(),
        ]);
        let r = rt_try!(
            vm,
            heap,
            state,
            HostCtx::enter(vm, &mut *heap, state, make_revoke, staged, None)
        );
        if r.raw() == exception {
            return vm.known().exception.get(heap).erase();
        }
        // Safety: fresh call result, rooted below.
        let revoke = scope.handle(r);
        let map = scope.handle(vm.known().object_initial_map.get(heap));
        let obj = heap
            .new_object(&scope, map, HandleSlice::EMPTY)
            .as_handle(&scope);
        let proxy_name = scope.handle(vm.known().strings.proxy.get(heap));
        let _ = Object::define_own_property(
            heap,
            &scope,
            obj,
            proxy_name,
            PropertyDescriptor::data(proxy.erase()),
        );
        let revoke_name = scope.handle(vm.known().strings.revoke.get(heap));
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
    nctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state: _, ..
    } = nctx;
    let proxy = args.get(heap, 1);
    let proxy = proxy.raw();
    Proxy::revoke(
        heap,
        // Safety: fresh argument word, consumed with no allocation delay.
        unsafe { Tagged::<Value>::from_value_unchecked(proxy) },
    );
    vm.known().undefined.get(heap).erase()
}

/// The revoke-closure template: `done` plays [[RevocableProxy]]'s
/// cleared-slot role (idempotent revoke), the captured `p` keeps the
/// proxy reachable.
pub const REVOKE_PRELUDE: &str = r#"
Function.prototype.__makeRevoke = function (p) {
  var done = false;
  return function revoke() {
    if (!done) {
      done = true;
      __revokeProxy(p);
    }
  };
};
"#;
