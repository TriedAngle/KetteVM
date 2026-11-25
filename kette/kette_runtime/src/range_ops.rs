use vm_core::{
    Args, CallInfo, DenseString, HostCtx, Range, RangeInit, Smi, Tagged, Value, VmError, raise,
};

fn boolean<'a>(heap: &'a vm_core::Heap, value: bool) -> Tagged<'a, Value> {
    if value {
        heap.known().true_object.get(heap).erase()
    } else {
        heap.known().false_object.get(heap).erase()
    }
}

fn range_arg(heap: &vm_core::Heap, args: Args, index: usize) -> Option<i64> {
    args.get(heap, index).to_i64()
}

fn allocate<'a>(
    heap: &'a mut vm_core::Heap,
    start: i64,
    end: i64,
    current: i64,
    inclusive: bool,
) -> Tagged<'a, Value> {
    heap.allocate::<Range>(RangeInit {
        start,
        end,
        current,
        inclusive,
    })
    .erase()
}

pub fn new<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let (Some(start), Some(end)) = (range_arg(heap, args, 0), range_arg(heap, args, 1)) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let inclusive = args.get(heap, 2).is_truthy(heap);
    allocate(heap, start, end, start, inclusive)
}

pub fn inclusive<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let (Some(start), Some(end)) = (range_arg(heap, args, 0), range_arg(heap, args, 1)) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    allocate(heap, start, end, start, true)
}

pub fn exclusive<'a>(ctx: HostCtx<'a>, _call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let (Some(start), Some(end)) = (range_arg(heap, args, 0), range_arg(heap, args, 1)) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    allocate(heap, start, end, start, false)
}

pub fn iterator<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let range = range.as_ref();
    allocate(
        heap,
        range.start(),
        range.end(),
        range.start(),
        range.inclusive(),
    )
}

pub fn has_next<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    boolean(heap, range.as_ref().has_next())
}

pub fn next<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    Smi::new(range.as_ref().next_value(heap)).into_tagged()
}

pub fn size<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    Smi::new(range.as_ref().count()).into_tagged()
}

pub fn is_empty<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    boolean(heap, !range.as_ref().has_next())
}

pub fn get<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let Some(index) = range_arg(heap, args, 0) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let range = range.as_ref();
    if index < 0 || index >= range.count() {
        raise!(vm, heap, state, VmError::IndexOutOfBounds)
    }
    let value = range.start() + index * range.step();
    Smi::new(value).into_tagged()
}

pub fn contains<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    let Some(value) = range_arg(heap, args, 0) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    boolean(heap, range.as_ref().contains(value))
}

pub fn start<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    Smi::new(range.as_ref().start()).into_tagged()
}

pub fn end<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
        raise!(vm, heap, state, VmError::InvalidOperandType)
    };
    Smi::new(range.as_ref().end()).into_tagged()
}

pub fn to_string<'a>(ctx: HostCtx<'a>, call: CallInfo<'_>, _args: Args) -> Tagged<'a, Value> {
    let HostCtx {
        vm, heap, state, ..
    } = ctx;
    state.handle_scope(|scope| {
        let Some(range) = call.receiver.as_tagged(heap).get_as::<Range>(heap) else {
            raise!(vm, heap, state, VmError::InvalidOperandType)
        };
        let range = range.as_ref();
        let separator = if range.inclusive() { "..=" } else { ".." };
        let text = format!("{}{}{}", range.start(), separator, range.end());
        let text = DenseString::from_utf8(heap, &scope, &text);
        text.as_tagged(heap).erase()
    })
}
