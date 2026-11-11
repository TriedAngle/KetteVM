//! ES 22.1 (Math): function properties of the Math namespace object.

use std::{sync::atomic::AtomicU64, sync::atomic::Ordering};
use vm_core::{
    Args, CallInfo, Heap, HostCtx, Object, Result, Tagged, ThreadState, VM, Value, rt_try,
};

crate::register_natives! {
    math_log,
    math_pow,
    math_sqrt,
    math_abs,
    math_floor,
    math_ceil,
    math_trunc,
    math_round,
    math_min,
    math_max,
    math_random,
}

/// TODO: better distribution
/// xorshift64
static RANDOM_STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn random_bits() -> u64 {
    let mut x = RANDOM_STATE.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RANDOM_STATE.store(x, Ordering::Relaxed);
    x
}

/// The inline half of an argument read: a value that already is a number
/// (a Smi, or a boxed `Float`) needs no ToPrimitive/ToNumber round-trip.
#[inline]
fn number_arg(heap: &Heap, args: Args, i: usize) -> Option<f64> {
    args.get(heap, i).as_number(heap)
}

/// The cold half of an argument read: full ToNumeric for strings, objects,
/// `undefined`, etc.
#[cold]
#[inline(never)]
fn numeric_arg_cold(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    args: Args,
    i: usize,
) -> Result<f64> {
    state.handle_scope(|scope| {
        let arg = scope.handle(args.get(heap, i));
        Object::to_numeric(vm, heap, state, arg)
    })
}

/// An argument as an f64: the inlinable number fast path, falling back to
/// the cold ToNumeric body.
#[inline]
fn arg_as_f64(vm: &VM, heap: &mut Heap, state: &ThreadState, args: Args, i: usize) -> Result<f64> {
    if let Some(x) = number_arg(heap, args, i) {
        return Ok(x);
    }
    numeric_arg_cold(vm, heap, state, args, i)
}

/// Shared body of the unary Math functions: coerce the sole argument and
/// apply `f` (`Math.abs`/`sqrt`/`log`).
#[inline]
fn math_unary<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    args: Args,
    f: impl Fn(f64) -> f64,
) -> Tagged<'a, Value> {
    let x = rt_try!(heap, arg_as_f64(vm, heap, state, args, 0));
    heap.new_number(f(x))
}

/// Shared body of the integer-preserving unaries (`floor`/`ceil`/`trunc`/
/// `round`): a Smi is already integral, so it comes straight back out with
/// no f64 round-trip and no heap traffic.
#[inline]
fn math_integral<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    args: Args,
    f: impl Fn(f64) -> f64,
) -> Tagged<'a, Value> {
    if let Some(bits) = args.get(heap, 0).smi_bits() {
        return Tagged::from_smi_bits(bits).erase();
    }
    math_unary(vm, heap, state, args, f)
}

/// `Math.sqrt(x)` (ES 22.1.2.29): ToNumber, then the IEEE-754 square root
/// (NaN/negative input → NaN, ±0 → ±0).
fn math_sqrt<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_unary(vm, heap, state, args, f64::sqrt)
}

/// `Math.log(x)` (ES 22.1.2.15): natural logarithm.
fn math_log<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_unary(vm, heap, state, args, f64::ln)
}

/// `Math.pow(base, exponent)` (ES 22.1.2.20).
fn math_pow<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let base = rt_try!(heap, arg_as_f64(vm, heap, state, args, 0));
    let exp = rt_try!(heap, arg_as_f64(vm, heap, state, args, 1));
    heap.new_number(base.powf(exp))
}

/// `Math.round(x)` (ES 22.1.2.24): ties round toward +∞ (`-0.5` → `-0`),
/// so `f64::round`'s half-away-from-zero is not usable directly.
fn js_round(x: f64) -> f64 {
    if x.is_nan() || x.is_infinite() || x == 0.0 {
        return x;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    (x + 0.5).floor()
}

/// `Math.abs(x)` (ES 22.1.2.1).
fn math_abs<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_unary(vm, heap, state, args, f64::abs)
}

/// `Math.floor(x)` (ES 22.1.2.11).
fn math_floor<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_integral(vm, heap, state, args, f64::floor)
}

/// `Math.ceil(x)` (ES 22.1.2.2).
fn math_ceil<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_integral(vm, heap, state, args, f64::ceil)
}

/// `Math.trunc(x)` (ES 22.1.2.31).
fn math_trunc<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_integral(vm, heap, state, args, f64::trunc)
}

/// `Math.round(x)` (ES 22.1.2.24).
fn math_round<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    math_integral(vm, heap, state, args, js_round)
}

/// Shared body of `Math.min`/`Math.max`: ToNumber every argument; any NaN
/// argument (or no argument at all) decides the result per spec.
#[inline]
fn math_minmax<'a>(ctx: HostCtx<'a>, args: Args, min: bool) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let mut result = if min {
        f64::INFINITY
    } else {
        f64::NEG_INFINITY
    };
    let mut i = 0;
    while i < args.len() {
        let x = rt_try!(heap, arg_as_f64(vm, heap, state, args, i));
        if x.is_nan() {
            result = f64::NAN;
        } else if min {
            if x < result || (x == 0.0 && result == 0.0 && x.is_sign_negative()) {
                result = x;
            }
        } else if x > result || (x == 0.0 && result == 0.0 && x.is_sign_positive()) {
            result = x;
        }
        i += 1;
    }
    heap.new_number(result)
}

/// `Math.min(...args)` (ES 22.1.2.14).
fn math_min<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    math_minmax(ctx, args, true)
}

/// `Math.max(...args)` (ES 22.1.2.13).
fn math_max<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    math_minmax(ctx, args, false)
}

/// `Math.random()` (ES 22.1.2.22): a number in `[0, 1)` built from the top
/// 53 random bits.
fn math_random<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let bits = random_bits() >> 11;
    ctx.heap
        .new_number((bits as f64) * (1.0 / 9_007_199_254_740_992.0))
}
