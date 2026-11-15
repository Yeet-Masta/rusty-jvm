use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::i64_to_vec;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frames_util::{StackElement, StackFramesUtil};

const MAX_DEPTH: usize = 32;

const INTERPRETED_METHOD: i32 = 1;
pub const NATIVE_METHOD: i32 = 2;

/// This function is called from the Java code to fill in the stack trace of a throwable object.
/// Fields are set as follows:
///   - Throwable.backtrace: is Object[] type, internal object that holds the stack trace information
///       - backtrace[0]: Class[] holds Class reference
///       - backtrace[1]: long[] holds method reference (raw pointer address)
///       - backtrace[2]: int[] holds line number
///       - backtrace[3]: int[] holds method tag code: 1 - interpreted method, 2 - native method
///       - backtrace[4]: int[] holds the bytecode index of the executing instruction (JEP 358)
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

/// Builds the internal `Throwable.backtrace` object (`Object[5]` of parallel `Class[]`, `long[]`
/// method pointers, `int[]` lines, `int[]` tags, and `int[]` bytecode indices) from collected
/// frames. Shared by `fillInStackTrace` and `Thread.getStackTrace0` (which snapshots another thread
/// at a safepoint), so both feed the same `StackTraceElement` construction path.
pub(crate) fn build_backtrace(stack_elements: &[StackElement]) -> Result<i32> {
    let mut roots = JavaThread::hold_temp_roots(
        stack_elements
            .iter()
            .map(|element| Slot::Ref(element.class_ref()))
            .collect(),
    );
    let classes: Vec<i32> = stack_elements.iter().map(StackElement::class_ref).collect();
    let methods: Vec<i32> = stack_elements
        .iter()
        .flat_map(|element| i64_to_vec(element.method_ref()))
        .collect();
    let lines: Vec<i32> = stack_elements
        .iter()
        .map(|element| element.line_number() as i32)
        .collect();
    let tags: Vec<i32> = stack_elements
        .iter()
        .map(|element| {
            if element.is_native() {
                NATIVE_METHOD
            } else {
                INTERPRETED_METHOD
            }
        })
        .collect();
    let bcis: Vec<i32> = stack_elements
        .iter()
        .map(|element| element.bci() as i32)
        .collect();

    // Each array is created already filled; every one stays rooted until the holder publishes it.
    let mut destination = crate::vm::stack::rooted_slots::RootedSlots::new();
    let mut parts = [0i32; 5];
    for (part, (type_name, values)) in parts.iter_mut().zip([
        ("[Ljava/lang/Class;", &classes),
        ("[J", &methods),
        ("[I", &lines),
        ("[I", &tags),
        ("[I", &bcis),
    ]) {
        *part = HEAP.create_array_with_values(type_name, values, &mut destination)?;
        roots.add_root(Slot::Ref(*part));
    }
    let backtrace_ref =
        HEAP.create_array_with_values("[Ljava/lang/Object;", &parts, &mut destination)?;
    roots.add_root(Slot::Ref(backtrace_ref));

    Ok(backtrace_ref)
}
