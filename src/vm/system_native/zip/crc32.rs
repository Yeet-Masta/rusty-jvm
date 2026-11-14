use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crc32fast::Hasher;

/// `java.util.zip.CRC32.updateBytes0(I[BII)I`
pub(crate) fn updatebytes0(crc: i32, byte_array_ref: i32, off: i32, len: i32) -> Result<i32> {
    let mut hasher = Hasher::new_with_initial(crc as u32);

    // Hashing cannot block, so it runs in the Running state, directly over the array storage;
    // a collection cannot start until this thread reaches its next safepoint poll.
    hasher.update(&HEAP.get_entire_raw_data(byte_array_ref)?[off as usize..(off + len) as usize]);

    Ok(hasher.finalize() as i32)
}

/// `java.util.zip.CRC32.update(II)I`
pub(crate) fn update(crc: i32, b: i32) -> Result<i32> {
    let mut hasher = Hasher::new_with_initial(crc as u32);
    hasher.update(&[b as u8]);
    Ok(hasher.finalize() as i32)
}

/// `java.util.zip.CRC32.updateByteBuffer0(IJII)I`: `len` bytes of native memory at `address + off`.
pub(crate) fn update_byte_buffer0(crc: i32, address: i64, off: i32, len: i32) -> Result<i32> {
    let mut hasher = Hasher::new_with_initial(crc as u32);
    hasher.update(native_bytes(address, off, len));
    Ok(hasher.finalize() as i32)
}

/// `java.util.zip.Adler32.update(II)I`
pub(crate) fn adler32_update(adler: i32, b: i32) -> Result<i32> {
    Ok(adler32(adler as u32, &[b as u8]) as i32)
}

/// `java.util.zip.Adler32.updateBytes(I[BII)I`
pub(crate) fn adler32_update_bytes(adler: i32, array: i32, off: i32, len: i32) -> Result<i32> {
    let data = HEAP.get_entire_raw_data(array)?;
    Ok(adler32(adler as u32, &data[off as usize..(off + len) as usize]) as i32)
}

/// `java.util.zip.Adler32.updateByteBuffer(IJII)I`
pub(crate) fn adler32_update_byte_buffer(
    adler: i32,
    address: i64,
    off: i32,
    len: i32,
) -> Result<i32> {
    Ok(adler32(adler as u32, native_bytes(address, off, len)) as i32)
}

/// `len` bytes of native memory at `address + off`.
pub(crate) fn native_bytes<'a>(address: i64, off: i32, len: i32) -> &'a [u8] {
    if len <= 0 {
        return &[];
    }
    unsafe {
        std::slice::from_raw_parts((address + off as i64) as usize as *const u8, len as usize)
    }
}

/// `len` writable bytes of native memory at `address`.
pub(crate) fn native_bytes_mut<'a>(address: i64, len: i32) -> &'a mut [u8] {
    if len <= 0 {
        return &mut [];
    }
    unsafe { std::slice::from_raw_parts_mut(address as usize as *mut u8, len as usize) }
}

/// zlib's Adler-32, continuing from `adler`.
pub(crate) fn adler32(adler: u32, data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (adler & 0xffff, adler >> 16);
    // 5552 bytes is the most that can be summed before the 32-bit sums could overflow.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::adler32;

    #[test]
    fn adler32_matches_zlib() {
        assert_eq!(adler32(1, b"abc"), 0x024d0127);
        assert_eq!(adler32(1, b""), 1);
        let (mut a, mut b) = (1u64, 0u64);
        for _ in 0..100_000 {
            a = (a + 0xff) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(1, &[0xff; 100_000]), ((b << 16) | a) as u32);
    }
}
