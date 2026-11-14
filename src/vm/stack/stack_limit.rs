//! Java stack depth limits and `StackOverflowError`.
//!
//! Interpreted frames live on the heap, so unbounded recursion would otherwise grow until the
//! process runs out of memory. Like HotSpot, each thread gets a Java stack budget derived from
//! `-Xss` (or `-XX:ThreadStackSize`), and a call that would exceed it throws
//! `StackOverflowError` in the caller instead.
//!
//! Native re-entry (`Executor`, class initialization, reflection) also nests the Rust stack, so
//! the number of nested interpreter segments has its own cap that fits the VM threads' native
//! stacks ([`NATIVE_STACK_BYTES`]).
//!
//! Throwing needs a few frames of its own (the throwable's constructor and `fillInStackTrace`),
//! so while a `StackOverflowError` is being built the limits grow by a reserve, the same idea as
//! HotSpot's yellow zone.

use crate::vm::error::{Error, Result};
use crate::vm::exception::common::construct_exception_and_throw;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::stack::stack_frame::StackFrames;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// HotSpot's default `-Xss` on 64-bit platforms.
const DEFAULT_STACK_BYTES: usize = 1024 * 1024;
/// Bytes of `-Xss` budget charged per interpreted frame. HotSpot reaches roughly 10,000 to
/// 20,000 frames of a small recursive method with the default 1 MB stack; 64 bytes per frame
/// gives 16,384.
const BYTES_PER_FRAME: usize = 64;
/// Extra frames available while a `StackOverflowError` is constructed and thrown.
const RESERVE_FRAMES: usize = 512;

/// Native stack size of every thread that runs Java code. The memory is reserved, not committed,
/// so a large value costs address space only.
pub(crate) const NATIVE_STACK_BYTES: usize = 256 * 1024 * 1024;
/// Nested interpreter segments (native re-entries) allowed on one thread.
const MAX_NESTED_SEGMENTS: usize = 2_000;
const RESERVE_SEGMENTS: usize = 32;

static MAX_FRAMES: AtomicUsize = AtomicUsize::new(DEFAULT_STACK_BYTES / BYTES_PER_FRAME);

thread_local! {
    static IN_RESERVE: Cell<bool> = const { Cell::new(false) };
}

/// Applies `-Xss<size>` / `-XX:ThreadStackSize=<KB>` from the VM options; the last one wins.
pub(crate) fn configure(options: &[String]) {
    let mut bytes = None;
    for option in options {
        if let Some(size) = option.strip_prefix("-Xss") {
            bytes = parse_size(size).or(bytes);
        } else if let Some(kilobytes) = option.strip_prefix("-XX:ThreadStackSize=") {
            bytes = parse_size(kilobytes)
                .and_then(|kb| kb.checked_mul(1024))
                .or(bytes);
        }
    }
    if let Some(bytes) = bytes {
        set_max_frames((bytes / BYTES_PER_FRAME).max(64));
    }
}

pub(crate) fn set_max_frames(frames: usize) {
    MAX_FRAMES.store(frames, Ordering::Relaxed);
}

fn parse_size(text: &str) -> Option<usize> {
    let (digits, multiplier) = match text.chars().last()? {
        'k' | 'K' => (&text[..text.len() - 1], 1024),
        'm' | 'M' => (&text[..text.len() - 1], 1024 * 1024),
        'g' | 'G' => (&text[..text.len() - 1], 1024 * 1024 * 1024),
        _ => (text, 1),
    };
    digits.parse::<usize>().ok()?.checked_mul(multiplier)
}

fn in_reserve() -> bool {
    IN_RESERVE.with(Cell::get)
}

/// Whether pushing one more frame on top of `stack_frames` would exceed the thread's budget.
#[inline]
pub(crate) fn would_overflow(stack_frames: &StackFrames) -> bool {
    let limit = MAX_FRAMES.load(Ordering::Relaxed);
    let depth = stack_frames.depth();
    depth >= limit && (!in_reserve() || depth >= limit + RESERVE_FRAMES)
}

/// Whether one more nested interpreter segment would exceed the native stack budget.
pub(crate) fn segments_would_overflow(segments: usize) -> bool {
    segments >= MAX_NESTED_SEGMENTS
        && (!in_reserve() || segments >= MAX_NESTED_SEGMENTS + RESERVE_SEGMENTS)
}

struct ReserveGuard {
    previous: bool,
}

impl ReserveGuard {
    fn enter() -> Self {
        Self {
            previous: IN_RESERVE.with(|flag| flag.replace(true)),
        }
    }
}

impl Drop for ReserveGuard {
    fn drop(&mut self) {
        IN_RESERVE.with(|flag| flag.set(self.previous));
    }
}

/// Throws a new `StackOverflowError` into the frames of `stack_frames`, as if the call that would
/// have overflowed raised it.
pub(crate) fn throw_stack_overflow(stack_frames: &mut StackFrames) -> Result<()> {
    let _reserve = ReserveGuard::enter();
    construct_exception_and_throw(
        "java/lang/StackOverflowError",
        "<init>:()V",
        &[],
        stack_frames,
    )
}

/// A `StackOverflowError` as an error value, for overflow detected outside the interpreter loop
/// (too many nested native re-entries).
pub(crate) fn stack_overflow_error() -> crate::vm::error::Error {
    let _reserve = ReserveGuard::enter();
    match Executor::invoke_default_constructor("java/lang/StackOverflowError") {
        Ok(throwable) => Error::uncaught_exception(throwable[0]),
        Err(error) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_like_hotspot() {
        assert_eq!(parse_size("1m"), Some(1024 * 1024));
        assert_eq!(parse_size("512k"), Some(512 * 1024));
        assert_eq!(parse_size("2G"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("x"), None);
    }

    #[test]
    fn reserve_raises_the_segment_cap_temporarily() {
        assert!(segments_would_overflow(MAX_NESTED_SEGMENTS));
        let guard = ReserveGuard::enter();
        assert!(!segments_would_overflow(MAX_NESTED_SEGMENTS));
        assert!(segments_would_overflow(
            MAX_NESTED_SEGMENTS + RESERVE_SEGMENTS
        ));
        drop(guard);
        assert!(segments_would_overflow(MAX_NESTED_SEGMENTS));
    }
}
