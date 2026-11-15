use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crate::vm::stack::stack_frames_util::{StackElement, StackFramesUtil};

const MAX_DEPTH: usize = 32;

const INTERPRETED_METHOD: i32 = 1;
pub const NATIVE_METHOD: i32 = 2;

/// Words per frame in a backtrace: class, method (two words), line, tag, bytecode index.
const FRAME_WORDS: usize = 6;

/// One frame of a backtrace, as [`build_backtrace`] records it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BacktraceFrame {
    /// The declaring class's mirror.
    pub(crate) class_ref: i32,
    /// The method, as the address of its `JavaMethod` (`Arc::as_ptr`).
    pub(crate) method_ref: i64,
    pub(crate) line_number: i32,
    /// [`INTERPRETED_METHOD`] or [`NATIVE_METHOD`].
    pub(crate) tag: i32,
    /// The bytecode index of the executing instruction (JEP 358).
    pub(crate) bci: i32,
}

/// This function is called from the Java code to fill in the stack trace of a throwable object.
/// Fields are set as follows:
///   - Throwable.backtrace: an `int[]` of [`FRAME_WORDS`] words per frame, newest first: the
///     class mirror, the method reference (raw pointer address, low word then high word), the
///     line number, the method tag (1 - interpreted, 2 - native) and the bytecode index. Only
///     this VM's natives read it, through [`backtrace_frames`].
///   - Throwable.depth: size of the stack trace
pub(crate) fn fill_in_stack_trace(throwable_ref: i32, _dummy: i32) -> Result<i32> {
    let throwable_name = HEAP.get_instance_name(throwable_ref)?;
    let stack_elements = StackFramesUtil::collect_stack_trace(&throwable_name, MAX_DEPTH)?;
    let depth = stack_elements.len() as i32;

    let backtrace_ref = build_backtrace(&stack_elements)?;

    HEAP.set_object_field_value(
        throwable_ref,
        &throwable_name,
        "backtrace",
        vec![backtrace_ref],
    )?;
    HEAP.set_object_field_value(throwable_ref, &throwable_name, "depth", vec![depth])?;

    Ok(throwable_ref)
}

/// Builds the internal `Throwable.backtrace` object (see [`fill_in_stack_trace`]) from collected
/// frames in a single allocation. Shared by `fillInStackTrace` and `Thread.getStackTrace0` (which
/// snapshots another thread at a safepoint), so both feed the same `StackTraceElement`
/// construction path.
///
/// The class mirrors it holds are plain words, which a collector does not trace. That is safe
/// because classes are never unloaded: every mirror stays reachable from the class registry.
pub(crate) fn build_backtrace(stack_elements: &[StackElement]) -> Result<i32> {
    let mut words = Vec::with_capacity(stack_elements.len() * FRAME_WORDS);
    for element in stack_elements {
        let method = element.method_ref();
        words.extend_from_slice(&[
            element.class_ref(),
            method as i32,
            (method >> 32) as i32,
            element.line_number() as i32,
            if element.is_native() {
                NATIVE_METHOD
            } else {
                INTERPRETED_METHOD
            },
            element.bci() as i32,
        ]);
    }
    let mut destination = crate::vm::stack::rooted_slots::RootedSlots::new();
    let backtrace_ref = HEAP.create_array_with_values("[I", &words, &mut destination)?;
    Ok(backtrace_ref)
}

/// The frames a backtrace built by [`build_backtrace`] holds, newest first.
pub(crate) fn backtrace_frames(backtrace_ref: i32) -> Result<Vec<BacktraceFrame>> {
    let data = HEAP.get_entire_raw_data(backtrace_ref)?;
    let word = |at: usize| i32::from_ne_bytes(data[at * 4..at * 4 + 4].try_into().unwrap());
    Ok((0..data.len() / 4 / FRAME_WORDS)
        .map(|frame| {
            let at = frame * FRAME_WORDS;
            BacktraceFrame {
                class_ref: word(at),
                method_ref: i64::from(word(at + 1) as u32) | (i64::from(word(at + 2)) << 32),
                line_number: word(at + 3),
                tag: word(at + 4),
                bci: word(at + 5),
            }
        })
        .collect())
}
