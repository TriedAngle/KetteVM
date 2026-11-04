//! ES 21.1: the Number constructor and prototype methods.

use vm_core::HostCtx;
use vm_core::Object;
use vm_core::{
    Args, Convert, DenseString, Handle, Heap, Smi, Tagged, Value, VmError, WellKnown, WrapperKind,
};
use vm_core::{raise_runtime, rt_try};

/// `Number ( value )`
/// [ES 21.1.1.1](https://tc39.es/ecma262/#sec-number-constructor-number-value)
/// Converts `value` with ToNumeric; under `new` it wraps that Number.
pub fn number_constructor<'a>(
    ctx: HostCtx<'a>,
    new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let n = rt_try!(
        vm,
        heap,
        state,
        state.handle_scope(|scope| {
            let arg = if args.len() > 1 {
                scope.handle(args.get(heap, 1))
            } else {
                scope.handle(Smi::new(0).into_tagged())
            };
            Object::to_numeric(vm, heap, state, arg)
        })
    );
    let Some(n) = n else {
        return vm.known().exception.get(heap).erase();
    };

    let Some(new_target) = new_target else {
        return heap.new_number(n);
    };
    state.handle_scope(|scope| {
        let value = scope.handle(heap.new_number(n));
        let obj = match Object::create_wrapper_from_constructor(
            vm,
            heap,
            state,
            new_target,
            WrapperKind::Number,
            value,
        ) {
            Ok(Some(obj)) => scope.handle(obj),
            Ok(None) => return vm.known().exception.get(heap).erase(),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        obj.as_tagged(heap).erase()
    })
}

pub fn number_value_of<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    // The result crosses this call boundary, so root it: `wrapper_value`
    // borrows the heap immutably, and a handle releases that borrow before
    // the `TypeError` path needs the heap mutably.
    ctx.handle_scope(|vm, heap, state, scope| {
        let v = match args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
        {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        v.as_tagged(heap).erase()
    })
}

pub fn number_to_string<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        // root the receiver: `Convert::to_string` allocates a fresh string
        let v = match args
            .get(heap, 0)
            .wrapper_value(heap, vm.known(), WrapperKind::Number)
        {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        let s = match Convert::to_string(heap, &scope, v) {
            Ok(v) => scope.handle(v),
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        s.as_tagged(heap).erase()
    })
}

/// The numeric `this` of a Number.prototype method (receiver or wrapper).
fn number_receiver(heap: &Heap, roots: &WellKnown, args: Args) -> Result<f64, VmError> {
    let v = args
        .get(heap, 0)
        .wrapper_value(heap, roots, WrapperKind::Number)?;
    v.to_number(heap)
}

/// `Number.prototype.toFixed(fractionDigits?)` (ES 21.1.3.3): fixed-point
/// notation with `fractionDigits` digits after the decimal point.
pub fn number_to_fixed<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let x = rt_try!(vm, heap, state, number_receiver(heap, vm.known(), args));
        let digits = match (args.len() > 1).then(|| args.get(heap, 1)) {
            Some(d) if d != vm.known().undefined.get(heap) => {
                scope.handle(d);
                rt_try!(vm, heap, state, d.to_number(heap)) as i64
            }
            _ => 0,
        };
        if !(0..=100).contains(&digits) {
            return raise_runtime(vm, heap, state, VmError::OutOfBounds);
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
pub fn number_to_precision<'a>(
    ctx: HostCtx<'a>,
    _new_target: Option<Handle<'_, Value>>,
    args: Args,
) -> Tagged<'a, Value> {
    ctx.handle_scope(|vm, heap, state, scope| {
        let x = rt_try!(vm, heap, state, number_receiver(heap, vm.known(), args));
        let arg = args.get(heap, 1);
        if arg == vm.known().undefined.get(heap) {
            let v = scope.handle(heap.new_number(x));
            let s = match Convert::to_string(heap, &scope, v) {
                Ok(v) => scope.handle(v),
                Err(err) => return raise_runtime(vm, heap, state, err),
            };
            return s.as_tagged(heap).erase();
        }
        let p = rt_try!(vm, heap, state, arg.to_number(heap)) as i64;
        if !(1..=100).contains(&p) {
            return raise_runtime(vm, heap, state, VmError::OutOfBounds);
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
