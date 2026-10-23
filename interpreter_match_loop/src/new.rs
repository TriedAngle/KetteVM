//! Sketch of the next match-loop interpreter: one stride-agnostic loop,
//! `step::<1>` for narrow instructions and `step::<2>` for wide ones.
//! Flow-free: hot state stays in locals; only `Ctx` owns interpreter state.

use bytecode::Opcode;

use vm_core::cold::add_body;
use vm_core::interpreter::{base, unwind};
use vm_core::{
    Acc, CallTarget, Callee, Ctx, EntryFn, FrameMeta, Handle, HandleSlice, Heap, HostCtx,
    Interpreter, Object, Ops, Regs, Smi, Tagged, ThreadState, Unwind, VM, Value, VmError,
};

pub struct NewInterpreter;

impl Interpreter for NewInterpreter {
    const EXECUTE: EntryFn = execute;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    Running,
    Return,
    Threw,
}

#[inline(always)]
fn offset(code: *const u8, pc: usize) -> *const u8 {
    unsafe { code.add(pc) }
}

pub fn execute<'a>(
    vm: &VM,
    heap: &'a mut Heap,
    state: &ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'_, Value>>,
) -> Result<Tagged<'a, Value>, VmError> {
    let stack = state.stack();

    let saved_top = stack.top();
    let was_active = state.is_frame_active();
    let outer = was_active.then(|| state.frame_base());

    state.handle_scope(|scope| {
        let result = start(vm, heap, state, callable, args, new_target)?;
        let rooted = scope.handle(result);

        stack.set_top(saved_top);
        if let Some(outer) = outer {
            state.set_frame_base(outer);
        } else {
            state.set_frame_active(false);
        }
        Ok(rooted.as_tagged(heap))
    })
}

fn start<'b>(
    vm: &'b VM,
    heap: &'b mut Heap,
    state: &'b ThreadState,
    callable: Handle<'_, Object>,
    args: HandleSlice<'_>,
    new_target: Option<Handle<'b, Value>>,
) -> Result<Tagged<'b, Value>, VmError> {
    match callable.as_tagged(heap).erase().call_target(heap) {
        None => Err(VmError::Type),
        Some(CallTarget::Proxy(_)) => Err(VmError::Type),
        Some(CallTarget::Native(rt)) => {
            let f = vm.runtime(rt);
            let nctx = HostCtx::new(vm, heap, state);
            Ok(f(nctx, new_target, args.as_args()))
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if new_target.is_none() && kind.is_class_constructor() {
                return Err(VmError::Type);
            }
            let stack = state.stack();
            let new_target_value = match &new_target {
                Some(nt) => nt.as_tagged(heap).erase(),
                None => heap.known().undefined.as_tagged(heap).erase(),
            };
            let frame = stack.push_frame_values(
                heap,
                FrameMeta::ROOT,
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                new_target_value,
                args.as_tagged(),
            )?;
            state.set_frame_base(frame.base);
            state.set_frame_active(true);

            let mut ctx = Ctx::new(vm, state, heap, frame.base);
            let value = dispatch(&mut ctx)?;
            // Safety: `value` came from the rooted accumulator slot and `heap`
            // is still borrowed here.
            Ok(unsafe { Tagged::from_value_unchecked(value) })
        }
    }
}

fn dispatch<'h>(ctx: &mut Ctx<'h, 'h>) -> Result<Value, VmError> {
    let mut ip = ctx.code_ptr();
    let mut regs = unsafe { Regs::new(ctx.regs_ptr()) };
    let mut acc = Acc::set(ctx.heap().known().undefined.as_tagged(ctx.heap()).erase());
    let mut exit = Exit::Running;
    let mut frame_base = ctx.frame_base();

    loop {
        step::<1>(
            ctx,
            &mut ip,
            &mut regs,
            &mut acc,
            &mut exit,
            &mut frame_base,
        );

        match exit {
            Exit::Running => {}
            Exit::Return => return Ok(acc.get(ctx.heap()).raw()),
            Exit::Threw => {
                let fault_pc = ip as usize - ctx.code_ptr() as usize;
                match unwind(ctx, fault_pc) {
                    Unwind::Caught { pc, ex } => {
                        acc = Acc::set(ex);
                        frame_base = ctx.frame_base();
                        regs = unsafe { Regs::new(ctx.regs_ptr()) };
                        exit = Exit::Running;
                        ctx.reload_code();
                        ip = offset(ctx.code_ptr(), pc);
                    }
                    Unwind::Escaped => {
                        return Ok(ctx
                            .heap()
                            .known()
                            .exception
                            .as_tagged(ctx.heap())
                            .erase()
                            .raw());
                    }
                }
            }
        }
    }
}

#[inline(always)]
fn step<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: &mut *const u8,
    regs: &mut Regs,
    acc: &mut Acc,
    exit: &mut Exit,
    frame_base: &mut usize,
) {
    let op = unsafe { Opcode::from_byte_unchecked(*(*ip).add(if STRIDE == 2 { 1 } else { 0 })) };

    match op {
        Opcode::Load => *ip = op_load::<STRIDE>(ctx, *ip, *regs, acc),
        Opcode::Store => *ip = op_store::<STRIDE>(ctx, *ip, *regs, *acc),
        Opcode::Add => *ip = op_add::<STRIDE>(ctx, *ip, *regs, acc, exit),
        Opcode::Wide => step::<2>(ctx, ip, regs, acc, exit, frame_base),
        Opcode::Call => *ip = op_call::<STRIDE>(ctx, *ip, regs, acc, exit, frame_base),
        Opcode::Return => *ip = op_return(ctx, *ip, regs, exit, frame_base),
        _ => unimplemented!("opcode {op:?} is not part of the new-interpreter sketch"),
    }
}

#[inline(always)]
fn op_load<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
) -> *const u8 {
    let ops = Ops::<STRIDE>::new(ip, Opcode::Load);
    let next =
        unsafe { ip.add(base::<STRIDE>() + STRIDE * const { Opcode::Load.operands().len() }) };
    let r = ops.signed::<0>();
    *acc = Acc::set(regs.get(r, ctx.heap()));
    next
}

#[inline(always)]
fn op_store<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: Regs,
    acc: Acc,
) -> *const u8 {
    let ops = Ops::<STRIDE>::new(ip, Opcode::Store);
    let next =
        unsafe { ip.add(base::<STRIDE>() + STRIDE * const { Opcode::Store.operands().len() }) };
    regs.set(ops.signed::<0>(), acc.get(ctx.heap()));
    next
}

#[inline(always)]
fn op_add<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    exit: &mut Exit,
) -> *const u8 {
    let ops = Ops::<STRIDE>::new(ip, Opcode::Add);
    let next =
        unsafe { ip.add(base::<STRIDE>() + STRIDE * const { Opcode::Add.operands().len() }) };
    let pc = ip as usize - ctx.code_ptr() as usize;
    let next_pc = pc + (next as usize - ip as usize);

    let heap = ctx.heap();
    let lhs = regs.get(ops.signed::<0>(), heap);
    if let (Some(a), Some(b)) = (lhs.smi_bits(), acc.get(heap).smi_bits())
        && let Some(sum) = a.checked_add(b)
    {
        *acc = Acc::from_smi_bits(sum);
        return next;
    }

    if let (Some(a), Some(b)) = (lhs.as_number(heap), acc.get(heap).as_number(heap)) {
        let sum = a + b;
        if let Some(s) = Smi::from_f64(sum) {
            *acc = Acc::set(s.into_tagged());
            return next;
        }
        if let Some(v) = ctx.heap_mut().try_new_float(sum) {
            *acc = Acc::set(v);
            return next;
        }
        let v = ctx.heap_mut().new_number(sum);
        *acc = Acc::set(v);
        ctx.reload_code();
        return offset(ctx.code_ptr(), next_pc);
    }

    slow_add::<STRIDE>(ctx, ip, regs, acc, exit, pc, next_pc)
}

#[cold]
fn slow_add<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: Regs,
    acc: &mut Acc,
    exit: &mut Exit,
    pc: usize,
    next_pc: usize,
) -> *const u8 {
    let ops = Ops::<STRIDE>::new(ip, Opcode::Add);
    let vm = ctx.vm();
    let thread = ctx.state();
    let exception = {
        let heap = ctx.heap();
        heap.known().exception.as_tagged(heap).raw()
    };
    let v = thread.handle_scope(|scope| {
        let lhs = scope.handle(regs.get(ops.signed::<0>(), ctx.heap()));
        let rhs = scope.handle(acc.get(ctx.heap()));
        add_body(vm, ctx.heap_mut(), thread, lhs, rhs)
    });
    let threw = v.raw() == exception;
    *acc = Acc::set(v);
    ctx.reload_code();
    if threw {
        *exit = Exit::Threw;
        offset(ctx.code_ptr(), pc)
    } else {
        offset(ctx.code_ptr(), next_pc)
    }
}

#[inline(always)]
fn op_call<'h, const STRIDE: usize>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: &mut Regs,
    acc: &mut Acc,
    exit: &mut Exit,
    frame_base: &mut usize,
) -> *const u8 {
    let ops = Ops::<STRIDE>::new(ip, Opcode::Call);
    let (callee_reg, args_base, count) =
        (ops.signed::<0>(), ops.signed::<1>(), ops.unsigned::<2>());
    let next =
        unsafe { ip.add(base::<STRIDE>() + STRIDE * const { Opcode::Call.operands().len() }) };
    let pc = ip as usize - ctx.code_ptr() as usize;
    let next_pc = pc + (next as usize - ip as usize);

    let heap = ctx.heap();
    let stack = ctx.stack();
    let callee = regs.get(callee_reg, heap);
    let args = stack.window(*frame_base, args_base, count);

    match callee.call_target(heap) {
        None => {
            *acc = Acc::set(ctx.raise_tag(VmError::Type));
            *exit = Exit::Threw;
            ip
        }
        Some(CallTarget::Proxy(_)) => {
            *acc = Acc::set(ctx.raise_tag(VmError::Type));
            *exit = Exit::Threw;
            ip
        }
        Some(CallTarget::Native(rt)) => {
            let exception = heap.known().exception.as_tagged(heap).raw();
            let vm = ctx.vm();
            let thread = ctx.state();
            let f = vm.runtime(rt);
            let nctx = HostCtx::new(vm, ctx.heap_mut(), thread);
            let v = f(nctx, None, args);
            let threw = v.raw() == exception;
            *acc = Acc::set(v);
            ctx.reload_code();
            if threw {
                *exit = Exit::Threw;
                offset(ctx.code_ptr(), pc)
            } else {
                offset(ctx.code_ptr(), next_pc)
            }
        }
        Some(CallTarget::Bytecode {
            target,
            info,
            context,
            kind,
            ..
        }) => {
            if kind.is_class_constructor() {
                *acc = Acc::set(ctx.raise_tag(VmError::Type));
                *exit = Exit::Threw;
                return ip;
            }
            let undefined = heap.known().undefined.as_tagged(heap).erase();
            let code = info.as_ref().code.get(heap).as_ptr();
            let frame = match stack.push_frame(
                heap,
                ctx.caller_meta(next_pc, pc),
                Callee {
                    callable: target.erase(),
                    info,
                    context: context.erase(),
                },
                undefined,
                args,
            ) {
                Ok(frame) => frame,
                Err(err) => {
                    *acc = Acc::set(ctx.raise_tag(err));
                    *exit = Exit::Threw;
                    return ip;
                }
            };
            ctx.switch_frame_code(frame.base, code);
            *frame_base = frame.base;
            *regs = unsafe { Regs::new(ctx.regs_ptr()) };
            *acc = Acc::set(undefined);
            ctx.code_ptr()
        }
    }
}

#[inline(always)]
fn op_return<'h>(
    ctx: &mut Ctx<'h, 'h>,
    ip: *const u8,
    regs: &mut Regs,
    exit: &mut Exit,
    frame_base: &mut usize,
) -> *const u8 {
    if *frame_base == ctx.base_anchor() {
        *exit = Exit::Return;
        return ip;
    }
    let caller = ctx.stack().pop_frame(*frame_base);
    ctx.switch_frame(caller.base);
    *frame_base = caller.base;
    *regs = unsafe { Regs::new(ctx.regs_ptr()) };
    offset(ctx.code_ptr(), caller.pc)
}
