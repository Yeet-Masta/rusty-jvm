use crate::vm::commons::auto_dash_map::auto_dash_map::AutoDashMap;
use crate::vm::commons::auto_dash_map::auto_dash_map_i64::AutoDashMapI64;
use crate::vm::error::{Error, Result};
use crate::vm::heap::heap::HEAP;
use crate::vm::system_native::zip::common::DEFAULT_WINDOW_BITS;
use crate::vm::system_native::zip::crc32::{native_bytes, native_bytes_mut};
use miniz_oxide::deflate::core::{create_comp_flags_from_zip_params, CompressorOxide};
use miniz_oxide::deflate::stream::deflate;
use miniz_oxide::{MZError, MZFlush, MZStatus, StreamResult};
use std::sync::LazyLock;

static REGISTRY: LazyLock<AutoDashMapI64<CompressorOxide>> =
    LazyLock::new(|| AutoDashMapI64::new(1));

/// `java.util.zip.Deflater.init(IIZ)J`
pub(crate) fn init(level: i32, strategy: i32, nowrap: bool) -> Result<i64> {
    let deflate_flags = create_comp_flags_from_zip_params(
        level,
        if nowrap {
            -DEFAULT_WINDOW_BITS
        } else {
            DEFAULT_WINDOW_BITS
        },
        strategy,
    );
    let compress = CompressorOxide::new(deflate_flags);

    let inserted_key = REGISTRY.insert_auto(compress);
    Ok(inserted_key)
}

/// `java.util.zip.Deflater.deflateBytesBytes(J[BII[BIIII)J`
pub(crate) fn deflate_bytes_bytes(
    _this: i32,
    addr: i64,
    input_array_ref: i32,
    input_off: i32,
    input_len: i32,
    output_array_ref: i32,
    output_off: i32,
    output_len: i32,
    flush: i32,
    params: i32,
) -> Result<i64> {
    let input = heap_bytes(input_array_ref, input_off, input_len)?;
    let mut output = vec![0; output_len as usize];
    let (status, written) = run(addr, &input, &mut output, flush, params)?;
    store_heap_bytes(output_array_ref, output_off, &output[..written])?;
    Ok(status)
}

/// `java.util.zip.Deflater.deflateBytesBuffer(J[BIIJIII)J`
pub(crate) fn deflate_bytes_buffer(
    _this: i32,
    addr: i64,
    input_array_ref: i32,
    input_off: i32,
    input_len: i32,
    output_address: i64,
    output_len: i32,
    flush: i32,
    params: i32,
) -> Result<i64> {
    let input = heap_bytes(input_array_ref, input_off, input_len)?;
    let output = native_bytes_mut(output_address, output_len);
    Ok(run(addr, &input, output, flush, params)?.0)
}

/// `java.util.zip.Deflater.deflateBufferBytes(JJI[BIIII)J`
pub(crate) fn deflate_buffer_bytes(
    _this: i32,
    addr: i64,
    input_address: i64,
    input_len: i32,
    output_array_ref: i32,
    output_off: i32,
    output_len: i32,
    flush: i32,
    params: i32,
) -> Result<i64> {
    let input = native_bytes(input_address, 0, input_len);
    let mut output = vec![0; output_len as usize];
    let (status, written) = run(addr, input, &mut output, flush, params)?;
    store_heap_bytes(output_array_ref, output_off, &output[..written])?;
    Ok(status)
}

/// `java.util.zip.Deflater.deflateBufferBuffer(JJIJIII)J`
pub(crate) fn deflate_buffer_buffer(
    _this: i32,
    addr: i64,
    input_address: i64,
    input_len: i32,
    output_address: i64,
    output_len: i32,
    flush: i32,
    params: i32,
) -> Result<i64> {
    let input = native_bytes(input_address, 0, input_len);
    let output = native_bytes_mut(output_address, output_len);
    Ok(run(addr, input, output, flush, params)?.0)
}

/// `java.util.zip.Deflater.getAdler(J)I`
pub(crate) fn get_adler(addr: i64) -> Result<i32> {
    let entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
    Ok(entry.value().adler32() as i32)
}

/// `java.util.zip.Deflater.reset(J)V`
pub(crate) fn reset(addr: i64) -> Result<()> {
    let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
    entry.value_mut().reset();
    Ok(())
}

/// `java.util.zip.Deflater.setDictionary(J[BII)V`: the miniz_oxide compressor has no preset
/// dictionary support.
pub(crate) fn set_dictionary(_addr: i64, _array: i32, _off: i32, _len: i32) -> Result<()> {
    Err(crate::vm::exception::pending_helpers::java_exception(
        "java/lang/UnsupportedOperationException",
        "Deflater preset dictionaries are not supported by rusty-jvm",
    ))
}

/// `java.util.zip.Deflater.setDictionaryBuffer(JJI)V`
pub(crate) fn set_dictionary_buffer(addr: i64, _address: i64, len: i32) -> Result<()> {
    set_dictionary(addr, 0, 0, len)
}

fn missing(addr: i64) -> Error {
    Error::new_execution(&format!(
        "Deflater not found in registry for address: {addr}"
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

/// One `deflate` step, or applying new parameters when bit 0 of `params` is set, as the JDK's
/// `doDeflate` does. Returns the packed status and the number of bytes written.
fn run(
    addr: i64,
    input: &[u8],
    output: &mut [u8],
    flush: i32,
    params: i32,
) -> Result<(i64, usize)> {
    if params & 1 != 0 {
        let level = params >> 3;
        let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
        // zlib's default level (-1) is 6.
        let level = if level < 0 { 6 } else { level.min(10) as u8 };
        entry.value_mut().set_compression_level_raw(level);
        // Parameters applied (bit 63 clear); nothing consumed or produced.
        return Ok((0, 0));
    }
    let flush = MZFlush::new(flush)?;
    let stream_result = crate::vm::safepoint::blocking(|| {
        let mut entry = REGISTRY.get_mut(addr).ok_or_else(|| missing(addr))?;
        Ok::<_, Error>(deflate(entry.value_mut(), input, output, flush))
    })?;
    let written = stream_result.bytes_written;
    Ok((check_deflate_status(stream_result)?, written))
}

/// Returns the packed jlong status like HotSpot's checkDeflateStatus
fn check_deflate_status(stream_result: StreamResult) -> Result<i64> {
    let input_used = stream_result.bytes_consumed;
    let output_used = stream_result.bytes_written;
    let mut finished = 0;

    let status = stream_result.status;
    match status {
        Ok(MZStatus::Ok) => {}
        Ok(MZStatus::StreamEnd) => {
            finished = 1;
        }
        Err(MZError::Buf) => {
            // java code will try to handle this by resizing buffer (probably)
        }
        Ok(MZStatus::NeedDict)
        | Err(MZError::ErrNo)
        | Err(MZError::Stream)
        | Err(MZError::Data)
        | Err(MZError::Mem)
        | Err(MZError::Version)
        | Err(MZError::Param) => {
            // throw InternalError here
            return Err(Error::new_execution(&format!("Deflate error: {status:?}")));
        }
        Err(_) => {
            // throw InternalError here
            return Err(Error::new_execution(&format!(
                "Unknown deflate error: {status:?}"
            )));
        }
    };

    // Pack into jlong like OpenJDK java code assumes
    Ok(((input_used as i64) & 0x7FFF_FFFF)
        | (((output_used as i64) & 0x7FFF_FFFF) << 31)
        | ((finished as i64) << 62))
}

/// `java.util.zip.Deflater.end(J)V`
pub(crate) fn end(addr: i64) -> Result<()> {
    let _safe = crate::vm::safepoint::Blocking::enter(crate::vm::safepoint::Reason::Blocking);
    REGISTRY.remove(addr).ok_or_else(|| {
        Error::new_execution(&format!("Address {addr} does not exist in REGISTRY"))
    })?;

    Ok(())
}
