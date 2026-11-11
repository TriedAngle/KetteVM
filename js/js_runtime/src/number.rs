//! ES 21.1: the Number constructor and prototype methods.

use vm_core::{
    Args, CallInfo, Convert, DenseString, HostCtx, Object, Smi, Tagged, Value, VmError,
    WrapperKind, extend_lifetime, raise, rt_try,
};

crate::register_natives! {
    number -> number_constructor,
    number_is_finite,
    number_is_integer,
    number_is_nan,
    number_is_safe_integer,
    number_value_of,
    number_to_string,
    number_to_exponential,
    number_to_fixed,
    number_to_locale_string,
    number_to_precision,
}

/// `Number ( value )`
/// [ES 21.1.1.1](https://tc39.es/ecma262/#sec-number-constructor-number-value)
/// Converts `value` with ToNumeric; under `new` it wraps that Number.
fn number_constructor<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = rt_try!(
        heap,
        state.handle_scope(|scope| {
            let arg = if !args.is_empty() {
                scope.handle(args.get(heap, 0))
            } else {
                scope.handle(Smi::new(0).into_tagged())
            };
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    let Some(new_target) = call.new_target else {
        return heap.new_number(n);
    };
    state.handle_scope(|scope| {
        let value = scope.handle(heap.new_number(n));
        let obj = scope.handle(rt_try!(
            heap,
            Object::create_wrapper_from_constructor(
                vm,
                heap,
                state,
                new_target,
                WrapperKind::Number,
                value,
            )
        ));
        obj.as_tagged(heap).erase()
    })
}

/// `Number.isFinite ( number )`
/// [ES 21.1.2.2](https://tc39.es/ecma262/#sec-number.isfinite)
/// True only for a Number that is finite; never coerces.
fn number_is_finite<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let finite = args
        .get(ctx.heap, 0)
        .as_number(ctx.heap)
        .is_some_and(f64::is_finite);
    Convert::boolean(ctx.heap, finite)
}

/// `Number.isInteger ( number )`
/// [ES 21.1.2.3](https://tc39.es/ecma262/#sec-number.isinteger)
/// True only for an integral Number; never coerces.
fn number_is_integer<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let integer = args
        .get(ctx.heap, 0)
        .as_number(ctx.heap)
        .is_some_and(|n| n.is_finite() && n.trunc() == n);
    Convert::boolean(ctx.heap, integer)
}

/// `Number.isNaN ( number )`
/// [ES 21.1.2.4](https://tc39.es/ecma262/#sec-number.isnan)
/// True only for the NaN Number value; never coerces.
fn number_is_nan<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let nan = args
        .get(ctx.heap, 0)
        .as_number(ctx.heap)
        .is_some_and(f64::is_nan);
    Convert::boolean(ctx.heap, nan)
}

/// `Number.isSafeInteger ( number )`
/// [ES 21.1.2.5](https://tc39.es/ecma262/#sec-number.issafeinteger)
/// True for an integer in the inclusive range ±(2^53 - 1); never coerces.
fn number_is_safe_integer<'a>(
    ctx: HostCtx<'a>,
    _call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let safe = args
        .get(ctx.heap, 0)
        .as_number(ctx.heap)
        .is_some_and(|n| n.is_finite() && n.trunc() == n && n.abs() <= MAX_SAFE_INTEGER);
    Convert::boolean(ctx.heap, safe)
}

/// `Number.prototype.valueOf ( )`
/// [ES 21.1.3.7](https://tc39.es/ecma262/#sec-number.prototype.valueof)
/// Returns `ThisNumberValue(this)`.
fn number_value_of<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match call
        .receiver
        .as_tagged(heap)
        .wrapper_value(heap, vm.known(), WrapperKind::Number)
    {
        Some(v) => extend_lifetime!(heap, v),
        None => raise!(vm, heap, state, VmError::IncompatibleNumberReceiver),
    }
}

/// `Number.prototype.toString ( [ radix ] )`
/// [ES 21.1.3.6](https://tc39.es/ecma262/#sec-number.prototype.tostring)
/// `ThisNumberValue(this)` in the requested radix (default 10).
fn number_to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        // Root the receiver across the radix coercion (it can run user code).
        let Some(x) = call
            .receiver
            .as_tagged(heap)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
            .and_then(|v| v.to_number(heap))
        else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let radix = if !args.is_empty() {
            let arg = args.get_handle(heap, 0);
            if arg.as_tagged(heap) == vm.known().undefined.get(heap).erase() {
                10
            } else {
                let number = rt_try!(heap, Object::to_numeric(vm, heap, state, arg));
                if !number.is_finite() {
                    raise!(vm, heap, state, VmError::IndexOutOfBounds);
                }
                let number = number.trunc();
                if !(2.0..=36.0).contains(&number) {
                    raise!(vm, heap, state, VmError::IndexOutOfBounds);
                }
                number as u32
            }
        } else {
            10
        };
        let text = if radix == 10 {
            Convert::number_to_string(x)
        } else {
            radix_string(x, radix)
        };
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}

/// `Number.prototype.toExponential ( fractionDigits )`
/// [ES 21.1.3.2](https://tc39.es/ecma262/#sec-number.prototype.toexponential)
/// Exponential notation with `fractionDigits` digits after the point, or
/// the shortest round-tripping significand when omitted.
fn number_to_exponential<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(x) = call
            .receiver
            .as_tagged(heap)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
            .and_then(|v| v.to_number(heap))
        else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let arg = args.get_handle(heap, 0);
        let present = arg.as_tagged(heap) != vm.known().undefined.get(heap).erase();
        let fraction = if present {
            let number = rt_try!(heap, Object::to_numeric(vm, heap, state, arg));
            let number = if number.is_nan() { 0.0 } else { number.trunc() };
            if !x.is_finite() {
                let v = scope.handle(heap.new_number(x));
                let s = scope.handle(rt_try!(
                    heap,
                    Convert::to_string(vm, heap, state, &scope, v)
                ));
                return s.as_tagged(heap).erase();
            }
            if !(0.0..=100.0).contains(&number) {
                raise!(vm, heap, state, VmError::IndexOutOfBounds);
            }
            Some(number as usize)
        } else {
            if !x.is_finite() {
                let v = scope.handle(heap.new_number(x));
                let s = scope.handle(rt_try!(
                    heap,
                    Convert::to_string(vm, heap, state, &scope, v)
                ));
                return s.as_tagged(heap).erase();
            }
            None
        };
        let text = exponential_string(x, fraction);
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}

/// `Number.prototype.toLocaleString ( [ reserved1 [ , reserved2 ] ] )`
/// [ES 21.1.3.4](https://tc39.es/ecma262/#sec-number.prototype.tolocalestring)
/// Implementation-defined; without ECMA-402 this returns the decimal form.
fn number_to_locale_string<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    number_to_string(ctx, call, args)
}

/// `Number.prototype.toFixed ( fractionDigits )`
/// [ES 21.1.3.3](https://tc39.es/ecma262/#sec-number.prototype.tofixed)
/// Fixed-point notation with `fractionDigits` digits after the point.
fn number_to_fixed<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(x) = call
            .receiver
            .as_tagged(heap)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
            .and_then(|v| v.to_number(heap))
        else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let digits = if !args.is_empty() {
            let arg = args.get_handle(heap, 0);
            if arg.as_tagged(heap) == vm.known().undefined.get(heap).erase() {
                0
            } else {
                let number = rt_try!(heap, Object::to_numeric(vm, heap, state, arg));
                let number = if number.is_nan() { 0.0 } else { number.trunc() };
                if !(0.0..=100.0).contains(&number) {
                    raise!(vm, heap, state, VmError::IndexOutOfBounds);
                }
                number as usize
            }
        } else {
            0
        };
        let text = if !x.is_finite() || x >= 1e21 {
            Convert::number_to_string(x)
        } else {
            fixed_string(x, digits)
        };
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}

/// `Number.prototype.toPrecision ( precision )`
/// [ES 21.1.3.5](https://tc39.es/ecma262/#sec-number.prototype.toprecision)
/// `precision` significant digits, fixed or exponential by magnitude.
fn number_to_precision<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(x) = call
            .receiver
            .as_tagged(heap)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
            .and_then(|v| v.to_number(heap))
        else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let arg = args.get_handle(heap, 0);
        if arg.as_tagged(heap) == vm.known().undefined.get(heap).erase() {
            let v = scope.handle(heap.new_number(x));
            let s = scope.handle(rt_try!(
                heap,
                Convert::to_string(vm, heap, state, &scope, v)
            ));
            return s.as_tagged(heap).erase();
        }
        let precision = rt_try!(heap, Object::to_numeric(vm, heap, state, arg));
        let precision = if precision.is_nan() {
            0.0
        } else {
            precision.trunc()
        };
        if !x.is_finite() {
            let v = scope.handle(heap.new_number(x));
            let s = scope.handle(rt_try!(
                heap,
                Convert::to_string(vm, heap, state, &scope, v)
            ));
            return s.as_tagged(heap).erase();
        }
        if !(1.0..=100.0).contains(&precision) {
            raise!(vm, heap, state, VmError::IndexOutOfBounds);
        }
        let p = precision as usize;
        let text = precision_string(x, p);
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}

/// Exponential decimal string for a finite Number (ES 21.1.3.2 steps 9-16).
fn exponential_string(x: f64, fraction: Option<usize>) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let value = x.abs();
    if value == 0.0 {
        let significand = match fraction {
            Some(0) | None => "0".to_string(),
            Some(count) => format!("0.{}", "0".repeat(count)),
        };
        return format!("{sign}{significand}e+0");
    }
    let (mantissa, exponent) = match fraction {
        Some(count) => {
            let (digits, exponent) = exact_decimal(value);
            let (digits, delta) = round_digits(&digits, count + 1);
            (exponential_digits(&digits), exponent + delta)
        }
        None => {
            let raw = format!("{value:e}");
            let (mantissa, exponent) = raw.split_once('e').expect("exponential formatting");
            let exponent: i64 = exponent.parse().expect("decimal exponent");
            (mantissa.to_string(), exponent)
        }
    };
    format!("{sign}{mantissa}e{exponent:+}")
}

/// Fixed-point string for a finite, non-negative-magnitude Number.
fn fixed_string(x: f64, fraction: usize) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let value = x.abs();
    if value == 0.0 {
        return if fraction == 0 {
            format!("{sign}0")
        } else {
            format!("{sign}0.{}", "0".repeat(fraction))
        };
    }
    let (digits, exponent) = exact_decimal(value);
    let keep = exponent + 1 + fraction as i64;
    if keep <= 0 {
        if keep == 0 && digits[0] >= 5 {
            return if fraction == 0 {
                format!("{sign}1")
            } else {
                format!("{sign}0.{}1", "0".repeat(fraction - 1))
            };
        }
        return if fraction == 0 {
            format!("{sign}0")
        } else {
            format!("{sign}0.{}", "0".repeat(fraction))
        };
    }
    let (rounded, delta) = round_digits(&digits, keep as usize);
    let exponent = exponent + delta;
    let integer_digits = exponent + 1;
    if integer_digits <= 0 {
        let mut fractional = vec![0; (-integer_digits) as usize];
        fractional.extend_from_slice(&rounded);
        fractional.resize(fraction, 0);
        format!("{sign}0.{}", digits_string(&fractional))
    } else {
        let mut all = rounded;
        all.resize((integer_digits as usize) + fraction, 0);
        let point = integer_digits as usize;
        if fraction == 0 {
            format!("{sign}{}", digits_string(&all))
        } else {
            format!(
                "{sign}{}.{}",
                digits_string(&all[..point]),
                digits_string(&all[point..])
            )
        }
    }
}

/// Significant-digit string for a finite Number (ES 21.1.3.5).
fn precision_string(x: f64, precision: usize) -> String {
    let sign = if x < 0.0 { "-" } else { "" };
    let value = x.abs();
    if value == 0.0 {
        return if precision == 1 {
            format!("{sign}0")
        } else {
            format!("{sign}0.{}", "0".repeat(precision - 1))
        };
    }
    let (digits, exponent) = exact_decimal(value);
    let (digits, delta) = round_digits(&digits, precision);
    let exponent = exponent + delta;
    if exponent < -6 || exponent >= precision as i64 {
        return format!("{sign}{}e{exponent:+}", exponential_digits(&digits));
    }
    if exponent >= 0 {
        let integer_digits = exponent as usize + 1;
        if integer_digits >= precision {
            let mut all = digits;
            all.resize(integer_digits, 0);
            return format!("{sign}{}", digits_string(&all));
        }
        return format!(
            "{sign}{}.{}",
            digits_string(&digits[..integer_digits]),
            digits_string(&digits[integer_digits..])
        );
    }
    format!(
        "{sign}0.{}{}",
        "0".repeat((-exponent - 1) as usize),
        digits_string(&digits)
    )
}

/// Exact decimal significand of a positive finite Number: `digits` and a
/// base-10 exponent such that the value is `d[0].d[1..] * 10^exponent`.
fn exact_decimal(value: f64) -> (Vec<u8>, i64) {
    let exact = format!("{value:.1100e}");
    let (mantissa, exponent) = exact.split_once('e').expect("exponential formatting");
    let digits = mantissa
        .chars()
        .filter(|&c| c != '.')
        .map(|c| c as u8 - b'0')
        .collect();
    (digits, exponent.parse().expect("decimal exponent"))
}

/// Round the significand to `keep` digits, ties away from zero, returning
/// the digit delta applied to the exponent.
fn round_digits(digits: &[u8], keep: usize) -> (Vec<u8>, i64) {
    let mut out: Vec<u8> = digits.iter().take(keep).copied().collect();
    let mut delta = 0;
    if digits.get(keep).is_some_and(|&digit| digit >= 5) {
        let mut index = out.len();
        while index > 0 {
            index -= 1;
            if out[index] == 9 {
                out[index] = 0;
            } else {
                out[index] += 1;
                break;
            }
        }
        if out.iter().all(|&digit| digit == 0) {
            out = vec![1];
            delta = 1;
        }
    }
    out.resize(keep, 0);
    (out, delta)
}

fn exponential_digits(digits: &[u8]) -> String {
    debug_assert!(!digits.is_empty());
    let mut out = String::with_capacity(digits.len() + 1);
    out.push((b'0' + digits[0]) as char);
    if digits.len() > 1 {
        out.push('.');
        for &digit in &digits[1..] {
            out.push((b'0' + digit) as char);
        }
    }
    out
}

fn digits_string(digits: &[u8]) -> String {
    digits.iter().map(|&d| (b'0' + d) as char).collect()
}

/// `Number::toString ( x, radix )`
/// [ES 6.1.6.1.20](https://tc39.es/ecma262/#sec-numeric-types-number-tostring)
/// Shortest positional representation of `x` in `radix`.
fn radix_string(x: f64, radix: u32) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    if x < 0.0 {
        return format!("-{}", radix_string(-x, radix));
    }
    if x.is_infinite() {
        return "Infinity".to_string();
    }
    let integer = x.trunc();
    let fraction = x - integer;
    let mut text = if integer == 0.0 {
        "0".to_string()
    } else if integer <= u128::MAX as f64 {
        let mut n = integer as u128;
        let mut digits = Vec::new();
        while n > 0 {
            digits.push(DIGITS[(n % radix as u128) as usize]);
            n /= radix as u128;
        }
        digits.reverse();
        String::from_utf8(digits).expect("radix digits are ASCII")
    } else {
        // TODO: arbitrary-precision integer digits for magnitudes beyond u128.
        let mut n = integer;
        let mut digits = Vec::new();
        while n >= 1.0 {
            digits.push(DIGITS[(n % radix as f64) as usize]);
            n = (n / radix as f64).trunc();
        }
        digits.reverse();
        String::from_utf8(digits).expect("radix digits are ASCII")
    };
    if fraction != 0.0 {
        text.push('.');
        let mut value = integer;
        let mut scale = 1.0;
        let mut f = fraction;
        for _ in 0..1100 {
            f *= radix as f64;
            let digit = f.floor();
            f -= digit;
            text.push(DIGITS[digit as usize] as char);
            value = value * radix as f64 + digit;
            scale *= radix as f64;
            if f == 0.0 || value / scale == x {
                break;
            }
        }
    }
    text
}
