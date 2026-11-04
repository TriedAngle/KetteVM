fn main() {
    let regs = interpreter::register_probe::probe_handler_regs();
    println!(
        "handler regs: ip=x{} regs=x{} acc=x{} ctx=x{} table=x{} roots=x{} float=d{}",
        regs.ip, regs.regs, regs.acc, regs.ctx, regs.table, regs.roots, regs.float
    );
}
