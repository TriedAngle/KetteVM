use crate::continuation::{Continuation, switch};
use core::ffi::c_void;

#[cfg(feature = "tsan")]
unsafe extern "C" {
    fn __tsan_create_fiber(flags: u32) -> *mut c_void;
    fn __tsan_destroy_fiber(fiber: *mut c_void);
    fn __tsan_switch_to_fiber(fiber: *mut c_void, flags: u32);
    fn __tsan_get_current_fiber() -> *mut c_void;
}

#[cfg(feature = "tsan")]
pub fn create_fiber() -> *mut c_void {
    // Safety: TSan runtime is linked by -Zsanitizer=thread.
    unsafe { __tsan_create_fiber(0) }
}

#[cfg(feature = "tsan")]
pub fn destroy_fiber(fiber: *mut c_void) {
    if !fiber.is_null() {
        // Safety: the task cannot be running while its last state is dropped.
        unsafe { __tsan_destroy_fiber(fiber) };
    }
}

#[cfg(feature = "tsan")]
pub fn current_fiber() -> *mut c_void {
    // Safety: called on the current OS thread.
    unsafe { __tsan_get_current_fiber() }
}

/// Switch TSan's fiber and the machine stack in one uninstrumented step.
///
/// The TSan switch must not be separated from the raw switch by any
/// instrumented function: its `__tsan_func_exit` would run on the target
/// fiber while its `__tsan_func_entry` ran on the source, walking the
/// source fiber's shadow stack below its base.
#[inline(always)]
pub unsafe fn switch_fiber(fiber: *mut c_void, from: *mut Continuation, to: *const Continuation) {
    #[cfg(feature = "tsan")]
    if !fiber.is_null() && fiber != current_fiber() {
        // Safety: `fiber` belongs to a live ThreadState and the raw switch follows immediately.
        unsafe { __tsan_switch_to_fiber(fiber, 0) };
    }
    #[cfg(not(feature = "tsan"))]
    let _ = fiber;
    // Safety: caller upholds the continuation invariants.
    unsafe { switch(from, to) }
}

#[cfg(not(feature = "tsan"))]
pub fn create_fiber() -> *mut c_void {
    core::ptr::null_mut()
}

#[cfg(not(feature = "tsan"))]
pub fn destroy_fiber(_fiber: *mut c_void) {}

#[cfg(not(feature = "tsan"))]
pub fn current_fiber() -> *mut c_void {
    core::ptr::null_mut()
}
