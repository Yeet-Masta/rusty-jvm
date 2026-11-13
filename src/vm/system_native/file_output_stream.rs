use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::{
    set_pending_file_not_found_exception, set_pending_index_out_of_bounds_exception,
    set_pending_io_exception, set_pending_null_pointer_exception,
};
use crate::vm::heap::heap::HEAP;
use crate::vm::safepoint;
use crate::vm::system_native::file_input_stream::region_in_bounds;
use crate::vm::system_native::platform_file::Mode::FileOutputStream;
use crate::vm::system_native::platform_file::{get_by_raw_id, PlatformFile};
use crate::vm::system_native::string::get_utf8_string_by_ref;
use std::fs::OpenOptions;
use std::io::Write as IoWrite;

/// `java.io.FileOutputStream.initIDs()V`
pub(crate) fn init_ids() -> Result<()> {
    // todo: implement me
    Ok(())
}

/// `java.io.FileOutputStream.open0(Ljava/lang/String;Z)V`
pub(crate) fn open0(obj_ref: i32, file_name_ref: i32, append: bool) -> Result<()> {
    let file_name = get_utf8_string_by_ref(file_name_ref)?;

    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(!append)
        .append(append)
        .open(file_name)
    {
        Ok(file) => {
            PlatformFile::set_raw_id(obj_ref, file, FileOutputStream)?;
            Ok(())
        }
        Err(e) => {
            set_pending_file_not_found_exception(file_name_ref, &e.to_string())?;
            Ok(())
        }
    }
}

/// `java.io.FileOutputStream.write(IZ)V`
pub(crate) fn write(obj_ref: i32, byte: i32, _append: bool) -> Result<()> {
    let Some(mut file) = get_by_raw_id(obj_ref, FileOutputStream)? else {
        return Ok(());
    };

    // A write to a full pipe or a stopped terminal can block indefinitely
    if let Err(e) = safepoint::blocking(|| file.write_all(&[byte as u8])) {
        set_pending_io_exception(&e.to_string())?;
    }

    Ok(())
}

/// `java.io.FileOutputStream.writeBytes([BIIZ)V`
pub(crate) fn write_bytes(
    obj_ref: i32,
    bytes_ref: i32,
    off: i32,
    len: i32,
    _append: bool,
) -> Result<()> {
    // Same checks and order as HotSpot's writeBytes in io_util.c
    if bytes_ref == 0 {
        set_pending_null_pointer_exception()?;
        return Ok(());
    }
    if !region_in_bounds(bytes_ref, off, len)? {
        set_pending_index_out_of_bounds_exception()?;
        return Ok(());
    }
    if len == 0 {
        return Ok(());
    }
    let Some(mut file) = get_by_raw_id(obj_ref, FileOutputStream)? else {
        return Ok(());
    };
    let off = off as usize;
    let bytes = HEAP.get_entire_raw_data(bytes_ref)?[off..off + len as usize].to_vec();

    // A write to a full pipe or a stopped terminal can block indefinitely, so it runs in the
    // Safe state, from the host copy made above
    if let Err(e) = safepoint::blocking(|| file.write_all(&bytes)) {
        set_pending_io_exception(&e.to_string())?;
    };

    Ok(())
}
