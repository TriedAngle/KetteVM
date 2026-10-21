/// Instruction Pointer
macro_rules! ip {
    ($ip:expr) => {
        $ip
    };
}

/// Insruction Offset
macro_rules! pc {
    ($ip:expr, $x:expr) => {{ $ip as usize - $x.code_ptr() as usize }};
}

/// Register Window of the Frame
macro_rules! regs {
    ($r:expr) => {
        $r
    };
}

/// Accumulator, must be used before reaching safepoints!
macro_rules! acc {
    ($acc:expr, $x:expr) => {{ $acc.get($x.heap()) }};
}

/// Float Register
macro_rules! float {
    ($f:expr) => {{ $f.get() }};
}

/// Set the accumulator from a tagged value.
macro_rules! set_acc {
    ($acc:ident, $v:expr) => {
        $acc = Acc::set($v)
    };
}

/// Set the accumulator from raw smi bits.
macro_rules! set_acc_smi {
    ($acc:ident, $v:expr) => {
        $acc = Acc::from_smi_bits($v)
    };
}

/// Park a float payload in the float register for a slow path.
macro_rules! set_float {
    ($f:ident, $v:expr) => {
        $f = FloatReg::new($v)
    };
}

/// Dispatch to the handler for the instruction at `$p`.
macro_rules! dispatch {
    ($p:expr, $a:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let h = $t.get(unsafe { *$p });
        become h($p, $r, $a, $x, $t, $k, $f)
    }};
}

/// Advance to next instruction and dispatch.
macro_rules! next {
    ($op:ident, $ip:expr, $r:expr, $x:expr, $t:ident, $k:ident, $f:ident, $a:expr) => {{
        let p = $ip.wrapping_add(
            (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() },
        );
        #[cfg(feature = "star_fusion")]
        {
            if star_lookahead(Opcode::$op) && unsafe { *p } == Opcode::Store as u8 {
                let dst = unsafe { *p.add(1) } as i8 as i32;
                $r.set(dst, $a.get($x.heap()));
                dispatch!(unsafe { p.add(2) }, $a, $r, $x, $t, $k, $f)
            } else {
                dispatch!(p, $a, $r, $x, $t, $k, $f)
            }
        }
        #[cfg(not(feature = "star_fusion"))]
        {
            dispatch!(p, $a, $r, $x, $t, $k, $f)
        }
    }};
}

/// next! but rederive the instruction pointer, must be done when having reached safepoints.
macro_rules! reenter {
    ($op:ident, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let pc = $pc;
        $ctx.reload_code();
        let fault_ip = unsafe { $ctx.code_ptr().add(pc) };
        if $acc.get($ctx.heap()).raw().to_bits()
            == $roots
                .known($ctx.heap())
                .exception
                .as_tagged($ctx.heap())
                .erase()
                .raw()
                .to_bits()
        {
            become throw_dispatch::<STRIDE>(fault_ip, $regs, $acc, $ctx, $table, $roots, $float)
        }
        let p = fault_ip.wrapping_add(
            (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() },
        );
        let regs = unsafe { Regs::new($ctx.regs_ptr()) };
        let h = $table.get(unsafe { *p });
        become h(p, regs, $acc, $ctx, $table, $roots, $float)
    }};
}

/// Tailcall into a named handler
macro_rules! tail {
    ($ip:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr, $func:path) => {
        become $func($ip, $regs, $acc, $ctx, $table, $roots, $float)
    };
}

/// Dispatch into the caller after a frame pop
macro_rules! dispatch_frame {
    ($pc:expr, $a:expr, $x:expr, $t:ident, $k:ident, $f:ident) => {{
        let regs = unsafe { Regs::new($x.regs_ptr()) };
        let p = unsafe { $x.code_ptr().add($pc) };
        let h = $t.get(unsafe { *p });
        become h(p, regs, $a, $x, $t, $k, $f)
    }};
}

/// call a callable
macro_rules! call_enter {
    ($op:ident, $result:expr, $pc:expr, $regs:expr, $acc:expr, $ctx:expr, $table:expr, $roots:expr, $float:expr) => {{
        let (acc, value) = match $result {
            Ok(StagedCall::Frame) => ($acc, false),
            Ok(StagedCall::Value(v)) => (Acc::set(v), true),
            Ok(StagedCall::Native { rt, staged }) => {
                (Acc::set($ctx.call_runtime(rt, staged)), true)
            }
            Ok(StagedCall::Proxy { callee_reg, staged }) => {
                (Acc::set(proxy_apply_staged($ctx, $roots, $regs, callee_reg, staged)), true)
            }
            Err(err) => (Acc::set($ctx.raise_tag(err)), true),
        };
        if value {
            $ctx.reload_code();
            let fault_ip = unsafe { $ctx.code_ptr().add($pc) };
            if acc.get($ctx.heap()).raw().to_bits()
                == $roots
                    .known($ctx.heap())
                    .exception
                    .as_tagged($ctx.heap())
                    .erase()
                    .raw()
                    .to_bits()
            {
                become throw_dispatch::<STRIDE>(fault_ip, $regs, acc, $ctx, $table, $roots, $float)
            }
            let size =
                (if STRIDE == 2 { 2 } else { 1 }) + STRIDE * const { Opcode::$op.operands().len() };
            let p = unsafe { fault_ip.add(size) };
            let regs = unsafe { Regs::new($ctx.regs_ptr()) };
            let h = $table.get(unsafe { *p });
            become h(p, regs, acc, $ctx, $table, $roots, $float)
        }
        let regs = unsafe { Regs::new($ctx.regs_ptr()) };
        let acc = Acc::set(
            $roots
                .known($ctx.heap())
                .undefined
                .as_tagged($ctx.heap())
                .erase(),
        );
        let entry = $ctx.code_ptr();
        let h = $table.get(unsafe { *entry });
        become h(entry, regs, acc, $ctx, $table, $roots, $float)
    }};
}

pub(crate) use {
    acc, call_enter, dispatch, dispatch_frame, float, ip, next, pc, reenter, regs, set_acc,
    set_acc_smi, set_float, tail,
};
