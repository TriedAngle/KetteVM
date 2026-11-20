//! Stackful continuations: saved registers plus a task stack.

use core::mem::MaybeUninit;

#[cfg(target_arch = "aarch64")]
#[repr(C)]
pub struct SavedRegisters {
    x19: usize,
    x20: usize,
    x21: usize,
    x22: usize,
    x23: usize,
    x24: usize,
    x25: usize,
    x26: usize,
    x27: usize,
    x28: usize,
    x29: usize,
    x30: usize,
    sp: usize,
    d8: u64,
    d9: u64,
    d10: u64,
    d11: u64,
    d12: u64,
    d13: u64,
    d14: u64,
    d15: u64,
}

#[cfg(target_arch = "x86_64")]
#[repr(C)]
pub struct SavedRegisters {
    rbx: u64,
    rbp: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rsp: u64,
    mxcsr: u32,
    _pad: u32,
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("continuation switching is implemented for aarch64 and x86_64");

#[repr(C)]
pub struct Continuation {
    pub regs: SavedRegisters,
    stack_base: *mut u8,
    stack_size: usize,
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".text",
    ".p2align 2",
    ".globl {switch}",
    "{switch}:",
    "stp x19, x20, [x0, #0]",
    "stp x21, x22, [x0, #16]",
    "stp x23, x24, [x0, #32]",
    "stp x25, x26, [x0, #48]",
    "stp x27, x28, [x0, #64]",
    "stp x29, x30, [x0, #80]",
    "mov x2, sp",
    "str x2, [x0, #96]",
    "stp d8, d9, [x0, #104]",
    "stp d10, d11, [x0, #120]",
    "stp d12, d13, [x0, #136]",
    "stp d14, d15, [x0, #152]",
    "ldp x19, x20, [x1, #0]",
    "ldp x21, x22, [x1, #16]",
    "ldp x23, x24, [x1, #32]",
    "ldp x25, x26, [x1, #48]",
    "ldp x27, x28, [x1, #64]",
    "ldp x29, x30, [x1, #80]",
    "ldr x2, [x1, #96]",
    "mov sp, x2",
    "ldp d8, d9, [x1, #104]",
    "ldp d10, d11, [x1, #120]",
    "ldp d12, d13, [x1, #136]",
    "ldp d14, d15, [x1, #152]",
    "ret",
    ".globl {entry}",
    "{entry}:",
    "mov x0, x19",
    "bl {task_entry}",
    "brk #0",
    switch = sym kette_switch,
    entry = sym kette_task_entry_trampoline,
    task_entry = sym kette_task_entry,
);

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".text",
    ".globl {switch}",
    "{switch}:",
    "mov [rdi], rbx",
    "mov [rdi + 8], rbp",
    "mov [rdi + 16], r12",
    "mov [rdi + 24], r13",
    "mov [rdi + 32], r14",
    "mov [rdi + 40], r15",
    "mov [rdi + 48], rsp",
    "stmxcsr [rdi + 56]",
    "mov rbx, [rsi]",
    "mov rbp, [rsi + 8]",
    "mov r12, [rsi + 16]",
    "mov r13, [rsi + 24]",
    "mov r14, [rsi + 32]",
    "mov r15, [rsi + 40]",
    "mov rsp, [rsi + 48]",
    "ldmxcsr [rsi + 56]",
    "ret",
    ".globl {entry}",
    "{entry}:",
    "mov rdi, r12",
    "call {task_entry}",
    "ud2",
    switch = sym kette_switch,
    entry = sym kette_task_entry_trampoline,
    task_entry = sym kette_task_entry,
);

unsafe extern "C" {
    fn kette_switch(from: *mut Continuation, to: *const Continuation);
    fn kette_task_entry_trampoline();
}

// The Rust entry of a new task, defined by the VM.
unsafe extern "C" {
    fn kette_task_entry(arg: *mut ()) -> !;
}

impl Continuation {
    pub fn zeroed() -> Self {
        // Safety: all fields are integers or pointers; zero is valid.
        unsafe { MaybeUninit::zeroed().assume_init() }
    }

    pub fn prepare(&mut self, stack_base: *mut u8, stack_size: usize, arg: *mut ()) {
        self.stack_base = stack_base;
        self.stack_size = stack_size;
        let top = (stack_base as usize + stack_size) & !15;
        #[cfg(target_arch = "aarch64")]
        {
            self.regs.sp = top;
            self.regs.x19 = arg as usize;
            self.regs.x30 = kette_task_entry_trampoline as *const () as usize;
        }
        #[cfg(target_arch = "x86_64")]
        {
            let entry = kette_task_entry_trampoline as *const () as u64;
            let sp = top - 8;
            // Safety: the stack has at least one word and is writable.
            unsafe { (sp as *mut u64).write(entry) };
            self.regs.rsp = sp as u64;
            self.regs.r12 = arg as usize as u64;
            // Safety: stmxcsr writes one u32 to a valid, aligned operand.
            unsafe {
                core::arch::asm!("stmxcsr [{}]", in(reg) &mut self.regs.mxcsr, options(nostack, preserves_flags))
            };
        }
    }

    pub fn stack_base(&self) -> *mut u8 {
        self.stack_base
    }

    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    pub fn reset(&mut self) {
        *self = Self::zeroed();
    }
}

/// # Safety
/// `from` and `to` must be valid continuations; `to` must be suspended or
/// freshly prepared.
pub unsafe fn switch(from: *mut Continuation, to: *const Continuation) {
    // Safety: caller upholds the continuation invariants.
    unsafe { kette_switch(from, to) }
}

/// Reserve a stack with a guard page at its low end.
pub fn allocate_stack(size: usize) -> Option<(*mut u8, usize)> {
    let page = page_size();
    let total = size.checked_add(page)?;
    let ptr = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            total,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON | map_noreserve(),
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return None;
    }
    if unsafe { libc::mprotect(ptr, page, libc::PROT_NONE) } != 0 {
        unsafe { libc::munmap(ptr, total) };
        return None;
    }
    Some((ptr.cast(), total))
}

pub fn free_stack(base: *mut u8, size: usize) {
    if !base.is_null() && size > 0 {
        unsafe { libc::munmap(base.cast(), size) };
    }
}

#[cfg(target_os = "linux")]
fn map_noreserve() -> libc::c_int {
    libc::MAP_NORESERVE
}

#[cfg(not(target_os = "linux"))]
fn map_noreserve() -> libc::c_int {
    0
}

fn page_size() -> usize {
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}
