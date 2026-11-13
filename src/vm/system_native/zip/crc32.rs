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
