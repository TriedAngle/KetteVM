//! Dynamic loader abstraction.
//!
//! `open`, `symbol`, and `close` are the only places that touch the platform's
//! dynamic loader. Handles are plain addresses; ownership lives in the
//! `ForeignLibrary` object.

use crate::ffi::FfiError;
#[cfg(unix)]
use core::ffi::CStr;
#[cfg(windows)]
use core::ffi::c_char;
use core::ffi::{c_int, c_void};
use core::ptr;

#[cfg(unix)]
const OPEN_FLAGS: c_int = libc::RTLD_NOW | libc::RTLD_LOCAL;

#[cfg(unix)]
fn last_error() -> String {
    // Safety: `dlerror` returns either null or a NUL-terminated string owned
    // by the loader.
    let message = unsafe { libc::dlerror() };
    if message.is_null() {
        "unknown dynamic loader error".to_string()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(windows)]
fn last_error() -> String {
    // Safety: `GetLastError` has no preconditions.
    let code = unsafe { GetLastError() };
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

#[cfg(windows)]
unsafe extern "system" {
    fn LoadLibraryA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
    fn GetLastError() -> u32;
}

fn c_name(text: &str) -> Result<std::ffi::CString, FfiError> {
    std::ffi::CString::new(text)
        .map_err(|_| FfiError::LoadFailed("name contains an interior NUL".to_string()))
}

/// Load a shared library, or the current process when `path` is `None`.
pub fn open(path: Option<&str>) -> Result<usize, FfiError> {
    #[cfg(unix)]
    {
        let name = match path {
            Some(path) => Some(c_name(path)?),
            None => None,
        };
        let pointer = name.as_ref().map_or(ptr::null(), |name| name.as_ptr());
        // Safety: `pointer` is null or a valid NUL-terminated path.
        let handle = unsafe { libc::dlopen(pointer, OPEN_FLAGS) };
        if handle.is_null() {
            return Err(FfiError::LoadFailed(last_error()));
        }
        Ok(handle as usize)
    }

    #[cfg(windows)]
    {
        let handle = match path {
            Some(path) => {
                let name = c_name(path)?;
                // Safety: `name` is NUL-terminated.
                unsafe { LoadLibraryA(name.as_ptr()) }
            }
            // Safety: a null module name asks for the current process; the
            // returned handle is not owned and must not be freed.
            None => unsafe { GetModuleHandleA(ptr::null()) },
        };
        if handle.is_null() {
            return Err(FfiError::LoadFailed(last_error()));
        }
        Ok(handle as usize)
    }
}

/// Resolve `name` in `handle`.
pub fn symbol(handle: usize, name: &str) -> Result<usize, FfiError> {
    let c = c_name(name)?;

    #[cfg(unix)]
    {
        // Clear any pending error so a null result can be attributed.
        // Safety: `dlerror` takes no arguments.
        unsafe { libc::dlerror() };
        // Safety: `handle` comes from `open`; `c` is NUL-terminated.
        let address = unsafe { libc::dlsym(handle as *mut c_void, c.as_ptr()) };
        if address.is_null() {
            let message = last_error();
            return Err(FfiError::SymbolNotFound {
                name: name.to_string(),
                message,
            });
        }
        Ok(address as usize)
    }

    #[cfg(windows)]
    {
        // Safety: `handle` comes from `open`; `c` is NUL-terminated.
        let address = unsafe { GetProcAddress(handle as *mut c_void, c.as_ptr()) };
        if address.is_null() {
            let message = last_error();
            return Err(FfiError::SymbolNotFound {
                name: name.to_string(),
                message,
            });
        }
        Ok(address as usize)
    }
}

/// Release a handle from [`open`]. Process handles are never released.
pub fn close(handle: usize) -> Result<(), FfiError> {
    #[cfg(unix)]
    {
        // Safety: `handle` comes from `dlopen` and is closed once.
        let status = unsafe { libc::dlclose(handle as *mut c_void) };
        if status != 0 {
            return Err(FfiError::LoadFailed(last_error()));
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        // Safety: `handle` comes from `LoadLibraryA` and is freed once.
        let status = unsafe { FreeLibrary(handle as *mut c_void) };
        if status == 0 {
            return Err(FfiError::LoadFailed(last_error()));
        }
        Ok(())
    }
}
