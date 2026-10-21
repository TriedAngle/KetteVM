use bytecode::Opcode;
use vm_core::ic::{CallHit, CallProbe, CallRecord, InlineCache};
use vm_core::proxy::Proxy;
use vm_core::{
    Args, CallTarget, Callee, Coercion, FeedbackVector, FunctionKind, Handle, HandleScope, Heap,
    HostCtx, Map, NativeIndex, Object, Tagged, ThreadState, Value, VmError, raise_runtime,
};

use crate::{Regs, RootsArg};

use crate::Ctx;

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
    // the pushed frame's code, straight from the callee's info:
    // `push_frame` does not allocate, so it cannot go stale
    let code = callee.info.as_ref().code.get(heap);
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
    let code = callee.info.as_ref().code.get(heap);
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

/// Cache the synthesized-receiver map for a closure (identity-keyed),
/// so subsequent `new` calls can allocate directly.
fn construct_cache_record(
    heap: &mut Heap,
    state: &ThreadState,
    scope: &HandleScope<'_>,
    callee: Handle<'_, Object>,
    obj: Handle<'_, Object>,
) {
    // The first-use cache-table allocation may collect; take it before
    // capturing any raw words that must survive the allocations below.
    state.construct_cache_ensure_table(heap);

    // Safety: fresh synthesized-receiver word; read before any allocation
    // below, and no collection can run until then.
    let map: Tagged<Map> =
        unsafe { Tagged::from_value_unchecked(obj.as_tagged(heap).map(heap).raw()) };
    // find the closure's `.prototype` data-property slot
    let proto_name = heap.known().strings.prototype.as_tagged(heap);
    let closure_map = callee.as_tagged(heap).map(heap);
    let mut found = None;
    for d in closure_map.as_ref().descriptors() {
        if d.name(heap).raw() == proto_name.raw() && !d.flags().is_accessor() {
            found = Some(d.offset());
            break;
        }
    }
    let Some(offset) = found else { return };
    let proto_word = callee
        .as_tagged(heap)
        .as_ref()
        .slot(heap, offset)
        .get(heap)
        .raw();
    if !proto_word.is_strong_ptr() {
        return; // non-object prototype: ES falls back, stays uncached
    }
    let proto: Handle<'_, Value> =
        scope.handle(unsafe { Tagged::from_value_unchecked(proto_word) });

    // Allocation-free from here: read the raw words last, immediately
    // before the (allocation-free) insert.
    let callee_obj: Tagged<Object> =
        unsafe { Tagged::from_value_unchecked(callee.as_tagged(heap).raw()) };
    let proto_word: Tagged<Value> =
        unsafe { Tagged::from_value_unchecked(proto.as_tagged(heap).raw()) };
    state.construct_cache_insert(heap, callee_obj, proto_word, offset, map);
}

// --- construct ---

/// Outcome of a `Construct` fast-path attempt.
pub enum ConstructStart {
    /// a constructor frame was pushed (the receiver cache already points
    /// at it); `op_return` applies the receiver fixup on the way out
    Frame,
    /// a native constructor: call it directly with `new_target` set
    Native(NativeIndex),
    /// receiver synthesis threw; the pending exception is set
    Threw,
    /// not an ordinary function constructor: take the general path
    Slow,
}

/// Synthesize the `this` (or stage the hole for a derived constructor)
/// and stage the `[receiver, args...]` parameter window at the stack top.
fn construct_receiver_args<'a>(
    x: &'a mut Ctx<'_, '_>,
    base: usize,
    args_base: i32,
    count: usize,
    derived: bool,
    regs: Regs,
    callee_reg: i32,
) -> Result<Option<Args>, VmError> {
    let vm = x.vm();
    let state = x.state();
    state.handle_scope(|scope| {
        let heap = x.heap_mut();
        let receiver: Handle<'_, Value> = if derived {
            scope.handle(heap.known().the_hole.as_tagged(heap).erase())
        } else {
            let callee_word = regs.get(callee_reg, heap);
            let Some(callee) = scope.cast::<Object>(heap, callee_word) else {
                return Err(VmError::Type);
            };
            if let Some(map) = state.construct_initial_map(heap, callee.as_tagged(heap)) {
                // identity-keyed initial-map cache hit: allocate directly,
                // with the parser's slot estimate as headroom
                let map = scope.handle(map);
                let expected = callee
                    .as_tagged(heap)
                    .as_ref()
                    .callable_info(heap)
                    .map_or(0, |info| info.as_ref().expected_slots());
                let capacity = if expected == 0 {
                    0
                } else {
                    expected.max(2) + Map::SLACK_MARGIN
                };
                scope.handle(heap.new_object_prealloc(&scope, map, capacity).erase())
            } else {
                let Some(obj) =
                    Object::create_construct_receiver_value(vm, heap, state, callee.erase())?
                else {
                    return Ok(None);
                };
                let obj = scope.handle(obj);
                let obj: Handle<'_, Object> = scope
                    .cast::<Object>(heap, obj.as_tagged(heap))
                    .expect("receiver is an object");
                construct_cache_record(heap, state, &scope, callee, obj);
                scope.handle(obj.as_tagged(heap).erase())
            }
        };
        let staged = state.stack().stage_construct(
            base,
            receiver.as_tagged(heap).erase(),
            args_base,
            count,
        )?;
        Ok(Some(staged))
    })
}

/// The `Construct` fast path: constructible bytecode callees push their
/// frame directly (receiver synthesis cached per closure); native
/// constructors are called with `new_target`; everything else falls back
/// to `cold::construct` in `slow_construct`.
pub fn construct_start<'a>(
    x: &'a mut Ctx<'_, '_>,
    pc: usize,
    size: usize,
    regs: Regs,
    callee_reg: i32,
    args_base: i32,
    count: usize,
) -> Result<ConstructStart, VmError> {
    let callee_word = regs.get(callee_reg, x.heap());
    let heap = x.heap();
    if !callee_word
        .as_heap_object()
        .is_some_and(|o| o.map(heap).kind().is_constructor())
    {
        return Ok(ConstructStart::Slow);
    }
    let kind = match callee_word.call_target(heap) {
        Some(CallTarget::Native(rt)) => return Ok(ConstructStart::Native(rt)),
        Some(CallTarget::Bytecode { kind, .. }) => kind,
        _ => return Ok(ConstructStart::Slow),
    };
    let derived = kind.is_derived_class_constructor();
    if !derived && kind != FunctionKind::Normal && kind != FunctionKind::BaseClassConstructor {
        return Ok(ConstructStart::Slow);
    }
    let base = x.frame_base();
    let Some(staged) =
        construct_receiver_args(x, base, args_base, count, derived, regs, callee_reg)?
    else {
        return Ok(ConstructStart::Threw);
    };
    // The receiver synthesis may have collected: re-read the callee from
    // the rooted register window and walk its target again.
    let callee_word = regs.get(callee_reg, x.heap());
    let Some(CallTarget::Bytecode {
        target,
        info,
        context,
        kind,
        ..
    }) = callee_word.call_target(x.heap())
    else {
        return Ok(ConstructStart::Slow);
    };
    let derived = kind.is_derived_class_constructor();
    if !derived && kind != FunctionKind::Normal && kind != FunctionKind::BaseClassConstructor {
        return Ok(ConstructStart::Slow);
    }
    let code = info.as_ref().code.get(x.heap());
    let frame = x.stack().push_frame(
        x.heap(),
        x.caller_meta(pc + size, pc),
        Callee {
            callable: target.erase(),
            info,
            context: context.erase(),
        },
        callee_word,
        staged,
    )?;
    x.switch_frame_code(frame.base, code);
    Ok(ConstructStart::Frame)
}

/// Invoke a native constructor: `undefined` receiver at the window's
/// element 0, `new_target` rooted over the callee register.
pub fn construct_native<'a>(
    x: &'a mut Ctx<'_, '_>,
    rt: NativeIndex,
    callee_reg: i32,
    args_base: i32,
    count: usize,
) -> Tagged<'a, Value> {
    let f = x.vm().runtime(rt);
    let state = x.state();
    let base = x.frame_base();
    let stack = state.stack();
    let undefined = stack.undefined_word(x.heap());
    let args = match stack.stage_construct(base, undefined, args_base, count) {
        Ok(a) => a,
        Err(err) => {
            let _ = x.raise_tag(err);
            return x.heap().known().exception.as_tagged(x.heap()).erase();
        }
    };
    let saved = stack.top();
    let bump = stack.rooting_top(args);
    if let Some(top) = bump {
        stack.set_top(top);
    }
    let new_target = Some(stack.reg_handle(base, callee_reg));
    let nctx = HostCtx::new(x.vm(), x.heap_mut(), state);
    let v = f(nctx, new_target, args);
    if bump.is_some() {
        stack.set_top(saved);
    }
    v
}
