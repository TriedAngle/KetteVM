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

#[cfg(feature = "tsan")]
pub fn switch_to(fiber: *mut c_void) {
    if fiber.is_null() || fiber == current_fiber() {
        return;
    }
    // Safety: `fiber` belongs to a live ThreadState and the raw switch follows.
    unsafe { __tsan_switch_to_fiber(fiber, 0) };
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

#[cfg(not(feature = "tsan"))]
pub fn switch_to(_fiber: *mut c_void) {}
