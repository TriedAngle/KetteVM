//! ES 21.1: the Number constructor and prototype methods.

use vm_core::{
    Args, Convert, DenseString, Handle, Heap, HostCtx, Object, Smi, Tagged, Value, VmError,
    WellKnown, WrapperKind, extend_lifetime, raise, rt_try,
};

crate::register_natives! {
    number -> number_constructor,
    number_value_of,
    number_to_string,
    number_to_fixed,
    number_to_precision,
}

/// `Number ( value )`
/// [ES 21.1.1.1](https://tc39.es/ecma262/#sec-number-constructor-number-value)
/// Converts `value` with ToNumeric; under `new` it wraps that Number.
fn number_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = rt_try!(
        heap,
        state.handle_scope(|scope| {
            let arg = if args.len() > 1 {
                scope.handle(args.get(heap, 1))
            } else {
                scope.handle(Smi::new(0).into_tagged())
            };
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    let Some(new_target) = new_target else {
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

fn number_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match args
        .get(heap, 0)
        .wrapper_value(heap, vm.known(), WrapperKind::Number)
    {
        Some(v) => extend_lifetime!(heap, v),
        None => raise!(vm, heap, state, VmError::IncompatibleNumberReceiver),
    }
}

fn number_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        // root the receiver: `Convert::to_string` allocates a fresh string
        let Some(v) = args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
        else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let v = scope.handle(v);
        let s = scope.handle(rt_try!(
            heap,
            Convert::to_string(vm, heap, state, &scope, v)
        ));
        s.as_tagged(heap).erase()
    })
}

/// The numeric `this` of a Number.prototype method (receiver or wrapper).
fn number_receiver(heap: &Heap, roots: &WellKnown, args: Args) -> Option<f64> {
    let v = args
        .get(heap, 0)
        .wrapper_value(heap, roots, WrapperKind::Number)?;
    v.to_number(heap)
}

/// `Number.prototype.toFixed(fractionDigits?)` (ES 21.1.3.3): fixed-point
/// notation with `fractionDigits` digits after the decimal point.
fn number_to_fixed<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(x) = number_receiver(heap, vm.known(), args) else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let digits = match (args.len() > 1).then(|| args.get(heap, 1)) {
            Some(d) if d != vm.known().undefined.get(heap) => {
                scope.handle(d);
                match d.to_number(heap) {
                    Some(number) => number as i64,
                    None => {
                        raise!(vm, heap, state, VmError::CannotConvertToNumber);
                    }
                }
            }
            _ => 0,
        };
        if !(0..=100).contains(&digits) {
            raise!(vm, heap, state, VmError::IndexOutOfBounds);
        }
        let text = if x.is_nan() {
            "NaN".into()
        } else if x == f64::INFINITY {
            "Infinity".into()
        } else if x == f64::NEG_INFINITY {
            "-Infinity".into()
        } else {
            format!("{:.*}", digits as usize, x)
        };
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}

/// `Number.prototype.toPrecision(precision?)` (ES 21.1.3.6): `precision`
/// significant digits, fixed or exponential per the magnitude.
fn number_to_precision<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let Some(x) = number_receiver(heap, vm.known(), args) else {
            raise!(vm, heap, state, VmError::IncompatibleNumberReceiver);
        };
        let arg = args.get(heap, 1);
        if arg == vm.known().undefined.get(heap) {
            let v = scope.handle(heap.new_number(x));
            let s = scope.handle(rt_try!(
                heap,
                Convert::to_string(vm, heap, state, &scope, v)
            ));
            return s.as_tagged(heap).erase();
        }
        let Some(p) = arg.to_number(heap) else {
            raise!(vm, heap, state, VmError::CannotConvertToNumber);
        };
        let p = p as i64;
        if !(1..=100).contains(&p) {
            raise!(vm, heap, state, VmError::IndexOutOfBounds);
        }
        let text = if x.is_nan() {
            "NaN".into()
        } else if x == f64::INFINITY {
            "Infinity".into()
        } else if x == f64::NEG_INFINITY {
            "-Infinity".into()
        } else if x == 0.0 {
            format!("{:.*}", p as usize - 1, 0.0)
        } else {
            let e = x.abs().log10().floor() as i64;
            if e < -6 || e >= p {
                // exponential: Rust writes 1.23e3; JS wants 1.23e+3
                let s = format!("{:.*e}", p as usize - 1, x);
                let (m, exp) = s.split_once('e').expect("exponent formatting");
                let exp: i64 = exp.parse().expect("decimal exponent");
                format!("{m}e{exp:+}")
            } else {
                format!("{:.*}", (p - 1 - e) as usize, x)
            }
        };
        let s = DenseString::from_utf8(heap, &scope, &text);
        s.as_tagged(heap).erase()
    })
}
