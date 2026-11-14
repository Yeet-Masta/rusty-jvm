use crate::vm::commons::auto_dash_map::auto_dash_map::AutoDashMap;
use crate::vm::commons::auto_dash_map::auto_dash_map_i64::AutoDashMapI64;
use crate::vm::error::{Error, Result};
use crate::vm::heap::heap::HEAP;
use crate::vm::system_native::zip::common::DEFAULT_WINDOW_BITS;
use crate::vm::system_native::zip::crc32::{native_bytes, native_bytes_mut};
use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus, StreamResult};
use std::sync::LazyLock;

static REGISTRY: LazyLock<AutoDashMapI64<InflateValue>> = LazyLock::new(|| AutoDashMapI64::new(1));

struct InflateValue(InflateState, DataFormat);

/// `java.util.zip.Inflater.initIDs()V`
pub(crate) fn init_ids() -> Result<()> {
    Ok(()) // todo: implement me
}

/// `java.util.zip.Inflater.init(Z)J`
pub(crate) fn init(nowrap: bool) -> Result<i64> {
    let data_format = DataFormat::from_window_bits(if nowrap {
        -DEFAULT_WINDOW_BITS
    } else {
        DEFAULT_WINDOW_BITS
    });
    let inflate_state = InflateState::new(data_format);

    let addr = REGISTRY.insert_auto(InflateValue(inflate_state, data_format));
    Ok(addr)
}

/// `java.util.zip.Inflater.inflateBytesBytes(J[BII[BII)J`
pub(crate) fn inflate_bytes_bytes(
    _this: i32,
    addr: i64,
    input_array_ref: i32,
    input_off: i32,
    input_len: i32,
    output_array_ref: i32,
    output_off: i32,
    output_len: i32,
) -> Result<i64> {
    let input = heap_bytes(input_array_ref, input_off, input_len)?;
    let mut output = vec![0; output_len as usize];
    let (status, written) = run(addr, &input, &mut output)?;
    store_heap_bytes(output_array_ref, output_off, &output[..written])?;
    Ok(status)
}

/// `java.util.zip.Inflater.inflateBytesBuffer(J[BIIJI)J`
pub(crate) fn inflate_bytes_buffer(
    _this: i32,
    addr: i64,
    input_array_ref: i32,
    input_off: i32,
    input_len: i32,
    output_address: i64,
    output_len: i32,
) -> Result<i64> {
    let input = heap_bytes(input_array_ref, input_off, input_len)?;
    Ok(run(addr, &input, native_bytes_mut(output_address, output_len))?.0)
}

/// `java.util.zip.Inflater.inflateBufferBytes(JJI[BII)J`
pub(crate) fn inflate_buffer_bytes(
    _this: i32,
    addr: i64,
    input_address: i64,
    input_len: i32,
    output_array_ref: i32,
    output_off: i32,
    output_len: i32,
) -> Result<i64> {
    let input = native_bytes(input_address, 0, input_len);
    let mut output = vec![0; output_len as usize];
    let (status, written) = run(addr, input, &mut output)?;
    store_heap_bytes(output_array_ref, output_off, &output[..written])?;
    Ok(status)
}

/// `java.util.zip.Inflater.inflateBufferBuffer(JJIJI)J`
pub(crate) fn inflate_buffer_buffer(
    _this: i32,
    addr: i64,
    input_address: i64,
    input_len: i32,
    output_address: i64,
    output_len: i32,
) -> Result<i64> {
    let input = native_bytes(input_address, 0, input_len);
    Ok(run(addr, input, native_bytes_mut(output_address, output_len))?.0)
}

/// `java.util.zip.Inflater.getAdler(J)I`
pub(crate) fn get_adler(addr: i64) -> Result<i32> {
    let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
    Ok(entry.value_mut().0.decompressor().adler32().unwrap_or(1) as i32)
}

/// `java.util.zip.Inflater.setDictionary(J[BII)V`: the miniz_oxide inflater has no preset
/// dictionary support.
pub(crate) fn set_dictionary(_addr: i64, _array: i32, _off: i32, _len: i32) -> Result<()> {
    Err(crate::vm::exception::pending_helpers::java_exception(
        "java/lang/UnsupportedOperationException",
        "Inflater preset dictionaries are not supported by rusty-jvm",
    ))
}

/// `java.util.zip.Inflater.setDictionaryBuffer(JJI)V`
pub(crate) fn set_dictionary_buffer(addr: i64, _address: i64, len: i32) -> Result<()> {
    set_dictionary(addr, 0, 0, len)
}

fn missing(addr: i64) -> Error {
    Error::new_execution(&format!(
        "Inflater not found in registry for address: {addr}"
    ))
}

fn heap_bytes(array: i32, off: i32, len: i32) -> Result<Vec<u8>> {
    Ok(HEAP.get_entire_raw_data(array)?[off as usize..(off + len) as usize].to_vec())
}

fn store_heap_bytes(array: i32, off: i32, data: &[u8]) -> Result<()> {
    HEAP.get_entire_raw_data_mut(array)?[off as usize..off as usize + data.len()]
        .copy_from_slice(data);
    Ok(())
}

fn run(addr: i64, input: &[u8], output: &mut [u8]) -> Result<(i64, usize)> {
    let stream_result = crate::vm::safepoint::blocking(|| {
        let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
        Ok::<_, Error>(inflate(
            &mut entry.value_mut().0,
            input,
            output,
            MZFlush::Sync,
        ))
    })?;
    let written = stream_result.bytes_written;
    Ok((check_inflate_status(stream_result)?, written))
}

fn check_inflate_status(stream_result: StreamResult) -> Result<i64> {
    let mut input_used = 0usize;
    let mut output_used = 0usize;
    let mut finished = 0;
    let mut need_dict = 0;

    let status = stream_result.status;
    match status {
        Ok(MZStatus::Ok) => {
            input_used = stream_result.bytes_consumed;
            output_used = stream_result.bytes_written;
        }
        Ok(MZStatus::StreamEnd) => {
            input_used = stream_result.bytes_consumed;
            output_used = stream_result.bytes_written;
            finished = 1;
        }
        Ok(MZStatus::NeedDict) => {
            input_used = stream_result.bytes_consumed;
            output_used = stream_result.bytes_written;
            need_dict = 1;
        }
        Err(MZError::Buf) => {
            // java code will try to handle this by resizing buffer (probably)
        }
        Err(MZError::Data) => {
            // This is a data error, we should throw DataFormatException
            return Err(Error::new_execution(&format!(
                "data format error in inflater stream_result: {stream_result:?}"
            )));
        }
        Err(MZError::Mem) => {
            // This is a memory error, we should throw OutOfMemoryError
            return Err(Error::new_execution(&format!(
                "memory error in inflater stream_result: {stream_result:?}"
            )));
        }
        Err(MZError::ErrNo)
        | Err(MZError::Stream)
        | Err(MZError::Version)
        | Err(MZError::Param) => {
            // throw InternalError here
            return Err(Error::new_execution(&format!("Inflate error: {status:?}")));
        }
        Err(_) => {
            // throw InternalError here
            return Err(Error::new_execution(&format!(
                "Unknown inflate error: {status:?}"
            )));
        }
    };

    // Pack into jlong like OpenJDK java code assumes
    Ok(((input_used as i64) & 0x7FFF_FFFF)
        | (((output_used as i64) & 0x7FFF_FFFF) << 31)
        | ((finished as i64) << 62)
        | ((need_dict as i64) << 63))
}

/// `java.util.zip.Inflater.end(J)V`
pub(crate) fn end(addr: i64) -> Result<()> {
    let _safe = crate::vm::safepoint::Blocking::enter(crate::vm::safepoint::Reason::Blocking);
    REGISTRY.remove(addr).ok_or_else(|| {
        Error::new_execution(&format!("Address {addr} does not exist in REGISTRY"))
    })?;

    Ok(())
}

/// `java.util.zip.Inflater.reset(J)V`
pub(crate) fn reset(addr: i64) -> Result<()> {
    let _safe = crate::vm::safepoint::Blocking::enter(crate::vm::safepoint::Reason::Blocking);
    let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| {
        Error::new_execution(&format!(
            "Inflater not found in registry for address: {}",
            addr
        ))
    })?;

    let data_format = entry.value().1;
    let inflate_state = InflateState::new(data_format);
    *entry.value_mut() = InflateValue(inflate_state, data_format);

    Ok(())
}
