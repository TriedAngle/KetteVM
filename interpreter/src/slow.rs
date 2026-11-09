#[cfg(feature = "fast")]
use handler_derive::handler_become as handler;
#[cfg(not(feature = "fast"))]
use handler_derive::handler_match as handler;

use bytecode::Opcode;
use vm_core::ic::{InlineCache, StoreHit, StoreOutcomeKind};
use vm_core::proxy::{Flow, Proxy};
use vm_core::{
    AccessorPair, Coercion, Compare, ContextInit, ContextObject, Convert, DenseString, EntryClass,
    Errors, FixedArray, Handle, HandleScope, Heap, Hint, HostCtx, Key, LanguageMode, Object,
    PropertyDescriptor, Prototype, SetApplied, SlotName, Smi, StoreSemantics, Tagged, ThreadState,
    VM, Value, VmError, raise_runtime,
};

#[cfg(not(feature = "fast"))]
use crate::{Acc, Ctx};
#[cfg(feature = "fast")]
use crate::{Acc, Ctx, TableArg, throw_dispatch};
use crate::{FloatReg, Ops, Regs, RootsArg};
#[cfg(feature = "fast")]
use crate::{
    acc, call_enter, dispatch, dispatch_frame, float, ip, next, pc, reenter, regs, set_acc,
    set_acc_smi, set_float, tail,
};

#[inline(always)]
pub fn add_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    lhs: Handle<'_, Value>,
    rhs: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let lhs = match Object::to_primitive(vm, heap, state, lhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.exception.get(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let rhs = match Object::to_primitive(vm, heap, state, rhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.exception.get(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        if lhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
            || rhs.as_tagged(heap).get_as::<DenseString>(heap).is_some()
        {
            let a = match Convert::to_string(heap, &scope, lhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            let b = match Convert::to_string(heap, &scope, rhs) {
                Ok(v) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            DenseString::concat(heap, &scope, a, b)
                .as_tagged(heap)
                .erase()
        } else {
            let a = match lhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            let b = match rhs.as_tagged(heap).to_number(heap) {
                Ok(v) => v,
                Err(e) => return raise_runtime(vm, heap, state, e),
            };
            // IEEE `-0 + -0` yields +0; the spec demands -0
            let r = a + b;
            let r = if r == 0.0 && a.is_sign_negative() && b.is_sign_negative() {
                -0.0
            } else {
                r
            };
            heap.new_number(r)
        }
    })
}

pub fn bitwise_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    kind: u8,
    lhs: Handle<'_, Value>,
    rhs: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    let a = match Object::to_numeric(vm, heap, state, lhs) {
        Ok(Some(n)) => n,
        Ok(None) => return roots.exception.get(heap).erase(),
        Err(e) => return raise_runtime(vm, heap, state, e),
    };
    let b = match Object::to_numeric(vm, heap, state, rhs) {
        Ok(Some(n)) => n,
        Ok(None) => return roots.exception.get(heap).erase(),
        Err(e) => return raise_runtime(vm, heap, state, e),
    };
    let (ai, bi) = (Convert::number_to_int32(a), Convert::number_to_uint32(b));
    // kind: 0 = `|`, 1 = `^`, 2 = `&`, 3 = `<<`, 4 = `>>`, 5 = `>>>`
    let r: i64 = match kind {
        0 => (ai | (bi as i32)) as i64,
        1 => (ai ^ (bi as i32)) as i64,
        2 => (ai & (bi as i32)) as i64,
        3 => ai.wrapping_shl(bi & 31) as i64,
        4 => ai.wrapping_shr(bi & 31) as i64,
        // >>>: the uint32 result is always non-negative
        _ => Convert::number_to_uint32(a).wrapping_shr(bi & 31) as i64,
    };
    Smi::new(r).into_tagged()
}

/// compare cold tail: `cmp` selects the relation (0 = loose `==`,
/// 1 = strict `===`, 2 = `<`, 3 = `<=`, 4 = `>`, 5 = `>=`). Returns the
/// boolean result value or the sentinel (a coercion threw).
pub fn compare_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    cmp: u8,
    lhs: Handle<'_, Value>,
    rhs: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let x = lhs;
        let y = rhs;
        if cmp == 1 {
            let b = Compare::strict_equal(heap, x.as_tagged(heap), y.as_tagged(heap));
            return Convert::boolean(heap, b);
        }
        let loose = cmp == 0;
        let x_obj = Compare::is_object_operand(heap, x.as_tagged(heap));
        let y_obj = Compare::is_object_operand(heap, y.as_tagged(heap));
        if loose && x_obj && y_obj {
            let b = Compare::strict_equal(heap, x.as_tagged(heap), y.as_tagged(heap));
            return Convert::boolean(heap, b);
        }
        let hint = if loose { Hint::Default } else { Hint::Number };
        let x = if x_obj {
            match Object::to_primitive(vm, heap, state, x, hint) {
                Ok(Coercion::Threw) => return roots.exception.get(heap).erase(),
                Ok(Coercion::Value(v)) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            }
        } else {
            x
        };
        let y = if y_obj {
            match Object::to_primitive(vm, heap, state, y, hint) {
                Ok(Coercion::Threw) => return roots.exception.get(heap).erase(),
                Ok(Coercion::Value(v)) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            }
        } else {
            y
        };
        let b = match cmp {
            0 => Compare::equal(heap, &roots, x.as_tagged(heap), y.as_tagged(heap)),
            1 => Ok(Compare::strict_equal(
                heap,
                x.as_tagged(heap),
                y.as_tagged(heap),
            )),
            2 => Compare::less_than(heap, x.as_tagged(heap), y.as_tagged(heap)),
            3 => Compare::less_than_or_equal(heap, x.as_tagged(heap), y.as_tagged(heap)),
            4 => Compare::greater_than(heap, x.as_tagged(heap), y.as_tagged(heap)),
            _ => Compare::greater_than_or_equal(heap, x.as_tagged(heap), y.as_tagged(heap)),
        };
        match b {
            Ok(v) => Convert::boolean(heap, v),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    })
}

/// The `IncLoc`/`DecLoc` cold tail shared by both handlers: writes the
/// incremented register and returns the numeric *old* value (postfix
/// semantics). The smi tail covers tagged-range overflow.
#[allow(clippy::too_many_arguments)]
pub fn incdec_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    regs: Regs,
    r: i32,
    old: Handle<'_, Value>,
    delta: f64,
) -> Tagged<'a, Value> {
    // the old value is rooted by the caller; a Smi never moves anyway
    if let Some(bits) = old.as_tagged(heap).smi_bits() {
        let new = heap.new_number((bits >> 1) as f64 + delta);
        regs.set(r, new);
        return old.as_tagged(heap);
    }
    state.handle_scope(|scope| {
        let n = match Object::to_numeric(vm, heap, state, old) {
            Ok(Some(n)) => n,
            Ok(None) => return roots.exception.get(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let old_num: Handle<'_, Value> =
            if n.fract() == 0.0 && Smi::in_range(n as i64) && !(n == 0.0 && n.is_sign_negative()) {
                scope.handle(Smi::new(n as i64).into_tagged())
            } else {
                scope.handle(heap.new_number(n))
            };
        let new = heap.new_number(n + delta);
        regs.set(r, new);
        old_num.as_tagged(heap).erase()
    })
}

/// Map a `[[Set]]` outcome to the IC outcome kind. Strict-mode
/// rejections raise inside `Object::set_property`, so `Flow::Threw`
/// means a pending exception.
fn set_outcome_kind(applied: Flow<SetApplied>) -> Result<StoreOutcomeKind, ()> {
    match applied {
        Flow::Threw => Err(()),
        Flow::Value(SetApplied::Rejected) => Ok(StoreOutcomeKind::Done),
        Flow::Value(SetApplied::Done) => Ok(StoreOutcomeKind::Done),
        Flow::Value(SetApplied::Transition) => Ok(StoreOutcomeKind::Transition),
        Flow::Value(SetApplied::CallSetter) => Ok(StoreOutcomeKind::CallSetter),
    }
}

/// The named-store tail: generic `[[Set]]` (setters, traps, defines),
/// then the IC fill (a transition is cached against the map the store
/// produced; a setter ran before the fill).
#[allow(clippy::too_many_arguments)]
pub fn store_named_tail_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    base: usize,
    recv: Handle<'_, Value>,
    name: Handle<'_, SlotName>,
    value: Handle<'_, Value>,
    fb_slot: Option<usize>,
    semantics: StoreSemantics,
    mode: LanguageMode,
) -> Tagged<'a, Value> {
    store_named_tail(
        vm, heap, state, roots, base, recv, name, value, fb_slot, semantics, false, mode,
    )
}

/// [`store_named_tail_body`] with the contextual-store flag (strict
/// undeclared globals raise `ReferenceError` inside).
#[allow(clippy::too_many_arguments)]
pub fn store_named_tail<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    base: usize,
    recv: Handle<'_, Value>,
    name: Handle<'_, SlotName>,
    value: Handle<'_, Value>,
    fb_slot: Option<usize>,
    semantics: StoreSemantics,
    contextual: bool,
    mode: LanguageMode,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        // the receiver's map before the store: the state key when the
        // outcome transitions it
        let prev = recv
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| scope.handle(o.map(heap)));
        let key = scope.handle(name.as_tagged(heap).erase());
        let applied = match Object::set_property(
            vm,
            heap,
            state,
            recv,
            recv,
            key.erase(),
            value,
            semantics,
            contextual,
            mode,
        ) {
            Ok(applied) => applied,
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let kind = match set_outcome_kind(applied) {
            Ok(kind) => kind,
            Err(()) => return roots.exception.get(heap).erase(),
        };
        if let (Some(fb), Some(prev)) = (fb_slot, prev) {
            InlineCache::update_store(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(heap)
                    .map(|v| scope.handle(v)),
                fb,
                recv.erase(),
                name,
                prev,
                kind,
            );
        }
        value.as_tagged(heap).erase()
    })
}

// --- arithmetic ---

/// Box an already-computed `f64` when the inline `try_new_float` failed.
/// One instantiation per opcode: `reenter!` advances by the opcode's
/// size, so `Add`/`Sub`/`Mul`/`Div`/`Negate` each need their own.
macro_rules! box_number_slow {
    ($op:ident, $name:ident) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            frame.set_acc(frame.ctx().heap_mut().new_number(frame.float()));
            reenter!(pc)
        }
    };
}

#[handler(Add)]
#[cold]
pub fn slow_box_number(frame: Frame) {
    let pc = frame.pc();
    frame.set_acc(frame.ctx().heap_mut().new_number(frame.float()));
    reenter!(pc)
}
box_number_slow!(Sub, slow_box_number_sub);
box_number_slow!(Mul, slow_box_number_mul);
box_number_slow!(Div, slow_box_number_div);
box_number_slow!(Negate, slow_box_number_negate);

/// The full numeric cold tails, one handler per opcode.
macro_rules! numeric_slow {
    ($op:ident, $name:ident, |$a:ident, $b:ident| $body:expr) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            state.handle_scope(|scope| {
                let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
                let rhs = scope.handle(frame.acc());
                let heap = frame.ctx().heap_mut();
                match Object::numeric_op(vm, heap, state, lhs, rhs, |$a, $b| $body) {
                    Ok(Some(val)) => frame.set_acc(val),
                    Ok(None) => frame.set_acc(roots.exception.get(heap).erase()),
                    Err(e) => frame.set_acc(raise_runtime(vm, heap, state, e)),
                }
            });
            reenter!(pc)
        }
    };
    ($op:ident, $name:ident, immediate, |$a:ident, $b:ident| $body:expr) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            state.handle_scope(|scope| {
                let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
                let rhs = scope.handle(Smi::new(frame.signed::<1>() as i64).into_tagged());
                let heap = frame.ctx().heap_mut();
                match Object::numeric_op(vm, heap, state, lhs, rhs, |$a, $b| $body) {
                    Ok(Some(val)) => frame.set_acc(val),
                    Ok(None) => frame.set_acc(roots.exception.get(heap).erase()),
                    Err(e) => frame.set_acc(raise_runtime(vm, heap, state, e)),
                }
            });
            reenter!(pc)
        }
    };
}

numeric_slow!(Sub, slow_numeric, |a, b| a - b);
numeric_slow!(Mul, slow_mul, |a, b| a * b);
numeric_slow!(Div, slow_div, |a, b| a / b);
numeric_slow!(Mod, slow_mod, |a, b| a % b);
numeric_slow!(Exp, slow_exp, |a, b| a.powf(b));
numeric_slow!(SubImmediate, slow_numeric_immediate, immediate, |a, b| a
    - b);
numeric_slow!(MulImmediate, slow_mul_immediate, immediate, |a, b| a * b);
numeric_slow!(DivImmediate, slow_div_immediate, immediate, |a, b| a / b);
numeric_slow!(ModImmediate, slow_mod_immediate, immediate, |a, b| a % b);
numeric_slow!(ExpImmediate, slow_exp_immediate, immediate, |a, b| a
    .powf(b));

#[handler(Negate)]
#[cold]
pub fn slow_negate(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let arg = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        match Object::to_numeric(vm, heap, state, arg) {
            Ok(Some(n)) => heap.new_number(-n),
            Ok(None) => roots.exception.get(heap).erase(),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(Add)]
#[cold]
pub fn slow_add(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(frame.acc());
        add_body(vm, frame.ctx().heap_mut(), state, roots, lhs, rhs)
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(AddImmediate)]
#[cold]
pub fn slow_add_immediate(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(Smi::new(frame.signed::<1>() as i64).into_tagged());
        add_body(vm, frame.ctx().heap_mut(), state, roots, lhs, rhs)
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- local arithmetic ---

/// Box the `f64` the hot `AddLoc`/`SubLoc` already stored in the float
/// register, writing it back into the destination register.
macro_rules! box_loc_slow {
    ($op:ident, $name:ident) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let dst = frame.signed::<0>();
            let val = frame.ctx().heap_mut().new_number(frame.float());
            frame.regs().set(dst, val);
            frame.set_acc(val);
            reenter!(pc)
        }
    };
}

box_loc_slow!(AddLoc, slow_box_add_loc);
box_loc_slow!(SubLoc, slow_box_sub_loc);

/// The `AddLoc`/`SubLoc` cold tails: numeric add/sub, then write the
/// result back into the destination register unless it threw. The
/// sentinel is compared as a raw word through the `'static` known table,
/// so the check needs no second heap borrow while the result is live.
macro_rules! loc_slow {
    ($op:ident, $name:ident, $sub:expr) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let dst = frame.signed::<0>();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            let window = frame.regs();
            state.handle_scope(|scope| {
                let lhs = scope.handle(frame.regs().get(dst, frame.ctx().heap()));
                let rhs = scope.handle(frame.regs().get(frame.signed::<1>(), frame.ctx().heap()));
                let heap = frame.ctx().heap_mut();
                let known = &*roots;
                let val = if $sub {
                    match Object::numeric_op(vm, heap, state, lhs, rhs, |a, b| a - b) {
                        Ok(Some(val)) => val,
                        Ok(None) => roots.exception.get(heap).erase(),
                        Err(e) => raise_runtime(vm, heap, state, e),
                    }
                } else {
                    add_body(vm, heap, state, roots, lhs, rhs)
                };
                if val.raw().to_bits() != known.exception.raw().to_bits() {
                    window.set(dst, val);
                }
                frame.set_acc(val);
            });
            reenter!(pc)
        }
    };
}

loc_slow!(AddLoc, slow_add_loc, false);
loc_slow!(SubLoc, slow_sub_loc, true);

// --- bitwise / shifts ---

macro_rules! bitwise_slow {
    ($op:ident, $name:ident, $kind:expr) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            let val = state.handle_scope(|scope| {
                let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
                let rhs = scope.handle(frame.acc());
                bitwise_body(vm, frame.ctx().heap_mut(), state, roots, $kind, lhs, rhs)
            });
            frame.set_acc(val);
            reenter!(pc)
        }
    };
    ($op:ident, $name:ident, $kind:expr, immediate) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            let val = state.handle_scope(|scope| {
                let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
                let rhs = scope.handle(Smi::new(frame.signed::<1>() as i64).into_tagged());
                bitwise_body(vm, frame.ctx().heap_mut(), state, roots, $kind, lhs, rhs)
            });
            frame.set_acc(val);
            reenter!(pc)
        }
    };
}

bitwise_slow!(BitwiseOr, slow_bitwise_or, 0);
bitwise_slow!(BitwiseXor, slow_bitwise_xor, 1);
bitwise_slow!(BitwiseAnd, slow_bitwise_and, 2);
bitwise_slow!(ShiftLeft, slow_shift_left, 3);
bitwise_slow!(ShiftRight, slow_shift_right, 4);
bitwise_slow!(ShiftRightLogical, slow_shift_right_logical, 5);
bitwise_slow!(BitwiseOrImmediate, slow_bitwise_or_immediate, 0, immediate);
bitwise_slow!(
    BitwiseXorImmediate,
    slow_bitwise_xor_immediate,
    1,
    immediate
);
bitwise_slow!(
    BitwiseAndImmediate,
    slow_bitwise_and_immediate,
    2,
    immediate
);
bitwise_slow!(ShiftLeftImmediate, slow_shift_left_immediate, 3, immediate);
bitwise_slow!(
    ShiftRightImmediate,
    slow_shift_right_immediate,
    4,
    immediate
);
bitwise_slow!(
    ShiftRightLogicalImmediate,
    slow_shift_right_logical_immediate,
    5,
    immediate
);

// --- comparisons ---

#[handler(Equal)]
#[cold]
pub fn slow_equal(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.acc());
        let rhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        compare_body(vm, frame.ctx().heap_mut(), state, roots, 0, lhs, rhs)
    });
    frame.set_acc(v);
    reenter!(pc)
}

macro_rules! compare_slow {
    ($op:ident, $name:ident, $cmp:expr) => {
        #[handler($op)]
        #[cold]
        pub fn $name(frame: Frame) {
            let pc = frame.pc();
            let vm = frame.ctx().vm();
            let state = frame.ctx().state();
            let val = state.handle_scope(|scope| {
                let lhs = scope.handle(frame.acc());
                let rhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
                compare_body(vm, frame.ctx().heap_mut(), state, roots, $cmp, lhs, rhs)
            });
            frame.set_acc(val);
            reenter!(pc)
        }
    };
}

compare_slow!(LessThan, slow_less_than, 2);
compare_slow!(GreaterThan, slow_greater_than, 4);
compare_slow!(LessThanOrEqual, slow_less_than_or_equal, 3);
compare_slow!(GreaterThanOrEqual, slow_greater_than_or_equal, 5);

#[handler(InstanceOf)]
#[cold]
pub fn slow_instance_of(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let object = scope.handle(frame.acc());
        let callable = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        match Object::instance_of(vm, heap, state, object, callable) {
            Ok(Some(r)) => Convert::boolean(heap, r),
            Ok(None) => roots.exception.get(heap).erase(),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(CompareJump)]
#[cold]
pub fn slow_compare_jump(frame: Frame) {
    let pc = frame.pc();
    let next_pc = pc
        + (if STRIDE == 2 { 2 } else { 1 })
        + STRIDE * const { Opcode::CompareJump.operands().len() };
    let r = frame.signed::<0>();
    let kind = frame.unsigned::<1>();
    let off = frame.signed::<2>();
    let jump_pc = (pc as isize + off as isize) as usize;
    let cmp = (kind / 2) as u8;
    let b = {
        let heap = frame.ctx().heap();
        let lhs = frame.acc();
        let other = frame.regs().get(r, heap);
        match (lhs.as_number(heap), other.as_number(heap)) {
            (Some(a), Some(b)) => match cmp {
                0 | 1 => a == b,
                2 => a < b,
                3 => a <= b,
                4 => a > b,
                _ => a >= b,
            },
            _ => {
                let vm = frame.ctx().vm();
                let state = frame.ctx().state();
                let v = state.handle_scope(|scope| {
                    let lhs = scope.handle(frame.acc());
                    let rhs = scope.handle(frame.regs().get(r, frame.ctx().heap()));
                    compare_body(vm, frame.ctx().heap_mut(), state, roots, cmp, lhs, rhs)
                });
                // erase the heap borrow into the acc slot before the
                // sentinel read through the shared heap
                frame.set_acc(v);
                let result = frame.acc();
                let heap = frame.ctx().heap();
                if result.raw().to_bits() == roots.exception.get(heap).erase().raw().to_bits() {
                    // `reenter!` routes the sentinel into
                    // `throw_dispatch` at the faulting instruction
                    reenter!(pc)
                }
                result.is_truthy(heap)
            }
        }
    };
    let boolean = Convert::boolean(frame.ctx().heap(), b);
    frame.set_acc(boolean);
    frame.ctx().reload_code();
    let code = frame.ctx().code_ptr();
    let dest = unsafe {
        if b != (kind % 2 == 1) {
            code.add(jump_pc)
        } else {
            code.add(next_pc)
        }
    };
    let regs = unsafe { Regs::new(frame.ctx().regs_ptr()) };
    dispatch!(dest)
}

// --- literals ---

#[handler(CreateBlockContext)]
#[cold]
pub fn slow_create_block_context(frame: Frame) {
    let pc = frame.pc();
    let count = frame.unsigned::<0>();
    let base = frame.ctx().frame_base();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let outer = scope
            .cast::<ContextObject>(
                heap,
                state.stack().frame_header(base).context_slot().get(heap),
            )
            .expect("frame context slot holds a ContextObject");
        let slots = if count == 0 {
            roots.empty_fixed_array.get_handle(heap, &scope)
        } else {
            let values = scope.stage(&vec![roots.the_hole.get(heap).erase(); count]);
            heap.allocate_handle::<FixedArray>(values, &scope)
        };
        heap.allocate::<ContextObject>(ContextInit {
            outer: Some(outer),
            slots,
            scope_info: roots.empty_scope_info.get_handle(heap, &scope),
        })
        .erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

// --- locals ---

#[handler(IncLoc)]
#[cold]
pub fn slow_inc_loc(frame: Frame) {
    let pc = frame.pc();
    let r = frame.signed::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let old = scope.handle(frame.regs().get(r, frame.ctx().heap()));
        incdec_body(
            vm,
            frame.ctx().heap_mut(),
            state,
            roots,
            frame.regs(),
            r,
            old,
            1.0,
        )
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(DecLoc)]
#[cold]
pub fn slow_dec_loc(frame: Frame) {
    let pc = frame.pc();
    let r = frame.signed::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let old = scope.handle(frame.regs().get(r, frame.ctx().heap()));
        incdec_body(
            vm,
            frame.ctx().heap_mut(),
            state,
            roots,
            frame.regs(),
            r,
            old,
            -1.0,
        )
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- loads ---

#[allow(clippy::too_many_arguments)]
fn load_keyed_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    scope: &HandleScope<'_>,
    base: usize,
    fb: usize,
    recv: Handle<'_, Value>,
    key: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    {
        let recv = scope.handle(recv.as_tagged(heap).erase());
        let key = scope.handle(key.as_tagged(heap).erase());
        if let Ok(Key::Element(i)) = key.as_tagged(heap).erase().classify_key(heap) {
            InlineCache::update_load_element(
                heap,
                scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(heap)
                    .map(|v| scope.handle(v)),
                fb,
                Some(recv),
                i,
            );
            let name = scope.handle(key.as_tagged(heap).erase().as_name());
            if let Some(unit) = DenseString::index_element(heap, scope, recv, name) {
                return scope.handle(unit).as_tagged(heap).erase();
            }
        }
        let out = match Object::get_property(vm, heap, state, recv, recv, key, false) {
            Ok(Coercion::Value(v)) => scope.handle(v),
            Ok(Coercion::Threw) => roots.exception.get_handle(heap, scope).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        out.as_tagged(heap).erase()
    }
}

#[handler(LoadKeyedProperty)]
#[cold]
pub fn slow_keyed_load(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let fb = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let raw_key = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let key = match Object::to_property_key(vm, heap, state, raw_key) {
            Ok(Some(k)) => scope.handle(k),
            Ok(None) => return roots.exception.get(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        load_keyed_body(vm, heap, state, roots, &scope, base, fb, recv, key.erase())
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadKeyedPropertyReg)]
#[cold]
pub fn slow_keyed_load_reg(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let fb = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let raw_key = scope.handle(frame.regs().get(frame.signed::<1>(), frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        let key = match Object::to_property_key(vm, heap, state, raw_key) {
            Ok(Some(k)) => scope.handle(k),
            Ok(None) => return roots.exception.get(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        load_keyed_body(vm, heap, state, roots, &scope, base, fb, recv, key.erase())
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadElementImm)]
pub fn slow_keyed_load_imm(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let idx = frame.unsigned::<1>();
    let fb = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        let smi = Smi::new(idx as i64).into_tagged();
        let key = scope.handle(smi.as_name());
        load_keyed_body(vm, heap, state, roots, &scope, base, fb, recv, key.erase())
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadNamedProperty)]
#[cold]
pub fn slow_named_getter(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let getter = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        call_getter(vm, heap, state, &scope, recv, getter)
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadGlobal)]
#[cold]
pub fn slow_global_getter(frame: Frame) {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(roots.global_object.get(frame.ctx().heap()).erase());
        let getter = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        call_getter(vm, heap, state, &scope, recv, getter)
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadNamedProperty)]
pub fn slow_named_load(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| -> Acc {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .callable(heap, base)
                .constant_slot_name(heap, name_idx),
        );
        let out = match Object::get_property(vm, heap, state, recv, recv, name.erase(), false) {
            Ok(result) => match result {
                Coercion::Value(v) => scope.handle(v),
                Coercion::Threw => roots.exception.get_handle(heap, &scope).erase(),
            },
            Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
        };
        {
            InlineCache::update_load(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(heap)
                    .map(|v| scope.handle(v)),
                fb_slot,
                recv.as_tagged(heap)
                    .as_heap_object()
                    .map(|o| scope.handle(o)),
                scope.handle(name.as_tagged(heap).erase().as_name()),
                true,
            );
        }
        Acc::set(out.as_tagged(heap).erase())
    });
    frame.set_acc(v.get(frame.ctx().heap()));
    reenter!(pc)
}

#[allow(clippy::too_many_arguments)]
pub fn global_load_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    base: usize,
    name_idx: usize,
    fb_slot: usize,
    throws: bool,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let global = roots.global_object.get(heap);
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );
        let global_handle = scope.handle(global);
        let global_value = scope.handle(global_handle.as_tagged(heap).erase());
        let name_value = name.as_tagged(heap).erase();
        let lexical = match vm_core::global_lexical_get(heap, name_value) {
            Ok(Some((value, binding))) => Some((scope.handle(value), binding)),
            Ok(None) => None,
            Err(err) => return raise_runtime(vm, heap, state, err),
        };
        if let Some((value, binding)) = lexical {
            InlineCache::update_global_lexical(
                heap,
                state.stack().frame_header(base).feedback(heap),
                fb_slot,
                binding.context,
                binding.slot_index,
                binding.info,
            );
            return value.as_tagged(heap).erase();
        }
        let out = match Object::get_property(
            vm,
            heap,
            state,
            global_value,
            global_value,
            name.erase(),
            throws,
        ) {
            Ok(result) => match result {
                Coercion::Value(v) => scope.handle(v),
                Coercion::Threw => roots.exception.get_handle(heap, &scope).erase(),
            },
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        {
            InlineCache::update_load(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(heap)
                    .map(|v| scope.handle(v)),
                fb_slot,
                Some(global_handle),
                scope.handle(name.as_tagged(heap).erase().as_name()),
                false,
            );
        }
        out.as_tagged(heap).erase()
    })
}

#[handler(LoadGlobal)]
#[cold]
pub fn slow_global_load(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<0>();
    let fb_slot = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = global_load_body(
        vm,
        frame.ctx().heap_mut(),
        state,
        roots,
        base,
        name_idx,
        fb_slot,
        true,
    );
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(LoadGlobalNoThrow)]
#[cold]
pub fn slow_global_load_no_throw(frame: Frame) {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<0>();
    let fb_slot = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = global_load_body(
        vm,
        frame.ctx().heap_mut(),
        state,
        roots,
        base,
        name_idx,
        fb_slot,
        false,
    );
    frame.set_acc(val);
    reenter!(pc)
}

// --- stores ---

#[allow(clippy::too_many_arguments)]
pub fn keyed_store_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    base: usize,
    fb: usize,
    recv: Handle<'_, Value>,
    raw_key: Handle<'_, Value>,
    value: Handle<'_, Value>,
    semantics: StoreSemantics,
    mode: LanguageMode,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let key = match Object::to_property_key(vm, heap, state, raw_key) {
            Ok(Some(k)) => scope.handle(k),
            Ok(None) => return roots.exception.get(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        // Fresh array/list construction is the hot no-shadow case: a dense
        // array element can be stored directly without walking the chain.
        if semantics == StoreSemantics::WriteThrough
            && let Some(i) = key.as_tagged(heap).as_index()
            && let Some(obj) = recv.as_tagged(heap).as_heap_object()
            && obj.is_array(heap)
            && obj.map(heap).kind().is_dense_elements()
        {
            let grew = i >= obj.length();
            let obj = scope.handle(obj);
            if let Err(e) = Object::store_array_element(heap, &scope, &obj, i, &value) {
                return raise_runtime(vm, heap, state, e);
            }
            InlineCache::update_store_element(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(heap)
                    .map(|v| scope.handle(v)),
                fb,
                Some(recv),
                grew,
            );
            return value.as_tagged(heap).erase();
        }
        let element = key.as_tagged(heap).as_index();
        let grew = element.is_some_and(|i| {
            recv.as_tagged(heap)
                .as_heap_object()
                .is_some_and(|obj| obj.is_array(heap) && i >= obj.length())
        });
        let applied = match Object::set_property(
            vm,
            heap,
            state,
            recv,
            recv,
            key.erase(),
            value,
            semantics,
            false,
            mode,
        ) {
            Ok(applied) => applied,
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        match set_outcome_kind(applied) {
            Ok(kind) => {
                if kind != StoreOutcomeKind::CallSetter
                    && element.is_some()
                    && recv
                        .as_tagged(heap)
                        .as_heap_object()
                        .is_some_and(|obj| obj.is_array(heap))
                {
                    InlineCache::update_store_element(
                        heap,
                        &scope,
                        state
                            .stack()
                            .frame_header(base)
                            .feedback(heap)
                            .map(|v| scope.handle(v)),
                        fb,
                        Some(recv),
                        grew,
                    );
                }
                value.as_tagged(heap).erase()
            }
            Err(()) => roots.exception.get(heap).erase(),
        }
    })
}

#[handler(StoreKeyedProperty)]
pub fn slow_keyed_store(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let fb = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let raw_key = scope.handle(frame.regs().get(frame.signed::<1>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        keyed_store_body(
            vm,
            frame.ctx().heap_mut(),
            state,
            roots,
            base,
            fb,
            recv,
            raw_key,
            value,
            StoreSemantics::Shadow,
            mode,
        )
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(StoreKeyedPropertyNoShadow)]
pub fn slow_keyed_store_no_shadow(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let fb = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let raw_key = scope.handle(frame.regs().get(frame.signed::<1>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        keyed_store_body(
            vm,
            frame.ctx().heap_mut(),
            state,
            roots,
            base,
            fb,
            recv,
            raw_key,
            value,
            StoreSemantics::WriteThrough,
            mode,
        )
    });
    frame.set_acc(val);
    reenter!(pc)
}

pub fn call_getter<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    scope: &HandleScope<'_>,
    receiver: Handle<'_, Value>,
    getter: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    let args = scope.stage(&[receiver.as_tagged(heap).erase()]);
    match HostCtx::enter(vm, state.runtime(vm, heap), heap, state, getter, args, None) {
        Ok(v) => scope.handle(v).as_tagged(heap).erase(),
        Err(e) => raise_runtime(vm, heap, state, e),
    }
}

macro_rules! apply_store_hit {
    ($vm:expr, $heap:expr, $state:expr, $roots:expr, $scope:expr, $recv:expr, $value:expr, $hit:expr, $mode:expr) => {
        match $hit {
            StoreHit::Done => $value.as_tagged($heap).erase(),
            StoreHit::Setter(setter) => {
                let setter = $scope.handle(setter);
                let args = $scope.stage(&[
                    $recv.as_tagged($heap).erase(),
                    $value.as_tagged($heap).erase(),
                ]);
                match HostCtx::enter(
                    $vm,
                    $state.runtime($vm, $heap),
                    $heap,
                    $state,
                    setter,
                    args,
                    None,
                ) {
                    Ok(v) => $scope.handle(v).as_tagged($heap).erase(),
                    Err(e) => raise_runtime($vm, $heap, $state, e),
                }
            }
            StoreHit::NativeSetter(index) => {
                let args = $scope
                    .stage(&[
                        $recv.as_tagged($heap).erase(),
                        $value.as_tagged($heap).erase(),
                    ])
                    .as_args();
                let result = $scope.handle($vm.native(index)(
                    HostCtx::new($vm, $state.runtime($vm, $heap), $heap, $state),
                    None,
                    args,
                ));
                if result.as_tagged($heap) == $roots.exception.get($heap).erase() {
                    result.as_tagged($heap).erase()
                } else if result.as_tagged($heap) == $roots.false_object.get($heap).erase() {
                    // rejected by the native setter (ArraySetLength): a
                    // strict store throws, a sloppy one is a no-op
                    if $mode.is_strict() {
                        raise_runtime($vm, $heap, $state, VmError::Type)
                    } else {
                        $value.as_tagged($heap).erase()
                    }
                } else {
                    $value.as_tagged($heap).erase()
                }
            }
        }
    };
}

macro_rules! cached_store {
    ($vm:expr, $heap:expr, $state:expr, $roots:expr, $scope:expr, $base:expr, $fb:expr, $recv:expr, $name:expr, $value:expr, $mode:expr) => {{
        if let Some(hit) = InlineCache::try_store(
            $heap,
            $scope,
            $state
                .stack()
                .frame_header($base)
                .feedback($heap)
                .map(|v| $scope.handle(v)),
            $fb,
            $recv.erase(),
            $name,
            $value,
            $mode,
        ) {
            return apply_store_hit!(
                $vm, $heap, $state, $roots, $scope, $recv, $value, hit, $mode
            );
        }
    }};
}

#[handler(StoreNamedProperty)]
pub fn slow_store_named(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );

        cached_store!(
            vm, heap, state, roots, &scope, base, fb_slot, recv, name, value, mode
        );
        store_named_tail_body(
            vm,
            heap,
            state,
            roots,
            base,
            recv,
            name,
            value,
            Some(fb_slot),
            StoreSemantics::Shadow,
            mode,
        )
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(StoreGlobal)]
pub fn slow_store_global(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<0>();
    let fb_slot = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );
        let global = roots.global_object.get_handle(heap, &scope).erase();
        let name_value = name.as_tagged(heap).erase();
        match vm_core::global_lexical_set(heap, name_value, value.as_tagged(heap)) {
            Ok(Some(binding)) => {
                InlineCache::update_global_lexical(
                    heap,
                    state.stack().frame_header(base).feedback(heap),
                    fb_slot,
                    binding.context,
                    binding.slot_index,
                    binding.info,
                );
                return value.as_tagged(heap).erase();
            }
            Ok(None) => {}
            Err(err) => return raise_runtime(vm, heap, state, err),
        }
        cached_store!(
            vm, heap, state, roots, &scope, base, fb_slot, global, name, value, mode
        );
        // contextual store: a strict undeclared global raises
        // ReferenceError inside `Object::set_property`
        store_named_tail(
            vm,
            heap,
            state,
            roots,
            base,
            global,
            name,
            value,
            Some(fb_slot),
            StoreSemantics::WriteThrough,
            true,
            mode,
        )
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(StoreNamedPropertyNoShadow)]
pub fn slow_store_named_no_shadow(frame: Frame) {
    let mode = frame.ctx().language_mode();
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );
        store_named_tail_body(
            vm,
            heap,
            state,
            roots,
            base,
            recv,
            name,
            value,
            None,
            StoreSemantics::WriteThrough,
            mode,
        )
    });
    frame.set_acc(val);
    reenter!(pc)
}

#[handler(AddParent)]
#[cold]
pub fn slow_add_parent(frame: Frame) {
    let pc = frame.pc();
    let recv_reg = frame.signed::<0>();
    let name_idx = frame.unsigned::<1>();
    let base = frame.ctx().frame_base();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let receiver = scope.handle(frame.regs().get(recv_reg, frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(heap)
                .at(heap, name_idx)
                .erase()
                .as_name(),
        );
        let Some(receiver) = scope.cast::<Object>(heap, receiver.as_tagged(heap)) else {
            return raise_runtime(vm, heap, state, VmError::Type);
        };
        if let Err(e) = Object::add_parent(heap, &scope, receiver, name, value) {
            return raise_runtime(vm, heap, state, e);
        }
        value.as_tagged(heap).erase()
    });
    frame.set_acc(val);
    reenter!(pc)
}

// --- construct ---

#[handler(Construct)]
pub fn slow_construct(frame: Frame) {
    let pc = frame.pc();
    let callee_reg = frame.signed::<0>();
    let args_base = frame.signed::<1>();
    let count = frame.unsigned::<2>();
    let base = frame.ctx().frame_base();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let val = state.handle_scope(|scope| {
        let callee = scope.handle(frame.regs().get(callee_reg, frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        let Some(obj) = callee.as_tagged(heap).as_heap_object() else {
            return Acc::set(raise_runtime(vm, heap, state, VmError::Type));
        };
        if !obj.map(heap).kind().is_constructor() {
            return Acc::set(raise_runtime(vm, heap, state, VmError::Type));
        }
        let args = state.stack().args(base, args_base, count);
        if Proxy::is_proxy(heap, callee.as_tagged(heap)) {
            return match Proxy::construct(vm, heap, state, callee, args, callee) {
                Ok(Flow::Threw) => Acc::set(roots.exception.get(heap).erase()),
                Ok(Flow::Value(v)) => Acc::set(v),
                Err(e) => Acc::set(raise_runtime(vm, heap, state, e)),
            };
        }
        if obj.map(heap).kind().is_bound() {
            let forwarded: Vec<Handle<'_, Value>> = (0..args.len())
                .map(|index| args.get_handle(heap, index))
                .collect();
            return match HostCtx::enter_bound(
                vm,
                state.runtime(vm, heap),
                heap,
                state,
                callee,
                &forwarded,
                Some(callee),
            ) {
                Ok(value) => Acc::set(value),
                Err(e) => Acc::set(raise_runtime(vm, heap, state, e)),
            };
        }
        // a derived constructor's `this` is the hole until `super()` binds
        // it; ordinary and base-class constructors synthesize the receiver
        let derived = obj.map(heap).kind().is_class_constructor()
            && obj
                .callable_info(heap)
                .is_some_and(|info| info.function_kind().is_derived_class_constructor());
        let callee_obj = scope
            .cast::<Object>(heap, callee.as_tagged(heap))
            .expect("constructible callee is an object");
        let receiver = if derived {
            roots.the_hole.get_handle(heap, &scope).erase()
        } else {
            match Object::create_construct_receiver_value(vm, heap, state, callee_obj.erase()) {
                Ok(Some(r)) => scope.handle(r),
                Ok(None) => return Acc::set(roots.exception.get(heap).erase()),
                Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
            }
        };
        let mut staged: Vec<Tagged<'_, Value>> = Vec::with_capacity(count + 1);
        staged.push(receiver.as_tagged(heap).erase());
        staged.extend(args.iter(heap));
        let staged = scope.stage(&staged);
        let result = match HostCtx::enter(
            vm,
            state.runtime(vm, heap),
            heap,
            state,
            callee_obj.erase(),
            staged,
            Some(callee_obj.erase()),
        ) {
            Ok(v) => scope.handle(v),
            Err(e) => return Acc::set(raise_runtime(vm, heap, state, e)),
        };
        if result.as_tagged(heap) == roots.exception.get(heap).erase() {
            return Acc::set(roots.exception.get(heap).erase());
        }
        if result.as_tagged(heap).is_primitive(heap) {
            if derived {
                return Acc::set(raise_runtime(vm, heap, state, VmError::Type));
            }
            Acc::set(receiver.as_tagged(heap).erase())
        } else {
            Acc::set(result.as_tagged(heap).erase())
        }
    });
    frame.set_acc(val.get(frame.ctx().heap()));
    reenter!(pc)
}
