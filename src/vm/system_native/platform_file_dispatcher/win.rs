use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::set_pending_io_exception;
use crate::vm::helper::get_handle;
use crate::vm::system_native::platform_native_dispatcher::windows_helpers::{
    blocking_win32, get_last_error,
};
use std::mem::zeroed;
use std::ptr::null_mut;
use winapi::shared::minwindef::{DWORD, FALSE, LPCVOID, LPVOID};
use winapi::shared::winerror::{ERROR_BROKEN_PIPE, ERROR_NO_DATA};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::fileapi::{
    GetFileSizeEx, ReadFile, SetFileInformationByHandle, SetFilePointerEx, WriteFile,
    FILE_END_OF_FILE_INFO,
};
use winapi::um::handleapi::{DuplicateHandle, INVALID_HANDLE_VALUE};
use winapi::um::minwinbase::{FileEndOfFileInfo, OVERLAPPED};
use winapi::um::processthreadsapi::GetCurrentProcess;
use winapi::um::winbase::{FILE_BEGIN, FILE_CURRENT};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, LARGE_INTEGER};

const IOS_EOF: i32 = -1; // End of file
const IOS_UNAVAILABLE: i32 = -2; // Nothing available (non-blocking)

/// `sun.nio.ch.FileDispatcherImpl.maxDirectTransferSize0()I`
pub(crate) fn max_direct_transfer_size0() -> Result<i32> {
    Ok(i32::MAX - 1) // Integer.MAX_VALUE - 1 is the maximum transfer size for TransmitFile()
}

/// `sun.nio.ch.FileDispatcherImpl.write0(Ljava/io/FileDescriptor;JIZ)I`
pub(crate) fn write0(fd_ref: i32, address: i64, len: i32, append: bool) -> Result<i32> {
    let handle = get_handle(fd_ref)?;
    let handle = handle as usize as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        set_pending_io_exception("Invalid handle")?;
        return Ok(0);
    }

    let mut ov: OVERLAPPED = unsafe { zeroed() };
    let mut written = 0 as DWORD;
    let lp_ov = if append {
        unsafe { ov.u.s_mut().Offset = 0xFFFFFFFF };
        unsafe { ov.u.s_mut().OffsetHigh = 0xFFFFFFFF };
        &mut ov
    } else {
        null_mut()
    };
    // Can block on a pipe or console; only native memory is touched while Safe
    let lp_ov = lp_ov as usize;
    let result = blocking_win32(|| unsafe {
        WriteFile(
            handle,                   /* File handle to write */
            address as LPCVOID,       /* pointer to the buffer */
            len as DWORD,             /* number of bytes to write */
            &mut written,             /* receives number of bytes written */
            lp_ov as *mut OVERLAPPED, /* overlapped struct */
        )
    });

    if result == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Write failed: {}", error_msg))?;
        return Ok(0);
    }

    Ok(written as i32)
}

/// `sun.nio.ch.FileDispatcherImpl.read0(Ljava/io/FileDescriptor;JI)I`
pub(crate) fn read0(fd_ref: i32, address: i64, len: i32) -> Result<i32> {
    let handle = get_handle(fd_ref)? as usize as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        set_pending_io_exception("Invalid handle")?;
        return Ok(0);
    }

    let mut read: DWORD = 0;
    // Can block on a pipe or console; only native memory is touched while Safe
    let result = blocking_win32(|| unsafe {
        ReadFile(
            handle,
            address as LPVOID,
            len as DWORD,
            &mut read,
            0 as *mut OVERLAPPED,
        )
    });
    if result == 0 {
        let error_code = unsafe { GetLastError() };
        if error_code == ERROR_BROKEN_PIPE {
            return Ok(IOS_EOF);
        }
        if error_code == ERROR_NO_DATA {
            return Ok(IOS_UNAVAILABLE);
        }
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Read failed: {}", error_msg))?;
        return Ok(0);
    }

    Ok(if read == 0 { IOS_EOF } else { read as i32 })
}

/// `sun.nio.ch.FileDispatcherImpl.pread0(Ljava/io/FileDescriptor;JIJ)I`
pub(crate) fn pread0(fd_ref: i32, address: i64, len: i32, offset: i64) -> Result<i32> {
    let handle = get_handle(fd_ref)? as usize as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        set_pending_io_exception("Invalid handle")?;
        return Ok(0);
    }

    let mut curr_pos: LARGE_INTEGER = unsafe { zeroed() };
    let mut ov: OVERLAPPED = unsafe { zeroed() };

    let ret = unsafe { SetFilePointerEx(handle, curr_pos, &mut curr_pos, FILE_CURRENT) };
    if ret == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Seek failed: {}", error_msg))?;
        return Ok(0);
    }

    unsafe {
        ov.u.s_mut().Offset = offset as DWORD;
        ov.u.s_mut().OffsetHigh = (offset >> 32) as DWORD;
    }

    let mut read: DWORD = 0;
    let ret = blocking_win32(|| unsafe {
        ReadFile(handle, address as LPVOID, len as DWORD, &mut read, &mut ov)
    });

    if ret == 0 {
        let error_code = unsafe { GetLastError() };
        if error_code == ERROR_BROKEN_PIPE {
            return Ok(IOS_EOF);
        }
        if error_code == ERROR_NO_DATA {
            return Ok(IOS_UNAVAILABLE);
        }
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Read failed: {}", error_msg))?;
        return Ok(0);
    }

    let ret = unsafe { SetFilePointerEx(handle, curr_pos, null_mut(), FILE_BEGIN) };
    if ret == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Seek failed: {}", error_msg))?;
        return Ok(0);
    }

    Ok(if read == 0 { IOS_EOF } else { read as i32 })
}

/// `sun.nio.ch.FileDispatcherImpl.size0(Ljava/io/FileDescriptor;)J`
pub(crate) fn size0(fd_ref: i32) -> Result<i64> {
    let handle = get_handle(fd_ref)? as usize as HANDLE;
    let mut size: LARGE_INTEGER = unsafe { zeroed() };

    let result = unsafe { GetFileSizeEx(handle, &mut size) };
    if result == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("GetFileSizeEx failed: {}", error_msg))?;
        return Ok(0);
    }
    let size = unsafe { *size.QuadPart() };
    Ok(size)
}

/// `sun.nio.ch.FileDispatcherImpl.truncate0(Ljava/io/FileDescriptor;J)I`
pub(crate) fn truncate0(fd_ref: i32, size: i64) -> Result<i32> {
    let handle = (get_handle(fd_ref))? as usize as HANDLE;
    let mut eof_info: FILE_END_OF_FILE_INFO = unsafe { zeroed() };
    unsafe {
        *eof_info.EndOfFile.QuadPart_mut() = size;
    }

    let result = unsafe {
        SetFileInformationByHandle(
            handle,
            FileEndOfFileInfo,
            &mut eof_info as *mut _ as LPVOID,
            size_of::<FILE_END_OF_FILE_INFO>() as DWORD,
        )
    };
    if result == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("Truncation failed: {}", error_msg))?;
        return Ok(0);
    }

    Ok(0)
}

/// `sun.nio.ch.FileDispatcherImpl.duplicateHandle0(J)J`
pub(crate) fn duplicate_handle(fd: i64) -> Result<i64> {
    let handle = fd as usize as HANDLE;
    if handle == INVALID_HANDLE_VALUE {
        set_pending_io_exception("Invalid handle")?;
        return Ok(0);
    }

    let h_process = unsafe { GetCurrentProcess() };
    let mut h_result: HANDLE = null_mut();
    let ret = unsafe {
        DuplicateHandle(
            h_process,
            handle,
            h_process,
            &mut h_result,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ret == 0 {
        let error_msg = get_last_error()?;
        set_pending_io_exception(&format!("DuplicateHandle failed: {}", error_msg))?;
        return Ok(0);
    }

    Ok(h_result as usize as i64)
}

/// `sun.nio.ch.FileDispatcherImpl.available0(Ljava/io/FileDescriptor;)I`
///
/// The bytes between the position and the end of a regular file; zero for other handle types.
pub(crate) fn available0(fd_ref: i32) -> Result<i32> {
    use winapi::um::fileapi::GetFileType;
    use winapi::um::winbase::FILE_TYPE_DISK;

    let handle = get_handle(fd_ref)? as usize as HANDLE;
    if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
        return Ok(0);
    }
    let mut position: LARGE_INTEGER = unsafe { zeroed() };
    if unsafe { SetFilePointerEx(handle, zeroed(), &mut position, FILE_CURRENT) } == 0 {
        set_pending_io_exception(&format!("Available failed: {}", get_last_error()?))?;
        return Ok(0);
    }
    let mut size: LARGE_INTEGER = unsafe { zeroed() };
    if unsafe { GetFileSizeEx(handle, &mut size) } == 0 {
        set_pending_io_exception(&format!("Available failed: {}", get_last_error()?))?;
        return Ok(0);
    }
    let available = unsafe { *size.QuadPart() - *position.QuadPart() };
    Ok(available.clamp(0, i32::MAX as i64) as i32)
}

/// `sun.nio.ch.FileDispatcherImpl.pwrite0(Ljava/io/FileDescriptor;JIJ)I`
pub(crate) fn pwrite0(fd_ref: i32, address: i64, len: i32, position: i64) -> Result<i32> {
    let handle = get_handle(fd_ref)? as usize as HANDLE;
    let mut saved: LARGE_INTEGER = unsafe { zeroed() };
    if unsafe { SetFilePointerEx(handle, zeroed(), &mut saved, FILE_CURRENT) } == 0 {
        set_pending_io_exception(&format!("Seek failed: {}", get_last_error()?))?;
        return Ok(0);
    }
    let mut ov: OVERLAPPED = unsafe { zeroed() };
    unsafe {
        ov.u.s_mut().Offset = position as DWORD;
        ov.u.s_mut().OffsetHigh = (position >> 32) as DWORD;
    }
    let mut written: DWORD = 0;
    let (handle_address, ov_address) = (handle as usize, &mut ov as *mut OVERLAPPED as usize);
    let result = blocking_win32(|| unsafe {
        WriteFile(
            handle_address as HANDLE,
            address as LPCVOID,
            len as DWORD,
            &mut written,
            ov_address as *mut OVERLAPPED,
        )
    });
    if result == 0 {
        set_pending_io_exception(&format!("Write failed: {}", get_last_error()?))?;
        return Ok(0);
    }
    if unsafe { SetFilePointerEx(handle, saved, null_mut(), FILE_BEGIN) } == 0 {
        set_pending_io_exception(&format!("Seek failed: {}", get_last_error()?))?;
        return Ok(0);
    }
    Ok(written as i32)
}

/// An `iovec` as `sun.nio.ch.IOVecWrapper` lays it out: base address, then length.
#[repr(C)]
struct IoVec {
    base: usize,
    len: usize,
}

fn io_vectors<'a>(address: i64, count: i32) -> &'a [IoVec] {
    if count <= 0 || address == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(address as usize as *const IoVec, count as usize) }
}

/// `sun.nio.ch.FileDispatcherImpl.readv0(Ljava/io/FileDescriptor;JI)J`
pub(crate) fn readv0(fd_ref: i32, address: i64, count: i32) -> Result<i64> {
    let mut total = 0i64;
    for vector in io_vectors(address, count) {
        if vector.len == 0 {
            continue;
        }
        let len = vector.len.min(i32::MAX as usize) as i32;
        let read = read0(fd_ref, vector.base as i64, len)?;
        if read < 0 {
            return Ok(if total > 0 { total } else { read as i64 });
        }
        total += read as i64;
        if read < len {
            break;
        }
    }
    Ok(total)
}

/// `sun.nio.ch.FileDispatcherImpl.writev0(Ljava/io/FileDescriptor;JIZ)J`
pub(crate) fn writev0(fd_ref: i32, address: i64, count: i32, append: bool) -> Result<i64> {
    let mut total = 0i64;
    for vector in io_vectors(address, count) {
        if vector.len == 0 {
            continue;
        }
        let len = vector.len.min(i32::MAX as usize) as i32;
        let written = write0(fd_ref, vector.base as i64, len, append)?;
        if written < 0 {
            return Ok(if total > 0 { total } else { written as i64 });
        }
        total += written as i64;
        if written < len {
            break;
        }
    }
    Ok(total)
}

/// `sun.nio.ch.FileDispatcherImpl.force0(Ljava/io/FileDescriptor;Z)I`
pub(crate) fn force0(fd_ref: i32, _metadata: bool) -> Result<i32> {
    use winapi::um::fileapi::FlushFileBuffers;
    let handle_address = get_handle(fd_ref)? as usize;
    if blocking_win32(|| unsafe { FlushFileBuffers(handle_address as HANDLE) }) == 0 {
        set_pending_io_exception(&format!("Force failed: {}", get_last_error()?))?;
        return Ok(-1);
    }
    Ok(0)
}

/// `sun.nio.ch.FileDispatcherImpl` lock results.
const NO_LOCK: i32 = -1;
const LOCKED: i32 = 0;

/// `sun.nio.ch.FileDispatcherImpl.lock0(Ljava/io/FileDescriptor;ZJJZ)I`
pub(crate) fn lock0(fd_ref: i32, block: bool, pos: i64, size: i64, shared: bool) -> Result<i32> {
    use winapi::shared::winerror::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION};
    use winapi::um::fileapi::LockFileEx;
    use winapi::um::ioapiset::GetOverlappedResult;
    use winapi::um::minwinbase::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY};

    let handle_address = get_handle(fd_ref)? as usize;
    let mut flags = 0;
    if !block {
        flags |= LOCKFILE_FAIL_IMMEDIATELY;
    }
    if !shared {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }
    let mut ov: OVERLAPPED = unsafe { zeroed() };
    unsafe {
        ov.u.s_mut().Offset = pos as DWORD;
        ov.u.s_mut().OffsetHigh = (pos >> 32) as DWORD;
    }
    let ov_address = &mut ov as *mut OVERLAPPED as usize;
    let result = blocking_win32(|| unsafe {
        LockFileEx(
            handle_address as HANDLE,
            flags,
            0,
            size as DWORD,
            (size >> 32) as DWORD,
            ov_address as *mut OVERLAPPED,
        )
    });
    if result != 0 {
        return Ok(LOCKED);
    }
    let mut error = unsafe { GetLastError() };
    if error == ERROR_IO_PENDING {
        let waited = blocking_win32(|| {
            let mut transferred: DWORD = 0;
            unsafe {
                GetOverlappedResult(
                    handle_address as HANDLE,
                    ov_address as *mut OVERLAPPED,
                    &mut transferred,
                    1,
                )
            }
        });
        if waited != 0 {
            return Ok(LOCKED);
        }
        error = unsafe { GetLastError() };
    }
    if error == ERROR_LOCK_VIOLATION && !block {
        return Ok(NO_LOCK);
    }
    set_pending_io_exception(&format!("Lock failed: {}", get_last_error()?))?;
    Ok(NO_LOCK)
}

/// `sun.nio.ch.FileDispatcherImpl.release0(Ljava/io/FileDescriptor;JJ)V`
pub(crate) fn release0(fd_ref: i32, pos: i64, size: i64) -> Result<()> {
    use winapi::shared::winerror::{ERROR_IO_PENDING, ERROR_NOT_LOCKED};
    use winapi::um::fileapi::UnlockFileEx;
    use winapi::um::ioapiset::GetOverlappedResult;

    let handle = get_handle(fd_ref)? as usize as HANDLE;
    let mut ov: OVERLAPPED = unsafe { zeroed() };
    unsafe {
        ov.u.s_mut().Offset = pos as DWORD;
        ov.u.s_mut().OffsetHigh = (pos >> 32) as DWORD;
    }
    let result = unsafe { UnlockFileEx(handle, 0, size as DWORD, (size >> 32) as DWORD, &mut ov) };
    if result == 0 {
        let mut error = unsafe { GetLastError() };
        if error == ERROR_IO_PENDING {
            let mut transferred: DWORD = 0;
            if unsafe { GetOverlappedResult(handle, &mut ov, &mut transferred, 1) } != 0 {
                return Ok(());
            }
            error = unsafe { GetLastError() };
        }
        if error != ERROR_NOT_LOCKED {
            set_pending_io_exception(&format!("Release failed: {}", get_last_error()?))?;
        }
    }
    Ok(())
}
