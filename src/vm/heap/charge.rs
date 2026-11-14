//! Versioned formula in docs/managed-heap-accounting.md.
use crate::vm::gc::budget::{add, aligned, bytes, AllocError, AllocResult};
use crate::vm::heap::java_instance::HeapValue;
use std::mem::size_of;

/// Estimated heap-table and recorded-charge-table bucket overhead per entry.
const ENTRY_ALLOWANCE: u64 = 64;
pub(super) fn header() -> AllocResult<u64> {
    aligned(
        bytes(1, size_of::<HeapValue>())?
            .checked_add(ENTRY_ALLOWANCE)
            .ok_or(AllocError::Arithmetic)?,
    )
}

pub(super) fn array(descriptor_capacity: usize, payload_capacity: usize) -> AllocResult<u64> {
    let mut total = header()?;
    add(&mut total, bytes(descriptor_capacity, 1)?)?;
    add(&mut total, bytes(payload_capacity, 1)?)?;
    Ok(total)
}

/// An object: its header and one `i32` per word of field storage.
pub(super) fn instance(words: usize) -> AllocResult<u64> {
    let mut total = header()?;
    add(&mut total, bytes(words, size_of::<i32>())?)?;
    Ok(total)
}

pub(super) fn intern(capacity: usize) -> AllocResult<u64> {
    let mut total = aligned(
        bytes(1, size_of::<(String, i32)>())?
            .checked_add(ENTRY_ALLOWANCE)
            .ok_or(AllocError::Arithmetic)?,
    )?;
    add(&mut total, bytes(capacity, 1)?)?;
    Ok(total)
}
