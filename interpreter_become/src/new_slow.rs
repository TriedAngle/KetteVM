//! Slow paths of the `new` become shell: every body a hot handler
//! tails into when its inline fast-path check fails (numeric fallbacks,
//! IC misses, allocation, proxies). Each is a `#[handler]` instantiation
//! at both strides, `pub` so the parent's dispatch table can
//! reference it; anything that can allocate re-derives `ip`/`regs` from
//! the frame header and re-enters through the code-relative pc captured
//! up front.
//!
//! The `#[handler]` expansion emits calls to the `next!`/`dispatch!`/
//! `reenter!`/`tail!` macros defined in the parent module; the module is
//! declared after them so macro_rules' textual scope reaches this file.

use handler_derive::handler;

use bytecode::Opcode;
use vm_core::ic::{InlineCache, StoreHit, StoreOutcomeKind};
use vm_core::proxy::Proxy;
use vm_core::{
    AccessorPair, Coercion, Compare, Convert, DenseString, EntryClass, Errors, Handle, Heap, Hint,
    HostCtx, Key, LoadOutcome, Lookup, Object, PropertyDescriptor, Prototype, Smi, SlotName,
    StoreOutcome, StoreSemantics, Tagged, ThreadState, VM, Value, raise_runtime,
};

use crate::new::{Acc, Ctx, TableArg, throw_dispatch};
use crate::new_macros::*;
use crate::{FloatReg, Ops, Regs, RootsArg};

#[inline(always)]
fn add_body<'a>(
    vm: &'a VM,
    heap: &'a mut Heap,
    state: &'a ThreadState,
    roots: RootsArg<'_>,
    lhs: Handle<'_, Value>,
    rhs: Handle<'_, Value>,
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        let lhs = match Object::to_primitive(vm, heap, state, lhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Ok(Coercion::Value(v)) => scope.handle(v),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let rhs = match Object::to_primitive(vm, heap, state, rhs, Hint::Default) {
            Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
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


fn bitwise_body<'a>(
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
        Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
        Err(e) => return raise_runtime(vm, heap, state, e),
    };
    let b = match Object::to_numeric(vm, heap, state, rhs) {
        Ok(Some(n)) => n,
        Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
        Err(e) => return raise_runtime(vm, heap, state, e),
    };
    let (ai, bi) = (Convert::number_to_int32(a), Convert::number_to_uint32(b));
    let r: i64 = match kind {
        3 => ai.wrapping_shl(bi & 31) as i64,
        _ => ai.wrapping_shr(bi & 31) as i64,
    };
    Smi::new(r).into_tagged()
}

/// The compare cold tail: `cmp` selects the relation (0 = loose `==`,
/// 2 = `<`, 3 = `<=`, 4 = `>`, 5 = `>=`). Returns the boolean result
/// value or the sentinel (a coercion threw).
fn compare_body<'a>(
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
                Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            }
        } else {
            x
        };
        let y = if y_obj {
            match Object::to_primitive(vm, heap, state, y, hint) {
                Ok(Coercion::Threw) => return roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => scope.handle(v),
                Err(e) => return raise_runtime(vm, heap, state, e),
            }
        } else {
            y
        };
        let b = match cmp {
            0 => Compare::equal(heap, x.as_tagged(heap), y.as_tagged(heap)),
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
fn incdec_body<'a>(
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
            Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
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

/// Apply a store outcome: transitions define the own property, setters
/// run as a blocking call (their return value is ignored; the stored
/// value stays the expression's result). `Err(())` = threw (pending set).
fn apply_store_outcome_body(
    vm: &VM,
    heap: &mut Heap,
    state: &ThreadState,
    roots: RootsArg<'_>,
    receiver: Handle<'_, Value>,
    outcome: StoreOutcome<'_>,
    value: Handle<'_, Value>,
) -> Result<(), ()> {
    match outcome {
        StoreOutcome::Transition { receiver: recv, name } => state.handle_scope(|scope| {
            match Object::add_own_property(
                heap,
                &scope,
                recv,
                name,
                PropertyDescriptor::data(value),
            ) {
                // TODO(strict-mode): a false result must throw in strict
                // code; the current store path preserves its existing
                // sloppy result.
                Ok(_) => Ok(()),
                Err(e) => {
                    raise_runtime(vm, heap, state, e);
                    Err(())
                }
            }
        }),
        StoreOutcome::CallSetter { setter } => state.handle_scope(|scope| {
            let args = scope.stage(&[receiver.as_tagged(heap).erase(), value.as_tagged(heap)]);
            match HostCtx::enter(vm, heap, state, setter, args, None) {
                Ok(v) => {
                    // erase the heap borrow before the sentinel read
                    let bits = v.raw().to_bits();
                    let sentinel = roots.known(heap).exception.as_tagged(heap).erase();
                    if bits == sentinel.raw().to_bits() {
                        Err(())
                    } else {
                        Ok(())
                    }
                }
                Err(e) => {
                    raise_runtime(vm, heap, state, e);
                    Err(())
                }
            }
        }),
        StoreOutcome::Done => Ok(()),
    }
}

/// The named-store tail: generic `store_lookup`, outcome application,
/// then the IC fill (a transition is cached against the map the store
/// produced; a setter ran before the fill).
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
) -> Tagged<'a, Value> {
    state.handle_scope(|scope| {
        // the receiver's map before the store: the state key when the
        // outcome transitions it
        let prev = recv
            .as_tagged(heap)
            .as_heap_object()
            .map(|o| scope.handle(o.map(heap)));
        let outcome = match recv.as_tagged(heap).erase().store_lookup(
            heap,
            &scope,
            name.as_tagged(heap),
            value.as_tagged(heap),
            semantics,
        ) {
            Ok(o) => o,
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let kind = match &outcome {
            StoreOutcome::Done => StoreOutcomeKind::Done,
            StoreOutcome::Transition { .. } => StoreOutcomeKind::Transition,
            StoreOutcome::CallSetter { .. } => StoreOutcomeKind::CallSetter,
        };
        if let Err(()) = apply_store_outcome_body(vm, heap, state, roots, recv, outcome, value) {
            return roots.known(heap).exception.as_tagged(heap).erase();
        }
        if let (Some(fb), Some(prev)) = (fb_slot, prev) {
            InlineCache::update_store(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(&*heap)
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

#[handler(Add)]
#[cold]
pub fn slow_box_number(frame: Frame) -> Acc {
    let pc = frame.pc();
    frame.set_acc(frame.ctx().heap_mut().new_number(frame.float()));
    reenter!(pc)
}

#[handler(Sub)]
#[cold]
pub fn slow_box_number_sub(frame: Frame) -> Acc {
    let pc = frame.pc();
    frame.set_acc(frame.ctx().heap_mut().new_number(frame.float()));
    reenter!(pc)
}

#[handler(Add)]
#[cold]
pub fn slow_add(frame: Frame) -> Acc {
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

#[handler(Sub)]
#[cold]
pub fn slow_numeric(frame: Frame) -> Acc {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        match Object::numeric_op(vm, heap, state, lhs, rhs, |a, b| a - b) {
            Ok(Some(v)) => v,
            Ok(None) => roots.known(heap).exception.as_tagged(heap).erase(),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(AddImmediate)]
#[cold]
pub fn slow_add_immediate(frame: Frame) -> Acc {
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

#[handler(SubImmediate)]
#[cold]
pub fn slow_numeric_immediate(frame: Frame) -> Acc {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(Smi::new(frame.signed::<1>() as i64).into_tagged());
        let heap = frame.ctx().heap_mut();
        match Object::numeric_op(vm, heap, state, lhs, rhs, |a, b| a - b) {
            Ok(Some(v)) => v,
            Ok(None) => roots.known(heap).exception.as_tagged(heap).erase(),
            Err(e) => raise_runtime(vm, heap, state, e),
        }
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(ShiftRightImmediate)]
#[cold]
pub fn slow_bitwise_immediate(frame: Frame) -> Acc {
    let pc = frame.pc();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let lhs = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let rhs = scope.handle(Smi::new(frame.signed::<1>() as i64).into_tagged());
        bitwise_body(vm, frame.ctx().heap_mut(), state, roots, 4, lhs, rhs)
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- comparisons ---

#[handler(Equal)]
#[cold]
pub fn slow_equal(frame: Frame) -> Acc {
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

#[handler(CompareJump)]
#[cold]
pub fn slow_compare_jump(frame: Frame) -> Acc {
    // every continuation target is captured code-relative BEFORE the
    // compare below: it may allocate and move the code object, after
    // which the old ip no longer pairs with the frame-header base
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
                if result.raw().to_bits()
                    == roots
                        .known(heap)
                        .exception
                        .as_tagged(heap)
                        .erase()
                        .raw()
                        .to_bits()
                {
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

// --- locals ---

#[handler(IncLoc)]
#[cold]
pub fn slow_inc_loc(frame: Frame) -> Acc {
    let pc = frame.pc();
    let r = frame.signed::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let old = scope.handle(frame.regs().get(r, frame.ctx().heap()));
        incdec_body(vm, frame.ctx().heap_mut(), state, roots, frame.regs(), r, old, 1.0)
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(DecLoc)]
#[cold]
pub fn slow_dec_loc(frame: Frame) -> Acc {
    let pc = frame.pc();
    let r = frame.signed::<0>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let old = scope.handle(frame.regs().get(r, frame.ctx().heap()));
        incdec_body(vm, frame.ctx().heap_mut(), state, roots, frame.regs(), r, old, -1.0)
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- loads ---

#[handler(LoadKeyedProperty)]
#[cold]
pub fn slow_keyed_load(frame: Frame) -> Acc {
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
            Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };

        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::get(vm, heap, state, recv, recv, key.erase()) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => v,
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        let element_index = match key.as_tagged(heap).erase().classify_key(heap) {
            Ok(Key::Element(i)) => Some(i),
            _ => None,
        };
        if let Some(i) = element_index {
            InlineCache::update_load_element(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(&*heap)
                    .map(|v| scope.handle(v)),
                fb,
                Some(recv),
                i,
            );
            if let Some(unit) = DenseString::index_element(heap, &scope, recv, key) {
                return scope.handle(unit).as_tagged(heap).erase();
            }
        }
        let out = match recv.as_tagged(heap).load_outcome_keyed(heap, key.as_tagged(heap)) {
            Ok(LoadOutcome::Value(v)) => scope.handle(v),
            Ok(LoadOutcome::Getter(getter)) => {
                let getter = scope.handle(getter);
                let args = scope.stage(&[recv.as_tagged(heap).erase()]);
                match HostCtx::enter(vm, heap, state, getter, args, None) {
                    Ok(v) => scope.handle(v),
                    Err(e) => return raise_runtime(vm, heap, state, e),
                }
            }
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        out.as_tagged(heap).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadKeyedPropertyReg)]
#[cold]
pub fn slow_keyed_load_reg(frame: Frame) -> Acc {
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
            Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::get(vm, heap, state, recv, recv, key.erase()) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => v,
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        let element_index = match key.as_tagged(heap).erase().classify_key(heap) {
            Ok(Key::Element(i)) => Some(i),
            _ => None,
        };
        if let Some(i) = element_index {
            InlineCache::update_load_element(
                heap,
                &scope,
                state
                    .stack()
                    .frame_header(base)
                    .feedback(&*heap)
                    .map(|v| scope.handle(v)),
                fb,
                Some(recv),
                i,
            );
            if let Some(unit) = DenseString::index_element(heap, &scope, recv, key) {
                return scope.handle(unit).as_tagged(heap).erase();
            }
        }
        let out = match recv.as_tagged(heap).load_outcome_keyed(heap, key.as_tagged(heap)) {
            Ok(LoadOutcome::Value(v)) => scope.handle(v),
            Ok(LoadOutcome::Getter(getter)) => {
                let getter = scope.handle(getter);
                let args = scope.stage(&[recv.as_tagged(heap).erase()]);
                match HostCtx::enter(vm, heap, state, getter, args, None) {
                    Ok(v) => scope.handle(v),
                    Err(e) => return raise_runtime(vm, heap, state, e),
                }
            }
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        out.as_tagged(heap).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadElementImm)]
pub fn slow_keyed_load_imm(frame: Frame) -> Acc {
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
        let key: Handle<'_, SlotName> = scope.handle(smi.as_name());
        InlineCache::update_load_element(
            heap,
            &scope,
            state
                .stack()
                .frame_header(base)
                .feedback(&*heap)
                .map(|v| scope.handle(v)),
            fb,
            Some(recv),
            idx,
        );
        if let Some(unit) = DenseString::index_element(heap, &scope, recv, key) {
            return scope.handle(unit).as_tagged(heap).erase();
        }
        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::get(vm, heap, state, recv, recv, key.erase()) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => v,
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        let out = match recv.as_tagged(heap).load_outcome_keyed(heap, key.as_tagged(heap)) {
            Ok(LoadOutcome::Value(v)) => scope.handle(v),
            Ok(LoadOutcome::Getter(getter)) => {
                let getter = scope.handle(getter);
                let args = scope.stage(&[recv.as_tagged(heap).erase()]);
                match HostCtx::enter(vm, heap, state, getter, args, None) {
                    Ok(v) => scope.handle(v),
                    Err(e) => return raise_runtime(vm, heap, state, e),
                }
            }
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        out.as_tagged(heap).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadNamedProperty)]
pub fn slow_named_load(frame: Frame) -> Acc {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<1>();
    let fb_slot = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let heap = frame.ctx().heap_mut();
        let name = scope.handle(
            state
                .stack()
                .callable(&*heap, base)
                .constant_slot_name(&*heap, name_idx),
        );
        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::get(vm, heap, state, recv, recv, name.erase()) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(v)) => v,
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        let out = match recv.as_tagged(heap).load_outcome(heap, name.as_tagged(heap)) {
            Ok(LoadOutcome::Value(v)) => {
                let v = scope.handle(v);
                InlineCache::update_load(
                    heap,
                    &scope,
                    state
                        .stack()
                        .frame_header(base)
                        .feedback(&*heap)
                        .map(|v| scope.handle(v)),
                    fb_slot,
                    recv.as_tagged(heap).as_heap_object().map(|o| scope.handle(o)),
                    scope.handle(name.as_tagged(heap).erase().as_name()),
                    true,
                );
                v
            }
            Ok(LoadOutcome::Getter(getter)) => {
                let getter = scope.handle(getter);
                let args = scope.stage(&[recv.as_tagged(heap).erase()]);
                match HostCtx::enter(vm, heap, state, getter, args, None) {
                    Ok(v) => scope.handle(v),
                    Err(e) => return raise_runtime(vm, heap, state, e),
                }
            }
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        out.as_tagged(heap).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(LoadGlobal)]
pub fn slow_global_load(frame: Frame) -> Acc {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let name_idx = frame.unsigned::<0>();
    let fb_slot = frame.unsigned::<1>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let heap = frame.ctx().heap_mut();
        let global = roots.known(heap).global_object;
        let name = scope.handle(
            state
                .stack()
                .frame_header(base)
                .constants(&*heap)
                .at(&*heap, name_idx)
                .erase()
                .as_name(),
        );
        let out = match global.as_tagged(heap).lookup(heap, name.as_tagged(heap)) {
            Lookup::Data { slot, .. } => {
                let v = scope.handle(slot.get(heap));
                InlineCache::update_load(
                    heap,
                    &scope,
                    state
                        .stack()
                        .frame_header(base)
                        .feedback(&*heap)
                        .map(|v| scope.handle(v)),
                    fb_slot,
                    Some(scope.handle(global.as_tagged(heap))),
                    scope.handle(name.as_tagged(heap).erase().as_name()),
                    false,
                );
                v
            }
            Lookup::Accessor { pair, .. } => {
                let getter = scope.handle(pair.get.get(heap));
                if getter.as_tagged(heap) == roots.known(heap).undefined.as_tagged(heap) {
                    // a setter-only accessor: the read yields undefined
                    return roots.known(heap).undefined.as_tagged(heap).erase();
                }
                let args = scope.stage(&[global.as_tagged(heap).erase()]);
                match HostCtx::enter(vm, heap, state, getter, args, None) {
                    Ok(v) => scope.handle(v),
                    Err(e) => return raise_runtime(vm, heap, state, e),
                }
            }
            Lookup::NotFound => {
                // unresolvable reference: GetValue throws a
                // ReferenceError naming the binding
                let text = name
                    .as_tagged(heap)
                    .erase()
                    .get_as::<DenseString>(heap)
                    .map(|s| s.to_rust_string(heap))
                    .unwrap_or_default();
                let ex = Errors::not_defined(vm, heap, state, &text)
                    .expect("error materialization must not fail");
                state.set_pending_exception(ex);
                return roots.known(heap).exception.as_tagged(heap).erase();
            }
        };
        out.as_tagged(heap).erase()
    });
    frame.set_acc(v);
    reenter!(pc)
}

// --- stores ---

#[handler(StoreKeyedProperty)]
pub fn slow_keyed_store(frame: Frame) -> Acc {
    let pc = frame.pc();
    let base = frame.ctx().frame_base();
    let fb = frame.unsigned::<2>();
    let vm = frame.ctx().vm();
    let state = frame.ctx().state();
    let v = state.handle_scope(|scope| {
        let recv = scope.handle(frame.regs().get(frame.signed::<0>(), frame.ctx().heap()));
        let raw_key = scope.handle(frame.regs().get(frame.signed::<1>(), frame.ctx().heap()));
        let value = scope.handle(frame.acc());
        let heap = frame.ctx().heap_mut();
        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::set(vm, heap, state, recv, raw_key, value, recv) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(_)) => value.as_tagged(heap).erase(),
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        let key = match Object::to_property_key(vm, heap, state, raw_key) {
            Ok(Some(k)) => scope.handle(k),
            Ok(None) => return roots.known(heap).exception.as_tagged(heap).erase(),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let name: Handle<'_, SlotName> = match key.as_tagged(heap).erase().classify_key(heap) {
            Ok(Key::Element(i)) => {
                // sparse arrays: the stored entry's details dispatch
                // (accessors call setters, read-only data ignores)
                let dict_mode = recv
                    .as_tagged(heap)
                    .as_heap_object()
                    .is_some_and(|h| h.element_dictionary(heap).is_some());
                let dict_fallthrough: Option<Handle<'_, SlotName>> = if dict_mode {
                    let entry_class = recv
                        .as_tagged(heap)
                        .as_heap_object()
                        .and_then(|h| h.element_dictionary(heap))
                        .map(|d| d.classify(heap, i));
                    match entry_class {
                        Some(EntryClass::Accessor) => {
                            let pair = recv
                                .as_tagged(heap)
                                .as_heap_object()
                                .and_then(|h| h.element_dictionary(heap))
                                .and_then(|d| d.find(heap, i).map(|e| d.value_at(heap, e)))
                                .and_then(|v| v.get_as::<AccessorPair>(heap));
                            let Some(pair) = pair else {
                                return roots.known(heap).exception.as_tagged(heap).erase();
                            };
                            let setter = pair.set.get(heap);
                            if setter == roots.known(heap).undefined.as_tagged(heap) {
                                return value.as_tagged(heap).erase();
                            }
                            let setter = scope.handle(setter);
                            return match apply_store_outcome_body(
                                vm,
                                heap,
                                state,
                                roots,
                                recv,
                                StoreOutcome::CallSetter { setter },
                                value,
                            ) {
                                Ok(()) => value.as_tagged(heap).erase(),
                                Err(()) => roots.known(heap).exception.as_tagged(heap).erase(),
                            };
                        }
                        Some(EntryClass::ReadOnly) => {
                            // sloppy-mode assignment silently ignores
                            return value.as_tagged(heap).erase();
                        }
                        Some(EntryClass::Data) => {
                            let obj = scope
                                .cast::<Object>(heap, recv.as_tagged(heap))
                                .expect("array receiver is an object");
                            if let Err(e) =
                                Object::store_array_element(heap, &scope, &obj, i, &value)
                            {
                                return raise_runtime(vm, heap, state, e);
                            }
                            return value.as_tagged(heap).erase();
                        }
                        _ => {
                            // absent: creating a property consults the
                            // chain (prototype setters); clean chains
                            // store directly, dirty ones take the
                            // ordinary named path below
                            let Some(h) = recv.as_tagged(heap).as_heap_object() else {
                                unreachable!("classified above")
                            };
                            let map = scope.handle(h.map(heap));
                            if Prototype::indexed_store_chain_ok(heap, &scope, &recv, &map, i) {
                                let obj = scope
                                    .cast::<Object>(heap, recv.as_tagged(heap))
                                    .expect("array receiver is an object");
                                if let Err(e) =
                                    Object::store_array_element(heap, &scope, &obj, i, &value)
                                {
                                    return raise_runtime(vm, heap, state, e);
                                }
                                return value.as_tagged(heap).erase();
                            }
                            Some(scope.handle(Tagged::<SlotName>::from(Smi::new(i as i64))))
                        }
                    }
                } else {
                    None
                };
                if let Some(dict_name) = dict_fallthrough {
                    dict_name
                } else {
                    let mut direct = false;
                    if let Some(hobj) = recv.as_tagged(heap).as_heap_object()
                        && hobj.is_array(heap)
                    {
                        let map = hobj.map(heap);
                        if map
                            .published_validity_cell(heap)
                            .is_some_and(|cell| cell.is_valid(heap))
                        {
                            direct = true;
                        } else {
                            let map = scope.handle(map);
                            direct = Prototype::indexed_store_chain_ok(
                                heap, &scope, &recv, &map, i,
                            ) || !recv.as_tagged(heap).chain_holds_index_name(heap, i);
                        }
                    }
                    if !direct {
                        scope.handle(Tagged::<SlotName>::from(Smi::new(i as i64)))
                    } else {
                        let obj = scope
                            .cast::<Object>(heap, recv.as_tagged(heap))
                            .expect("array receiver is an object");
                        let grew = i >= obj.as_tagged(heap).length();
                        if let Err(e) = Object::store_array_element(heap, &scope, &obj, i, &value)
                        {
                            return raise_runtime(vm, heap, state, e);
                        }
                        if let Some(fbv) = state.stack().frame_header(base).feedback(&*heap) {
                            InlineCache::update_store_element(
                                heap,
                                &scope,
                                Some(scope.handle(fbv)),
                                fb,
                                Some(recv),
                                grew,
                            );
                        }
                        return value.as_tagged(heap).erase();
                    }
                }
            }
            Ok(Key::Name(key)) => scope.handle(key),
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        let outcome = match recv.as_tagged(heap).erase().store_lookup(
            heap,
            &scope,
            name.as_tagged(heap),
            value.as_tagged(heap),
            StoreSemantics::Shadow,
        ) {
            Ok(o) => o,
            Err(e) => return raise_runtime(vm, heap, state, e),
        };
        match apply_store_outcome_body(vm, heap, state, roots, recv, outcome, value) {
            Ok(()) => value.as_tagged(heap).erase(),
            Err(()) => roots.known(heap).exception.as_tagged(heap).erase(),
        }
    });
    frame.set_acc(v);
    reenter!(pc)
}

#[handler(StoreNamedProperty)]
pub fn slow_store_named(frame: Frame) -> Acc {
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
                .constants(&*heap)
                .at(&*heap, name_idx)
                .erase()
                .as_name(),
        );

        if Proxy::is_proxy(heap, recv.as_tagged(heap)) {
            return match Proxy::set(vm, heap, state, recv, name.erase(), value, recv) {
                Ok(Coercion::Threw) => roots.known(heap).exception.as_tagged(heap).erase(),
                Ok(Coercion::Value(_)) => value.as_tagged(heap).erase(),
                Err(e) => raise_runtime(vm, heap, state, e),
            };
        }
        if let Some(hit) = InlineCache::try_store(
            heap,
            &scope,
            state
                .stack()
                .frame_header(base)
                .feedback(&*heap)
                .map(|v| scope.handle(v)),
            fb_slot,
            recv.erase(),
            name,
            value,
        ) {
            return match hit {
                StoreHit::Done => value.as_tagged(heap).erase(),
                StoreHit::Setter(setter) => {
                    let setter = scope.handle(setter);
                    let args = scope.stage(&[recv.as_tagged(heap).erase(), value.as_tagged(heap)]);
                    // the setter's return value is ignored (store result
                    // semantics: the accumulator keeps the stored value)
                    match HostCtx::enter(vm, heap, state, setter, args, None) {
                        Ok(v) => scope.handle(v).as_tagged(heap),
                        Err(e) => return raise_runtime(vm, heap, state, e),
                    }
                }
            };
        }
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
        )
    });
    frame.set_acc(v);
    reenter!(pc)
}
