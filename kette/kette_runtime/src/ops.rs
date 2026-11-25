use vm_core::{
    Args, CallInfo, Convert, DenseString, HostCtx, Smi, StringRef, Tagged, Value, VmError, raise,
    raise_runtime,
};

fn number(value: Tagged<'_, Value>, heap: &vm_core::Heap) -> Option<f64> {
    value.as_number(heap)
}

fn checked<'a>(
    value: i64,
    vm: &'a vm_core::VM,
    heap: &'a mut vm_core::Heap,
    state: &'a vm_core::ThreadState,
) -> Tagged<'a, Value> {
    if Smi::in_range(value) {
        Smi::new(value).into_tagged()
    } else {
        raise!(vm, heap, state, VmError::IntegerOverflow)
    }
}

fn boolean<'a>(heap: &'a vm_core::Heap, value: bool) -> Tagged<'a, Value> {
    if value {
        heap.known().true_object.get(heap).erase()
    } else {
        heap.known().false_object.get(heap).erase()
    }
}

/// Primitive equality with no coercion: nullish matches nullish, numbers
/// compare numerically across Smi/Float, strings by contents, everything
/// else by identity.
fn equals(heap: &vm_core::Heap, a: Tagged<'_, Value>, b: Tagged<'_, Value>) -> bool {
    if a.is_nullish(heap) && b.is_nullish(heap) {
        return true;
    }
    if let (Some(x), Some(y)) = (a.to_i64(), b.to_i64()) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (a.as_number(heap), b.as_number(heap)) {
        return x == y;
    }
    if let (Some(x), Some(y)) = (StringRef::try_from(heap, a), StringRef::try_from(heap, b)) {
        return x.to_rust_string(heap) == y.to_rust_string(heap);
    }
    a == b
}

macro_rules! numeric_binary {
    ($name:ident, $body:expr) => {
        pub fn $name<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
            let HostCtx {
                vm, heap, state, ..
            } = ctx;
            let (Some(a), Some(b)) = (
                number(call.receiver.as_tagged(heap), heap),
                number(args.get(heap, 0), heap),
            ) else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let op: fn(f64, f64) -> f64 = $body;
            let result = op(a, b);
            heap.new_number(result)
        }
    };
}

numeric_binary!(number_add, |a, b| a + b);
numeric_binary!(number_mul, |a, b| a * b);
numeric_binary!(number_div, |a, b| a / b);
numeric_binary!(number_rem, |a, b| a % b);

macro_rules! numeric_compare {
    ($name:ident, $body:expr) => {
        pub fn $name<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
            let HostCtx {
                vm, heap, state, ..
            } = ctx;
            let (Some(a), Some(b)) = (
                number(call.receiver.as_tagged(heap), heap),
                number(args.get(heap, 0), heap),
            ) else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let op: fn(f64, f64) -> bool = $body;
            boolean(heap, op(a, b))
        }
    };
}

numeric_compare!(number_lt, |a, b| a < b);
numeric_compare!(number_lte, |a, b| a <= b);
numeric_compare!(number_gt, |a, b| a > b);
numeric_compare!(number_gte, |a, b| a >= b);

/// `-x` (no argument) and `x - y` share the method name.
pub fn number_sub<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let receiver = call.receiver.as_tagged(heap);
    if args.is_empty() {
        if let Some(i) = receiver.to_i64() {
            return checked(i.checked_neg().unwrap_or(i64::MAX), vm, heap, state);
        }
        let Some(a) = number(receiver, heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType)
        };
        heap.new_number(-a)
    } else {
        let (Some(a), Some(b)) = (number(receiver, heap), number(args.get(heap, 0), heap)) else {
            raise!(vm, heap, state, VmError::InvalidOperandType)
        };
        heap.new_number(a - b)
    }
}

macro_rules! integer_binary {
    ($name:ident, $body:expr) => {
        pub fn $name<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
            let HostCtx {
                vm, heap, state, ..
            } = ctx;
            let a = call.receiver.as_tagged(heap).to_i64();
            let b = args.get(heap, 0).to_i64();
            let (Some(a), Some(b)) = (a, b) else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let op: fn(i64, i64) -> i64 = $body;
            checked(op(a, b), vm, heap, state)
        }
    };
}

integer_binary!(number_band, |a, b| a & b);
integer_binary!(number_bor, |a, b| a | b);
integer_binary!(number_bxor, |a, b| a ^ b);

fn shift_left<'a>(
    value: i64,
    count: i64,
    vm: &'a vm_core::VM,
    heap: &'a mut vm_core::Heap,
    state: &'a vm_core::ThreadState,
) -> Tagged<'a, Value> {
    if count < 0 {
        return shift_right(value, count.saturating_neg(), vm, heap, state);
    }
    if count >= 63 {
        return if value == 0 {
            Smi::new(0).into_tagged()
        } else {
            raise!(vm, heap, state, VmError::IntegerOverflow)
        };
    }
    let result = (value as i128) << (count as u32);
    if result < Smi::MIN as i128 || result > Smi::MAX as i128 {
        raise!(vm, heap, state, VmError::IntegerOverflow)
    }
    Smi::new(result as i64).into_tagged()
}

fn shift_right<'a>(
    value: i64,
    count: i64,
    vm: &'a vm_core::VM,
    heap: &'a mut vm_core::Heap,
    state: &'a vm_core::ThreadState,
) -> Tagged<'a, Value> {
    if count < 0 {
        return shift_left(value, count.saturating_neg(), vm, heap, state);
    }
    if count >= 64 {
        return Smi::new(if value < 0 { -1 } else { 0 }).into_tagged();
    }
    Smi::new(value >> count).into_tagged()
}

pub fn number_shift_left<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let (Some(value), Some(count)) = (
        call.receiver.as_tagged(heap).to_i64(),
        args.get(heap, 0).to_i64(),
    ) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    shift_left(value, count, vm, heap, state)
}

pub fn number_shift_right<'a>(
    ctx: HostCtx<'a>,
    call: CallInfo<'_>,
    args: Args,
) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let (Some(value), Some(count)) = (
        call.receiver.as_tagged(heap).to_i64(),
        args.get(heap, 0).to_i64(),
    ) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    shift_right(value, count, vm, heap, state)
}

pub fn number_bnot<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    match call.receiver.as_tagged(heap).to_i64() {
        Some(value) => Smi::new(!value).into_tagged(),
        None => raise!(vm, heap, state, VmError::InvalidOperandType),
    }
}

/// `String + value`: stringifies the right operand and concatenates.
pub fn string_add<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let receiver = scope.handle(call.receiver.as_tagged(heap));
        let other = match Convert::to_string(vm, heap, state, &scope, args.get_handle(heap, 0)) {
            Ok(value) => scope.handle(value),
            Err(error) => return raise_runtime(heap, error),
        };
        DenseString::concat(heap, &scope, receiver, other)
            .as_tagged(heap)
            .erase()
    })
}

macro_rules! string_compare {
    ($name:ident, $body:expr) => {
        pub fn $name<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
            let HostCtx {
                vm, heap, state, ..
            } = ctx;
            let a = StringRef::try_from(heap, call.receiver.as_tagged(heap));
            let b = StringRef::try_from(heap, args.get(heap, 0));
            let (Some(a), Some(b)) = (a, b) else {
                raise!(vm, heap, state, VmError::InvalidOperandType)
            };
            let op: fn(&str, &str) -> bool = $body;
            boolean(heap, op(&a.to_rust_string(heap), &b.to_rust_string(heap)))
        }
    };
}

string_compare!(string_lt, |a, b| a < b);
string_compare!(string_lte, |a, b| a <= b);
string_compare!(string_gt, |a, b| a > b);
string_compare!(string_gte, |a, b| a >= b);

pub fn any_eq<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    boolean(
        heap,
        equals(heap, call.receiver.as_tagged(heap), args.get(heap, 0)),
    )
}

pub fn any_ne<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    boolean(
        heap,
        !equals(heap, call.receiver.as_tagged(heap), args.get(heap, 0)),
    )
}

pub fn any_not<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx { heap, .. } = ctx;
    boolean(heap, !call.receiver.as_tagged(heap).is_truthy(heap))
}
