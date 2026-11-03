//! ES 19: function properties of the global object (eval, isNaN).

use vm_core::HostCtx;
use vm_core::Object;
use vm_core::materialize::Materialize;
use vm_core::{
    Args, ContextInit, ContextObject, Convert, DenseString, Handle, HandleScope, HandleSlice, Heap,
    SCOPE_DECLARATION, SCOPE_SCRIPT, ScopeInfo, ScopeInfoInit, Smi, Tagged, ThreadState, VM, Value,
    VmError, WellKnown,
};
use vm_core::{raise_runtime, rt_try};

pub fn eval_runtime<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let source = args.get_handle(heap, 1);
    if source.as_tagged(heap).get_as::<DenseString>(heap).is_none() {
        return source.as_tagged(heap).erase();
    }
    state.handle_scope(|scope| {
        let text = source
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .expect("checked string source");
        let program = match js_compiler::compile_js(&text, bytecode::SourceMode::EvalGlobal) {
            Ok(program) => program,
            Err(_) => return raise_runtime(vm, heap, state, VmError::Syntax),
        };
        let context = eval_global_context(vm, heap, &scope);
        let closure = rt_try!(
            vm,
            heap,
            state,
            Materialize::closure_vm(vm, heap, state, &scope, &program, context)
        );
        match HostCtx::enter(vm, heap, state, closure.erase(), HandleSlice::EMPTY, None)
            .map(|v| v.raw())
        {
            Ok(v) => unsafe { Tagged::<Value>::from_value_unchecked(v) },
            Err(err) => raise_runtime(vm, heap, state, err),
        }
    })
}

fn eval_global_context<'s>(
    vm: &VM,
    heap: &mut Heap,
    scope: &'s HandleScope<'_>,
) -> Handle<'s, ContextObject> {
    let info = heap.allocate_handle::<ScopeInfo>(
        ScopeInfoInit {
            names: HandleSlice::EMPTY,
            infos: &[],
            flags: SCOPE_DECLARATION | SCOPE_SCRIPT,
        },
        scope,
    );
    let outer = scope.handle(vm.known().empty_context.get(heap));
    let slots = scope.handle(vm.known().empty_fixed_array.get(heap));
    heap.allocate_handle::<ContextObject>(
        ContextInit {
            outer: Some(outer),
            slots,
            scope_info: info,
        },
        scope,
    )
}

/// `isNaN(x)`: ToNumber(x) is NaN.
pub fn is_nan<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let n = rt_try!(
        vm,
        heap,
        state,
        state.handle_scope(|scope| {
            let arg = scope.handle(args.get(heap, 1));
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    let Some(n) = n else {
        return vm.known().exception.get(heap).erase();
    };

    Convert::boolean(heap, n.is_nan())
}

/// `print(x)`: ToString(x) to stdout followed by a newline (a shell
/// convenience, not an ES builtin; the Octane runner reports through it).
pub fn print<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    if args.len() < 2 {
        println!();
        return vm.known().undefined.get(heap).erase();
    }
    let arg = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let text = match Object::to_string(vm, heap, state, arg) {
            Ok(Some(s)) => {
                let s = scope.handle(s);
                s.as_tagged(heap)
                    .get_as::<DenseString>(heap)
                    .map(|s| s.to_rust_string(heap))
                    .unwrap_or_default()
            }
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        println!("{text}");
        vm.known().undefined.get(heap).erase()
    })
}

/// The inline half of a `console.log` argument read: Smis, strings, and the
/// singleton primitives stringify with no coercion and no allocation.
#[inline]
fn primitive_to_string(heap: &Heap, roots: &WellKnown, v: Tagged<'_, Value>) -> Option<String> {
    let known = roots;
    if let Some(smi) = Smi::decode(v.raw()) {
        return Some(smi.value().to_string());
    }
    if v == known.undefined.get(heap) {
        return Some("undefined".to_string());
    }
    if v == known.null.get(heap) {
        return Some("null".to_string());
    }
    if v == known.true_object.get(heap) {
        return Some("true".to_string());
    }
    if v == known.false_object.get(heap) {
        return Some("false".to_string());
    }
    v.get_as::<DenseString>(heap)
        .map(|s| s.to_rust_string(heap))
}

/// The cold half: full ToString for boxed numbers, objects, and symbols.
/// `Ok(None)` means user code threw (the pending exception holds the cause).
#[cold]
#[inline(never)]
fn console_arg_cold(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    arg: Handle<'_, Value>,
) -> Result<Option<String>, VmError> {
    state.handle_scope(|scope| {
        let Some(s) = Object::to_string(vm, heap, state, arg)? else {
            return Ok(None);
        };
        let s = scope.handle(s);
        let text = s
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_default();
        Ok(Some(text))
    })
}

/// `console.log(...args)`: ToString every argument, join with single
/// spaces, and write the result to stdout followed by a newline (Node's
/// single-line formatting; a shell convenience, not an ES builtin).
pub fn console_log<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx { vm, heap, state } = ctx;
    let mut parts: Vec<String> = Vec::with_capacity(args.len().saturating_sub(1));
    for i in 1..args.len() {
        // Safety: `i < args.len()`, so the slot exists.
        let arg = args.get_handle(heap, i);
        if let Some(text) = primitive_to_string(heap, vm.known(), arg.as_tagged(heap)) {
            parts.push(text);
            continue;
        }
        match console_arg_cold(vm, heap, state, arg) {
            Ok(Some(text)) => parts.push(text),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
    }
    println!("{}", parts.join(" "));
    vm.known().undefined.get(heap).erase()
}

/// `performance.now()`: fractional milliseconds since the Unix epoch
/// (shell timing convenience mirroring the browser API).
pub fn performance_now<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    _args: Args,
) -> Tagged<'a, Value> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    ctx.heap.new_number(ms)
}
