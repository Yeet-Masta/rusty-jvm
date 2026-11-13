use crate::vm::error::Result;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use std::ptr::null_mut;
use winapi::um::errhandlingapi::{GetLastError, SetLastError};
use winapi::um::winbase::{FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM};
use winapi::um::winnt::WCHAR;

/// Runs a Win32 call that can block in the Safe state, so a collection does not wait for it.
///
/// Resuming from the Safe state takes locks, which can overwrite the thread's last-error code,
/// so the code the call left is restored for the caller's `GetLastError`. The call must only
/// touch host or native memory, never the managed heap.
pub(crate) fn blocking_win32<T>(call: impl FnOnce() -> T) -> T {
    let (result, error) = crate::vm::safepoint::blocking(|| {
        let result = call();
        (result, unsafe { GetLastError() })
    });
    unsafe { SetLastError(error) };
    result
}

pub fn get_last_error() -> Result<String> {
    let error_code = unsafe { GetLastError() };
    let mut message = [0 as WCHAR; 256];

    let msg_len = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM,
            null_mut(),
            error_code,
            0,
            message.as_mut_ptr(),
            255,
            null_mut(),
        )
    };

    let message = &message[..msg_len as usize];
    Ok(format!(
        "{} ({})",
        String::from_utf16(&message)?,
        error_code
    ))
}

pub fn strip_string(win_string: &[WCHAR]) -> Result<String> {
    let mut len = 0;
    while win_string[len] != 0 {
        len += 1;
    }
    let slice = &win_string[0..len];
    let stripped = String::from_utf16(&slice)?;
    Ok(stripped)
}

pub fn wchar_to_string_ref(win_string: &[WCHAR]) -> Result<i32> {
    let stripped = strip_string(win_string)?;
    StringPoolHelper::get_string(&stripped)
}
