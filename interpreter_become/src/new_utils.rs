//! Call plumbing shared by the `new` handlers: resolving a callee into a
//! [`StagedCall`], pushing bytecode frames from a rooted window or a value
//! slice, and applying proxies over staged windows.

#![allow(dead_code)]

use bytecode::Opcode;
use vm_core::ic::{CallHit, CallProbe, CallRecord, InlineCache};
use vm_core::proxy::Proxy;
use vm_core::{
    Args, CallTarget, Callee, Coercion, FeedbackVector, Heap, NativeIndex, Tagged, Value, VmError,
    raise_runtime,
};

use crate::{Regs, RootsArg};

use crate::new::Ctx;

/// The outcome of a call-site start helper.
pub enum StagedCall<'a> {
    /// the call completed: the value or the exception sentinel
    Value(Tagged<'a, Value>),
    /// a frame was pushed and made current: enter it at pc 0
    Frame,
    /// a rooted argument window for a runtime callee
    Native { rt: NativeIndex, staged: Args },
    /// a proxy apply over a staged window
    Proxy { callee_reg: i32, staged: Args },
}

/// Push a callee frame (facts from a `CallTarget::Bytecode` destructure
/// or a call-IC hit; the caller is the current frame) and switch to it.
/// Entry is at pc 0 with the accumulator seeded undefined.
#[inline(always)]
pub fn push_callee_frame<'a>(
    x: &Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    args: Args,
) -> Result<StagedCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = roots.known(heap).undefined.as_tagged(heap).erase();
    // the pushed frame's pc origin, straight from the callee's info:
    // `push_frame` does not allocate, so it cannot go stale
    let code = callee.info.as_ref().bytecode.get(heap).as_ptr();
    let frame = x.stack().push_frame(heap, meta, callee, undefined, args)?;
    x.switch_frame_code(frame.base, code);
    Ok(StagedCall::Frame)
}

/// Push with parameters as values: stored straight into the callee's
/// parameter slots, no intermediate staging window.
#[inline(always)]
pub fn push_callee_frame_values<'a>(
    x: &Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee: Callee<'_>,
    values: &[Tagged<'_, Value>],
) -> Result<StagedCall<'a>, VmError> {
    let meta = x.caller_meta(pc + size, pc);
    let heap = x.heap();
    let undefined = roots.known(heap).undefined.as_tagged(heap).erase();
    let code = callee.info.as_ref().bytecode.get(heap).as_ptr();
    let frame = x
        .stack()
        .push_frame_values(heap, meta, callee, undefined, values)?;
    x.switch_frame_code(frame.base, code);
    Ok(StagedCall::Frame)
}

/// The IC-miss resolution (cold): walk the callee's target and record
/// the hit; the result feeds `resolve_target` like a probe hit.
#[cold]
#[inline(never)]
pub fn call_target_miss<'a>(
    heap: &'a Heap,
    feedback: Option<Tagged<'a, FeedbackVector>>,
    fb: Option<usize>,
    callee: Tagged<'a, Value>,
) -> Option<CallTarget<'a>> {
    let target = callee.call_target(heap);
    match &target {
        Some(CallTarget::Native(rt)) => {
            if let Some(fb) = fb {
                InlineCache::call_update(heap, feedback, fb, callee, CallRecord::Native(*rt));
            }
        }
        Some(CallTarget::Bytecode { info, .. }) => {
            if let Some(fb) = fb {
                InlineCache::call_update(heap, feedback, fb, callee, CallRecord::Bytecode(*info));
            }
        }
        None | Some(CallTarget::Proxy(_)) => {}
    }
    target
}

/// Resolve a walked target over an already-staged argument window —
/// the cold path (IC miss, `CallNoFeedback`): class constructors and
/// non-callables raise (as `Err`), bytecode pushes through the shared
/// helper, natives and proxies carry their staged window into the
/// exclusive phase. The hot probe arms bypass this and act on the
/// `CallHit` fields directly.
#[inline(always)]
pub fn resolve_target<'a>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    size: usize,
    callee_reg: i32,
    target: Option<CallTarget<'a>>,
    staged: Args,
) -> Result<StagedCall<'a>, VmError> {
    match target {
        None => Err(VmError::Type),
        Some(CallTarget::Proxy(_)) => Ok(StagedCall::Proxy { callee_reg, staged }),
        Some(CallTarget::Native(rt)) => Ok(StagedCall::Native { rt, staged }),
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                staged,
            )
        }
    }
}

/// The scattered call start (`CallMethodN` / `CallFunctionN`):
/// eagerly-read operand values, shared context. `METHOD` selects the
/// method form (and its extra operand); everything up to the rooted
/// or consumed words is allocation-free.
#[inline(always)]
pub fn call_scatter_start<'a, const STRIDE: usize, const METHOD: bool>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    fb: usize,
    callee_reg: i32,
    callee: Tagged<'a, Value>,
    recv: Tagged<'a, Value>,
    args: &[Tagged<'a, Value>],
) -> Result<StagedCall<'a>, VmError> {
    let size = (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * (2 + METHOD as usize + args.len());
    // parameter payload: [receiver, args..]
    let mut window = [recv; 3];
    window[1..1 + args.len()].copy_from_slice(args);
    let values = &window[..1 + args.len()];
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame_values(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                values,
            )
        }
        CallProbe::Native(rt) => Ok(StagedCall::Native {
            rt,
            staged: x.stack().stage_slice(values)?,
        }),
        CallProbe::Miss => resolve_target(
            x,
            roots,
            pc,
            size,
            callee_reg,
            call_target_miss(x.heap(), x.feedback_ref(x.heap()), Some(fb), callee),
            x.stack().stage_slice(values)?,
        ),
    }
}

/// The contiguous-window call start (`Call`): the window lives in the
/// caller's frame (already rooted), so no staging is needed.
#[inline(always)]
pub fn call_window_start<'a, const STRIDE: usize>(
    x: &'a Ctx<'_, '_>,
    roots: RootsArg<'_>,
    pc: usize,
    fb: usize,
    callee_reg: i32,
    callee: Tagged<'a, Value>,
    base: i32,
    count: usize,
) -> Result<StagedCall<'a>, VmError> {
    let size = (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::Call.operands().len() };
    let staged = x.stack().window(x.frame_base(), base, count);
    match InlineCache::call_probe(x.heap(), x.feedback_ref(x.heap()), fb, callee) {
        CallProbe::Bytecode(CallHit {
            target,
            info,
            context,
            kind,
        }) => {
            if kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            push_callee_frame(
                x,
                roots,
                pc,
                size,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                staged,
            )
        }
        CallProbe::Native(rt) => Ok(StagedCall::Native { rt, staged }),
        CallProbe::Miss => resolve_target(
            x,
            roots,
            pc,
            size,
            callee_reg,
            call_target_miss(x.heap(), x.feedback_ref(x.heap()), Some(fb), callee),
            staged,
        ),
    }
}

/// Apply a callable proxy over a staged window: root the window for
/// the call, re-read the callee register, restore the stack top.
#[cold]
#[inline(never)]
pub fn proxy_apply_staged<'a>(
    x: &'a mut Ctx<'_, '_>,
    roots: RootsArg<'_>,
    regs: Regs,
    callee_reg: i32,
    staged: Args,
) -> Tagged<'a, Value> {
    let vm = x.vm();
    let state = x.state();
    let stack = state.stack();
    let saved_top = stack.top();
    if let Some(top) = stack.rooting_top(staged) {
        stack.set_top(top);
    }
    let v = state.handle_scope(|scope| {
        let callee = scope.handle(regs.get(callee_reg, x.heap()));
        match Proxy::apply(vm, x.heap_mut(), state, callee, staged) {
            Ok(Coercion::Threw) => roots.known(x.heap()).exception.as_tagged(x.heap()).erase(),
            Ok(Coercion::Value(v)) => v,
            Err(err) => raise_runtime(vm, x.heap_mut(), state, err),
        }
    });
    stack.set_top(saved_top);
    v
}
