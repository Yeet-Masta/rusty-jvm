//! `JVM_*` functions that rusty-jvm provides to the JDK's native libraries through the JVM shim
//! (see [`super::jvm_shim`]).
//!
//! JDK native code calls these from native (Safe) state, with JNI handles for references. Most
//! `JVM_*` functions back natives that rusty-jvm implements as built-ins, so the JDK libraries
//! never reach them; the ones here are those their own natives call.

use std::ffi::c_void;

/// `(export name, implementation)` pairs registered with the shim at startup.
pub(crate) fn functions() -> Vec<(&'static str, *mut c_void)> {
    vec![
        ("JVM_MaxMemory", jvm_max_memory as *mut c_void),
        ("JVM_TotalMemory", jvm_total_memory as *mut c_void),
        ("JVM_FreeMemory", jvm_free_memory as *mut c_void),
        (
            "JVM_FindLibraryEntry",
            jvm_find_library_entry as *mut c_void,
        ),
        #[cfg(windows)]
        (
            "JVM_GetThreadInterruptEvent",
            crate::vm::system_native::interrupt_event::jvm_get_thread_interrupt_event
                as *mut c_void,
        ),
    ]
}

extern "C" fn jvm_max_memory() -> i64 {
    crate::vm::system_native::runtime::max_memory(0).unwrap_or(i64::MAX)
}

extern "C" fn jvm_total_memory() -> i64 {
    crate::vm::system_native::runtime::total_memory(0).unwrap_or(0)
}

extern "C" fn jvm_free_memory() -> i64 {
    crate::vm::system_native::runtime::free_memory(0).unwrap_or(0)
}

/// `void* JVM_FindLibraryEntry(void* handle, const char* name)`: a symbol of an already loaded
/// native library (`dlsym` / `GetProcAddress`), null when absent.
extern "C" fn jvm_find_library_entry(
    handle: *mut c_void,
    name: *const std::ffi::c_char,
) -> *mut c_void {
    if handle.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    #[cfg(unix)]
    unsafe {
        nix::libc::dlsym(handle, name)
    }
    #[cfg(windows)]
    unsafe {
        winapi::um::libloaderapi::GetProcAddress(handle as _, name) as *mut c_void
    }
}
