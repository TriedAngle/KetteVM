//! ES 19: function properties of the global object (eval, isNaN).
use crate::JSRuntime;
use vm_core::{
    Args, ContextInit, ContextObject, Convert, DenseString, Handle, HandleScope, HandleSlice, Heap,
    HostCtx, Object, Result, Runtime, SCOPE_DECLARATION, SCOPE_SCRIPT, ScopeInfo, ScopeInfoInit,
    Smi, Tagged, ThreadState, VM, Value, VmError, WellKnown, materialize::Materialize, raise,
    raise_runtime, rt_try,
};

crate::register_natives! {
    eval -> eval_runtime,
    is_nan,
    is_finite,
    parse_int,
    parse_float,
    print,
    performance_now,
    console_log,
}

fn eval_runtime<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let ids = &ctx.runtime::<JSRuntime>().ids;
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
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
        let program = match js_compiler::compile_js(&text, bytecode::SourceMode::EvalGlobal, ids) {
            Ok(program) => program,
            Err(_) => {
                raise!(vm, heap, state, VmError::InvalidDeclaration);
            }
        };
        let context = eval_global_context(vm, heap, &scope);
        let closure =
            Materialize::closure_vm(vm, heap, state, &scope, &program, JSRuntime::ID, context);
        match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            closure.erase(),
            HandleSlice::EMPTY,
            None,
        ) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(heap, err),
        }
        .as_tagged(heap)
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
    let outer = vm.known().empty_context.get_handle(heap, scope);
    let slots = vm.known().empty_fixed_array.get_handle(heap, scope);
    heap.allocate_handle::<ContextObject>(
        ContextInit {
            outer: Some(outer),
            slots,
            scope_info: info,
        },
        scope,
    )
}

/// `isNaN ( number )`
/// [ES 19.2.3](https://tc39.es/ecma262/#sec-isnan-number)
/// Number(x) is NaN.
fn is_nan<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = rt_try!(
        heap,
        state.handle_scope(|scope| {
            let arg = scope.handle(args.get(heap, 1));
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    Convert::boolean(heap, n.is_nan())
}

/// `isFinite ( number )`
/// [ES 19.2.2](https://tc39.es/ecma262/#sec-isfinite-number)
/// Number(x) is finite.
fn is_finite<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = rt_try!(
        heap,
        state.handle_scope(|scope| {
            let arg = scope.handle(args.get(heap, 1));
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    Convert::boolean(heap, n.is_finite())
}

/// `parseFloat ( string )`
/// [ES 19.2.4](https://tc39.es/ecma262/#sec-parsefloat-string)
/// Longest leading `StrDecimalLiteral` of `string`; no radix or hex form.
fn parse_float<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let input = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, input)));
        let dense = s
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .expect("ToString returns a string");
        let text = dense.data(heap);
        let start = text.trim_start_index();
        // Longest leading StrDecimalLiteral; the rest is ignored.
        let Some(end) = text.decimal_prefix_end(start) else {
            return heap.new_number(f64::NAN);
        };
        let bytes: Vec<u8> = (start..end).map(|i| text.code_unit(i) as u8).collect();
        let value = core::str::from_utf8(&bytes)
            .ok()
            .and_then(|prefix| prefix.parse::<f64>().ok())
            .unwrap_or(f64::NAN);
        heap.new_number(value)
    })
}

/// `parseInt ( string, radix )`
/// [ES 19.2.5](https://tc39.es/ecma262/#sec-parseint-string-radix)
/// Longest integer prefix of `string` in `radix`, honoring a `0x` prefix
/// when `radix` is 0 or 16.
fn parse_int<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let input = args.get_handle(heap, 1);
    let radix_arg = args.get_handle(heap, 2);
    state.handle_scope(|scope| {
        // ToString first: step order is observable through `valueOf`.
        let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, input)));
        // The radix coercion runs user code, so the string's payload is only
        // borrowed afterwards (trimming and the sign scan are pure).
        let radix = Convert::number_to_int32(rt_try!(
            heap,
            Object::to_numeric(vm, heap, state, radix_arg)
        ));
        let dense = s
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .expect("ToString returns a string");
        let text = dense.data(heap);
        let mut lo = text.trim_start_index();
        let mut negative = false;
        if lo < text.len() {
            let unit = text.code_unit(lo);
            if unit == b'-' as u16 {
                negative = true;
                lo += 1;
            } else if unit == b'+' as u16 {
                lo += 1;
            }
        }
        let (mut radix, strip_prefix) = if radix == 0 {
            (10, true)
        } else if (2..=36).contains(&radix) {
            (radix, radix == 16)
        } else {
            return heap.new_number(f64::NAN);
        };
        if strip_prefix
            && lo + 1 < text.len()
            && text.code_unit(lo) == 0x30
            && matches!(text.code_unit(lo + 1), 0x58 | 0x78)
        {
            lo += 2;
            radix = 16;
        }
        let digits_start = lo;
        let mut value = 0.0f64;
        while lo < text.len() {
            let digit = match text.code_unit(lo) {
                unit @ 0x30..=0x39 => (unit - 0x30) as i32,
                unit @ 0x41..=0x5a => (unit - 0x41) as i32 + 10,
                unit @ 0x61..=0x7a => (unit - 0x61) as i32 + 10,
                _ => break,
            };
            if digit >= radix {
                break;
            }
            value = value * radix as f64 + digit as f64;
            lo += 1;
        }
        if lo == digits_start {
            return heap.new_number(f64::NAN);
        }
        heap.new_number(if negative { -value } else { value })
    })
}

/// `print(x)`: ToString(x) to stdout followed by a newline (a shell
/// convenience, not an ES builtin; the Octane runner reports through it).
fn print<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    if args.len() < 2 {
        println!();
        return vm.known().undefined.get(heap).erase();
    }
    let arg = args.get_handle(heap, 1);
    state.handle_scope(|scope| {
        let s = scope.handle(rt_try!(heap, Object::to_string(vm, heap, state, arg)));
        let text = s
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_default();
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
#[cold]
#[inline(never)]
fn console_arg_cold(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    arg: Handle<'_, Value>,
) -> Result<String> {
    state.handle_scope(|scope| {
        let s = scope.handle(Object::to_string(vm, heap, state, arg)?);
        let text = s
            .as_tagged(heap)
            .get_as::<DenseString>(heap)
            .map(|s| s.to_rust_string(heap))
            .unwrap_or_default();
        Ok(text)
    })
}

/// `console.log(...args)`: ToString every argument, join with single
/// spaces, and write the result to stdout followed by a newline (Node's
/// single-line formatting; a shell convenience, not an ES builtin).
fn console_log<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let mut parts: Vec<String> = Vec::with_capacity(args.len().saturating_sub(1));
    for i in 1..args.len() {
        // Safety: `i < args.len()`, so the slot exists.
        let arg = args.get_handle(heap, i);
        if let Some(text) = primitive_to_string(heap, vm.known(), arg.as_tagged(heap)) {
            parts.push(text);
            continue;
        }
        match console_arg_cold(vm, heap, state, arg) {
            Ok(text) => parts.push(text),
            Err(err) => return raise_runtime(heap, err),
        }
    }
    println!("{}", parts.join(" "));
    vm.known().undefined.get(heap).erase()
}

/// `performance.now()`: fractional milliseconds since the Unix epoch
/// (shell timing convenience mirroring the browser API).
fn performance_now<'a>(
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
