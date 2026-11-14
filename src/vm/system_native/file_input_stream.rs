use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::{
    set_pending_file_not_found_exception, set_pending_index_out_of_bounds_exception,
    set_pending_io_exception, set_pending_null_pointer_exception,
};
use crate::vm::heap::heap::HEAP;
use crate::vm::safepoint;
use crate::vm::system_native::platform_file::Mode::FileInputStream;
use crate::vm::system_native::platform_file::{get_by_raw_id, length, PlatformFile};
use crate::vm::system_native::string::get_utf8_string_by_ref;
use std::fs::OpenOptions;
use std::io::{Read as IoRead, Seek};

/// `java.io.FileInputStream.initIDs()V`
pub(crate) fn init_ids() -> Result<()> {
    // todo: implement me
    Ok(())
}

/// `java.io.FileInputStream.open0(Ljava/lang/String;)V`
pub(crate) fn open0(obj_ref: i32, file_name_ref: i32) -> Result<()> {
    let file_name = get_utf8_string_by_ref(file_name_ref)?;
    match OpenOptions::new().read(true).open(&file_name) {
        Ok(file) => {
            let metadata = match file.metadata() {
                Ok(m) => m,
                Err(e) => {
                    set_pending_io_exception(&e.to_string())?;
                    return Ok(());
                }
            };
            if metadata.is_dir() {
                set_pending_file_not_found_exception(file_name_ref, "Is a directory")?;
                return Ok(());
            }

            PlatformFile::set_raw_id(obj_ref, file, FileInputStream)
        }
        Err(e) => {
            let message = e.to_string();
            set_pending_file_not_found_exception(file_name_ref, &message)?;
            Ok(())
        }
    }
}

/// `java.io.FileInputStream.length0()J`
pub(crate) fn length0(obj_ref: i32) -> Result<i64> {
    let len = length(obj_ref, FileInputStream)?.unwrap_or(-1);
    Ok(len)
}

/// `java.io.FileInputStream.position0()J`
pub(crate) fn position0(obj_ref: i32) -> Result<i64> {
    let Some(mut file) = get_by_raw_id(obj_ref, FileInputStream)? else {
        return Ok(-1);
    };
    let pos = match file.stream_position() {
        Ok(p) => p,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(-1);
        }
    };
    Ok(pos as i64)
}

/// `java.io.FileInputStream.available0()I`
pub(crate) fn available0(obj_ref: i32) -> Result<i32> {
    let Some(mut file) = get_by_raw_id(obj_ref, FileInputStream)? else {
        return Ok(0);
    };
    // Pipes, sockets and terminals cannot seek: ask the kernel how many bytes are buffered, as
    // HotSpot's `handleAvailable` does with FIONREAD.
    #[cfg(unix)]
    if file.metadata().map(|m| !m.is_file()).unwrap_or(true) {
        use std::os::fd::AsRawFd;
        let mut pending: nix::libc::c_int = 0;
        let rc = unsafe { nix::libc::ioctl(file.as_raw_fd(), nix::libc::FIONREAD, &mut pending) };
        return Ok(if rc == 0 { pending.max(0) } else { 0 });
    }
    let current_pos = match file.stream_position() {
        Ok(p) => p,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(0);
        }
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(0);
        }
    };
    let file_size = metadata.len();
    if file_size < current_pos {
        return Ok(0);
    }
    let available = file_size - current_pos;
    let available = if available > i32::MAX as u64 {
        i32::MAX
    } else {
        available as i32
    };
    Ok(available)
}

/// `java.io.FileInputStream.readBytes([BII)I`
///
/// Checks its arguments in the same order as HotSpot's `readBytes` in `io_util.c`.
pub(crate) fn read_bytes(obj_ref: i32, bytes_ref: i32, off: i32, len: i32) -> Result<i32> {
    if bytes_ref == 0 {
        set_pending_null_pointer_exception()?;
        return Ok(-1);
    }
    if !region_in_bounds(bytes_ref, off, len)? {
        set_pending_index_out_of_bounds_exception()?;
        return Ok(-1);
    }
    if len == 0 {
        return Ok(0);
    }
    let Some(mut file) = get_by_raw_id(obj_ref, FileInputStream)? else {
        return Ok(-1);
    };

    // The read can wait for input indefinitely (a terminal, a pipe), so it runs in the Safe
    // state, into host memory, and the bytes are copied into the array afterwards.
    let mut buffer = vec![0u8; len as usize];
    let read_bytes = match safepoint::blocking(|| file.read(&mut buffer)) {
        Ok(n) => n,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(-1);
        }
    };
    if read_bytes == 0 {
        return Ok(-1);
    }
    let off = off as usize;
    HEAP.get_entire_raw_data_mut(bytes_ref)?[off..off + read_bytes]
        .copy_from_slice(&buffer[..read_bytes]);

    Ok(read_bytes as i32)
}

/// `java.io.FileInputStream.read0()I`
pub(crate) fn read0(obj_ref: i32) -> Result<i32> {
    let Some(mut file) = get_by_raw_id(obj_ref, FileInputStream)? else {
        return Ok(-1);
    };
    let mut buffer = [0u8; 1];
    // Can wait for input indefinitely, see `read_bytes`
    let read_bytes = match safepoint::blocking(|| file.read(&mut buffer)) {
        Ok(n) => n,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(-1);
        }
    };
    if read_bytes == 0 {
        return Ok(-1);
    }
    Ok(buffer[0] as i32)
}

/// Whether `off..off + len` lies within the array, like `outOfBounds` in HotSpot's `io_util.c`.
pub(crate) fn region_in_bounds(array_ref: i32, off: i32, len: i32) -> Result<bool> {
    let array_len = HEAP.get_array_len(array_ref)?;
    Ok(off >= 0 && len >= 0 && array_len - off >= len)
}

/// `java.io.FileInputStream.isRegularFile0(Ljava/io/FileDescriptor;)Z`
pub(crate) fn is_regular_file0(_this: i32, fd_ref: i32) -> Result<bool> {
    let Some(file) = PlatformFile::get_by_fd(fd_ref)? else {
        return Ok(false);
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(e) => {
            set_pending_io_exception(&e.to_string())?;
            return Ok(false);
        }
    };

    Ok(metadata.is_file())
}
