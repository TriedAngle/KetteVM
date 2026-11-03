//! ES 20.4: the Symbol constructor (minimal surface).

use vm_core::HostCtx;
use vm_core::raise_runtime;
use vm_core::{Args, DenseString, Handle, Object, Symbol, Tagged, Value};

/// `Symbol(desc)`: a fresh Symbol primitive (ES 20.4.1.1). The description
/// is ToString'd and stored raw as `[[Description]]`; the `"Symbol(…)"`
/// wording is applied by `SymbolDescriptiveString` (see the `String`
/// constructor). This minimal surface exists so user code can author
/// iterables (`obj[Symbol.iterator] = ...`); `Symbol.iterator` is the
/// well-known one.
pub fn symbol_constructor<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    state.handle_scope(|scope| {
        // absent/undefined description is the empty string
        let desc = if args.len() > 1 && args.get(heap, 1) != vm.known().undefined.get(heap) {
            let h = args.get_handle(heap, 1);
            match Object::to_string(vm, heap, state, h) {
                Ok(Some(s)) => {
                    let s = scope.handle(s);
                    s.as_tagged(heap)
                        .get_as::<DenseString>(heap)
                        .map(|d| d.to_rust_string(heap))
                        .unwrap_or_default()
                }
                Ok(None) => return vm.known().exception.get(heap).erase(),
                Err(err) => return raise_runtime(vm, heap, state, err),
            }
        } else {
            String::new()
        };
        let sym = Symbol::new(heap, &scope, desc.as_bytes());
        sym.as_tagged(heap).erase()
    })
}
