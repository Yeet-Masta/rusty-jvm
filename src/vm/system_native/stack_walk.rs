//! `java.lang.StackWalker` support: the natives of `StackStreamFactory`, `StackFrameInfo` and
//! `StackTraceElement.initStackTraceElement`, following HotSpot's `StackWalk` protocol.
//!
//! `callStackWalk` snapshots the calling thread's frames, fills the first batch of the Java-side
//! frame buffer, and calls back into `AbstractStackWalker.doStackWalk`. While that callback runs,
//! the walker may ask for more batches through `fetchStackFrames`, identified by the anchor handed
//! to `doStackWalk`. The snapshot is dropped once `doStackWalk` returns, so a stale anchor is an
//! error, as in HotSpot.

use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frames_util::{StackElement, StackFramesUtil};
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::method_handle_natives::resolved_method_name::ResolvedMethodName;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};

const CLASS_INFO_ONLY: i32 = 0x2;
const SHOW_HIDDEN_FRAMES: i32 = 0x20;

const MN_IS_METHOD: i32 = 0x0001_0000;
const MN_IS_CONSTRUCTOR: i32 = 0x0002_0000;
const MN_CALLER_SENSITIVE: i32 = 0x0010_0000;
const MN_HIDDEN_MEMBER: i32 = 0x0040_0000;
/// `JVM_RECOGNIZED_METHOD_MODIFIERS`.
const RECOGNIZED_METHOD_MODIFIERS: i32 = 0x1DFF;

const STACK_WALKER: &str = "java/lang/StackWalker";
const ABSTRACT_STACK_WALKER: &str = "java/lang/StackStreamFactory$AbstractStackWalker";
const CLASS_FRAME_INFO: &str = "java/lang/ClassFrameInfo";
const STACK_FRAME_INFO: &str = "java/lang/StackFrameInfo";
const HIDDEN: &str = "Ljdk/internal/vm/annotation/Hidden;";
const CALLER_SENSITIVE: &str = "Ljdk/internal/reflect/CallerSensitive;";

struct Walk {
    frames: Vec<StackElement>,
    next: usize,
}

thread_local! {
    static WALKS: RefCell<HashMap<i64, Walk>> = RefCell::new(HashMap::new());
}
static NEXT_ANCHOR: AtomicI64 = AtomicI64::new(1);

/// Removes the walk's snapshot when `callStackWalk` returns, however it returns.
struct WalkGuard(i64);
impl Drop for WalkGuard {
    fn drop(&mut self) {
        WALKS.with(|walks| walks.borrow_mut().remove(&self.0));
    }
}

fn method(element: &StackElement) -> &'static JavaMethod {
    // SAFETY: class unloading is disabled, so the method registry owns every `JavaMethod` for
    // the life of the VM (the same invariant `Throwable.backtrace` relies on).
    unsafe { &*(element.method_ref() as *const JavaMethod) }
}

fn is_hidden(element: &StackElement) -> Result<bool> {
    let method = method(element);
    if method.has_annotation(HIDDEN) || method.is_polymorphic_signature() {
        return Ok(true);
    }
    // Every method of a hidden class is a hidden frame, as in HotSpot.
    Ok(crate::vm::method_area::loaded_classes::CLASSES
        .get_loaded(method.class_name())
        .is_some_and(|class| class.hidden()))
}

/// The frames a walk sees: the caller's stack minus the walker's own implementation frames
/// (HotSpot skips frames of `StackWalker`, `AbstractStackWalker` and its direct subclasses), the
/// requested number of skipped frames and, unless asked for, hidden frames.
fn snapshot(mode: i32, skip_frames: i32) -> Result<Vec<StackElement>> {
    let all = StackFramesUtil::collect_current_stack(usize::MAX)?;
    let mut start = 0;
    while let Some(element) = all.get(start) {
        let klass = klass(element.class_ref())?;
        let name = klass.this_class_name();
        let is_walker = name == STACK_WALKER
            || name == ABSTRACT_STACK_WALKER
            || klass.parent().as_deref() == Some(ABSTRACT_STACK_WALKER);
        if !is_walker {
            break;
        }
        start += 1;
    }
    let show_hidden = mode & SHOW_HIDDEN_FRAMES != 0;
    let mut frames = Vec::with_capacity(all.len() - start);
    for element in all.into_iter().skip(start) {
        if show_hidden || !is_hidden(&element)? {
            frames.push(element);
        }
    }
    let skip = (skip_frames.max(0) as usize).min(frames.len());
    frames.drain(..skip);
    Ok(frames)
}

fn member_flags(element: &StackElement) -> Result<i32> {
    let method = method(element);
    let mut flags = method.access_flags() & RECOGNIZED_METHOD_MODIFIERS;
    flags |= if method.name() == "<init>" {
        MN_IS_CONSTRUCTOR
    } else {
        MN_IS_METHOD
    };
    if method.has_annotation(CALLER_SENSITIVE) {
        flags |= MN_CALLER_SENSITIVE;
    }
    if is_hidden(element)? {
        flags |= MN_HIDDEN_MEMBER;
    }
    Ok(flags)
}

/// Fills `frames[start_index..buffer_size]` from the walk, returning how many were filled.
fn fill_batch(
    anchor: i64,
    mode: i32,
    buffer_size: i32,
    start_index: i32,
    frames_ref: i32,
) -> Result<i32> {
    let capacity = (buffer_size - start_index).max(0) as usize;
    let batch = WALKS.with(|walks| {
        let mut walks = walks.borrow_mut();
        let walk = walks
            .get_mut(&anchor)
            .ok_or_else(|| Error::new_execution("doStackWalk: corrupted buffers on stack"))?;
        let end = (walk.next + capacity).min(walk.frames.len());
        let batch = walk.frames[walk.next..end].to_vec();
        walk.next = end;
        Ok::<_, Error>(batch)
    })?;

    for (offset, element) in batch.iter().enumerate() {
        let index = start_index + offset as i32;
        let frame_ref = HEAP.get_array_value(frames_ref, index)?[0];
        if frame_ref == 0 {
            return Err(Error::new_execution(
                "StackWalker frame buffer has a null slot",
            ));
        }
        let old_flags = HEAP.get_object_field_value(frame_ref, CLASS_FRAME_INFO, "flags")?[0];
        let flags = old_flags | member_flags(element)?;
        if mode & CLASS_INFO_ONLY != 0 {
            HEAP.set_object_field_value(
                frame_ref,
                CLASS_FRAME_INFO,
                "classOrMemberName",
                vec![element.class_ref()],
            )?;
        } else {
            let method = method(element);
            let resolved = ResolvedMethodName::new_create_instance(
                element.class_ref(),
                method.slot() as i64,
            )?;
            resolved.propagate_all()?;
            HEAP.set_object_field_value(
                frame_ref,
                CLASS_FRAME_INFO,
                "classOrMemberName",
                vec![resolved.resolved_method_name_ref()],
            )?;
            let bci = if method.is_native() {
                -1
            } else {
                element.bci() as i32
            };
            HEAP.set_object_field_value(frame_ref, STACK_FRAME_INFO, "bci", vec![bci])?;
        }
        HEAP.set_object_field_value(frame_ref, CLASS_FRAME_INFO, "flags", vec![flags])?;
    }
    Ok(batch.len() as i32)
}

/// `StackStreamFactory$AbstractStackWalker.callStackWalk(IILjdk/internal/vm/ContinuationScope;Ljdk/internal/vm/Continuation;II[Ljava/lang/Object;)Ljava/lang/Object;`
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_stack_walk(
    walker_ref: i32,
    mode: i32,
    skip_frames: i32,
    _cont_scope: i32,
    _continuation: i32,
    buffer_size: i32,
    start_index: i32,
    frames_ref: i32,
) -> Result<i32> {
    let frames = snapshot(mode, skip_frames)?;
    let anchor = NEXT_ANCHOR.fetch_add(1, Ordering::Relaxed);
    WALKS.with(|walks| walks.borrow_mut().insert(anchor, Walk { frames, next: 0 }));
    let _guard = WalkGuard(anchor);

    let _roots = JavaThread::hold_temp_roots(vec![Slot::Ref(walker_ref), Slot::Ref(frames_ref)]);
    let num_frames = fill_batch(anchor, mode, buffer_size, start_index, frames_ref)?;

    let args = [
        StackValueKind::I64(anchor),
        skip_frames.into(),
        num_frames.into(),
        start_index.into(),
        (start_index + num_frames).into(),
    ];
    let result = Executor::invoke_non_static_method(
        ABSTRACT_STACK_WALKER,
        "doStackWalk:(JIIII)Ljava/lang/Object;",
        walker_ref,
        &args,
    )?;
    Ok(result.first().map(|slot| slot.value()).unwrap_or(0))
}

/// `StackStreamFactory$AbstractStackWalker.fetchStackFrames(IJIII[Ljava/lang/Object;)I`
pub(crate) fn fetch_stack_frames(
    _walker_ref: i32,
    mode: i32,
    anchor: i64,
    _last_batch_frame_count: i32,
    buffer_size: i32,
    start_index: i32,
    frames_ref: i32,
) -> Result<i32> {
    fill_batch(anchor, mode, buffer_size, start_index, frames_ref)
}

/// `StackStreamFactory$AbstractStackWalker.setContinuation(J[Ljava/lang/Object;Ljdk/internal/vm/Continuation;)V`
///
/// Walks here always cover the carrier's whole stack in one snapshot, so there is no separate
/// continuation to switch to.
pub(crate) fn set_continuation(
    _walker_ref: i32,
    _anchor: i64,
    _frames_ref: i32,
    _continuation: i32,
) -> Result<()> {
    Ok(())
}

/// `StackStreamFactory.checkStackWalkModes()Z`
pub(crate) fn check_stack_walk_modes() -> Result<bool> {
    Ok(true)
}

fn method_of_frame_info(frame_info_ref: i32) -> Result<(i32, std::sync::Arc<JavaMethod>)> {
    let member =
        HEAP.get_object_field_value(frame_info_ref, CLASS_FRAME_INFO, "classOrMemberName")?[0];
    let resolved = ResolvedMethodName::new_load_by_ref(member)?;
    let method = klass(resolved.vmholder())?.get_method_by_index(resolved.vmtarget())?;
    Ok((resolved.vmholder(), method))
}

/// `StackFrameInfo.expandStackFrameInfo()V`: fills `name` and `type` (the descriptor string).
pub(crate) fn expand_stack_frame_info(frame_info_ref: i32) -> Result<()> {
    let (_, method) = method_of_frame_info(frame_info_ref)?;
    let name_ref = StringPoolHelper::get_string(method.name())?;
    let descriptor = method
        .name_signature()
        .split_once(':')
        .map(|(_, descriptor)| descriptor)
        .unwrap_or("");
    let type_ref = StringPoolHelper::get_string(descriptor)?;
    HEAP.set_object_field_value(frame_info_ref, STACK_FRAME_INFO, "name", vec![name_ref])?;
    HEAP.set_object_field_value(frame_info_ref, STACK_FRAME_INFO, "type", vec![type_ref])?;
    Ok(())
}

/// `StackTraceElement.initStackTraceElement(Ljava/lang/StackTraceElement;Ljava/lang/StackFrameInfo;)V`
pub(crate) fn init_stack_trace_element(element_ref: i32, frame_info_ref: i32) -> Result<()> {
    let (class_ref, method) = method_of_frame_info(frame_info_ref)?;
    let bci = HEAP.get_object_field_value(frame_info_ref, STACK_FRAME_INFO, "bci")?[0];
    let line_number = if method.is_native() {
        -2
    } else {
        method
            .code_context()
            .map(|code| {
                StackFramesUtil::extract_line_number(code.line_numbers(), bci.max(0) as usize)
                    as i32
            })
            .unwrap_or(-1)
    };
    crate::vm::system_native::stack_trace_element::fill_element(
        element_ref,
        class_ref,
        method.name(),
        line_number,
    )?;
    Ok(())
}
