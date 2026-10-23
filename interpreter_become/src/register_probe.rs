#[derive(Clone, Copy, Debug)]
pub struct HandlerRegs {
    pub ip: u8,
    pub regs: u8,
    pub acc: u8,
    pub ctx: u8,
    pub table: u8,
    pub roots: u8,
    pub float: u8,
}

#[cfg(target_arch = "aarch64")]
mod imp {
    use super::HandlerRegs;

    const IP_SENTINEL: u64 = 0x1111_1111_1111_1111;
    const REGS_SENTINEL: u64 = 0x2222_2222_2222_2222;
    const ACC_SENTINEL: u64 = 0x3333_3333_3333_3333;
    const CTX_SENTINEL: u64 = 0x4444_4444_4444_4444;
    const TABLE_SENTINEL: u64 = 0x5555_5555_5555_5555;
    const ROOTS_SENTINEL: u64 = 0x6666_6666_6666_6666;

    core::arch::global_asm!(
        ".globl {probe}",
        "{probe}:",
        "fmov x16, d0",
        "stp x0, x1, [x16, #0]",
        "stp x2, x3, [x16, #16]",
        "stp x4, x5, [x16, #32]",
        "stp x6, x7, [x16, #48]",
        "stp x8, x9, [x16, #64]",
        "stp x10, x11, [x16, #80]",
        "stp x12, x13, [x16, #96]",
        "stp x14, x15, [x16, #112]",
        "stp x16, x17, [x16, #128]",
        "stp x18, x19, [x16, #144]",
        "stp x20, x21, [x16, #160]",
        "stp x22, x23, [x16, #176]",
        "stp x24, x25, [x16, #192]",
        "stp x26, x27, [x16, #208]",
        "stp x28, x29, [x16, #224]",
        "str x30, [x16, #240]",
        "mov x0, #0",
        "ret",
        probe = sym interpreter_become_abi_probe,
    );

    unsafe extern "rust-preserve-none" {
        fn interpreter_become_abi_probe(
            ip: usize,
            regs: usize,
            acc: u64,
            ctx: usize,
            table: usize,
            roots: usize,
            float: f64,
        ) -> u64;
    }

    pub fn probe_handler_regs() -> HandlerRegs {
        let mut dump = [0u64; 48];
        unsafe {
            interpreter_become_abi_probe(
                IP_SENTINEL as usize,
                REGS_SENTINEL as usize,
                ACC_SENTINEL,
                CTX_SENTINEL as usize,
                TABLE_SENTINEL as usize,
                ROOTS_SENTINEL as usize,
                f64::from_bits(dump.as_mut_ptr() as u64),
            );
        }

        let find = |value| {
            dump.iter()
                .position(|&v| v == value)
                .expect("handler ABI changed") as u8
        };

        HandlerRegs {
            ip: find(IP_SENTINEL),
            regs: find(REGS_SENTINEL),
            acc: find(ACC_SENTINEL),
            ctx: find(CTX_SENTINEL),
            table: find(TABLE_SENTINEL),
            roots: find(ROOTS_SENTINEL),
            float: 0,
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub use imp::probe_handler_regs;

#[cfg(not(target_arch = "aarch64"))]
pub fn probe_handler_regs() -> HandlerRegs {
    panic!("handler ABI probe is only implemented for aarch64");
}
