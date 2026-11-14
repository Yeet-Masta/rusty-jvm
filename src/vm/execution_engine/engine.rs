use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::{
    ops_comparison_processor, ops_constant_processor, ops_control_processor,
    ops_conversion_processor, ops_extended_processor, ops_load_processor, ops_math_processor,
    ops_reference_processor, ops_stack_processor, ops_store_processor,
};
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::safepoint;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::stack_frame::{StackFrame, StackFrames};
use crate::vm::stack::stack_frames_util::StackFramesUtil;
use crate::vm::stack::stack_limit;
use std::sync::Arc;
use tracing::{span, trace, Level};

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
#[path = "engine_coordinator_tests.rs"]
mod coordinator_tests;

#[cfg(test)]
thread_local! {
    static INJECT_INTERNAL_ERROR: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct Engine {}

impl Engine {
    pub(crate) fn execute(initial_stack_frame: StackFrame, reason: &str) -> Result<RootedSlots> {
        let mut destination = RootedSlots::new();
        trace!("@@@ Entering execute: {reason}");

        if stack_limit::segments_would_overflow(JavaThread::segment_count()) {
            return Err(stack_limit::stack_overflow_error());
        }
        let mut stack_frames = StackFrames::new(vec![initial_stack_frame]);
        // Register this segment on the thread's stack chain for the whole interpreter loop so
        // stack-walking natives can traverse it (and older segments) without a parameter. The
        // guard pops it on return/unwind, keeping the chain correct across native re-entries.
        let _stack_frames_guard = JavaThread::register_stack_frames(&mut stack_frames);
        let execution_result = (|| -> Result<()> {
            // The executing frame's class name and class, refreshed only when the frame changes.
            // Cloning the shared `Arc`s on every bytecode would make all threads running the same
            // method hammer one reference counter, which serializes them on its cache line.
            let mut class_name: Option<Arc<String>> = None;
            let mut klass: Option<Arc<JavaClass>> = None;
            while !stack_frames.is_empty() {
                Self::enter_synchronized_method(&mut stack_frames)?;
                #[cfg(test)]
                if INJECT_INTERNAL_ERROR.with(|inject| inject.replace(false)) {
                    return Err(Error::new_execution("injected internal execution error"));
                }
                safepoint::dispatch_bytecode();
                // Stamp the instruction's start pc so that, if this opcode raises an implicit
                // NullPointerException, the throwable's backtrace records the faulting bytecode index
                // (JEP 358) even though the handler advances `pc` while reading operands.
                let (code, span) = {
                    let frame = stack_frames
                        .last_mut()
                        .ok_or_else(|| Error::new_execution("Error getting stack frame"))?;
                    let pc = frame.pc();
                    frame.set_bci(pc);
                    let same_class = class_name
                        .as_ref()
                        .is_some_and(|name| Arc::ptr_eq(name, frame.current_class_name_ref()));
                    if !same_class {
                        class_name = Some(frame.current_class_name_arc());
                        klass = None;
                    }
                    // Only resolve the source line when someone is listening: this runs for every
                    // bytecode and the lookup is a B-tree search.
                    let span = if tracing::enabled!(Level::TRACE) {
                        let line = StackFramesUtil::extract_line_number(frame.line_numbers(), pc);
                        span!(Level::TRACE, "", "{}:{line}", frame.current_class_name())
                    } else {
                        tracing::Span::none()
                    };
                    (frame.get_bytecode_byte(), span)
                };
                if (178u8..=201u8).contains(&code) && klass.is_none() {
                    klass = Some(Self::current_class(&stack_frames)?);
                }
                let class = class_name
                    .as_deref()
                    .map(String::as_str)
                    .ok_or_else(|| Error::new_execution("Error getting current class name"))?;
                let _entered = span.enter();

                let result = (|| -> Result<()> {
                    match code {
                        0u8..=20u8 => {
                            ops_constant_processor::process(code, class, &mut stack_frames)?;
                        }
                        21u8..=53u8 => {
                            ops_load_processor::process(code, &mut stack_frames)?;
                        }
                        54u8..=86u8 => {
                            ops_store_processor::process(code, &mut stack_frames)?;
                        }
                        87u8..=95u8 => {
                            ops_stack_processor::process(code, &mut stack_frames)?;
                        }
                        96u8..=132u8 => {
                            ops_math_processor::process(code, &mut stack_frames)?;
                        }
                        133u8..=147u8 => {
                            ops_conversion_processor::process(code, &mut stack_frames)?;
                        }
                        148u8..=166u8 => {
                            ops_comparison_processor::process(code, &mut stack_frames)?;
                        }
                        167u8..=177u8 => {
                            ops_control_processor::process(
                                code,
                                &mut stack_frames,
                                &mut destination,
                            )?;
                        }
                        178u8..=195u8 => {
                            let klass = klass
                                .as_ref()
                                .ok_or_else(|| Error::new_execution("Current class unresolved"))?;
                            ops_reference_processor::process(
                                code,
                                class,
                                klass,
                                &mut stack_frames,
                            )?;
                        }
                        196u8..=201u8 => {
                            let klass = klass
                                .as_ref()
                                .ok_or_else(|| Error::new_execution("Current class unresolved"))?;
                            ops_extended_processor::process(code, klass, &mut stack_frames)?;
                        }
                        _ => unreachable!("{}", format! {"xxx = {}", code}),
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    if let Some(throwable) = error.throwable_ref() {
                        crate::vm::exception::common::throw_exception_with_ref(
                            crate::vm::stack::slot::Slot::Ref(throwable),
                            &mut stack_frames,
                        )?;
                    } else {
                        return Err(error);
                    }
                }
            }
            Ok(())
        })();

        if let Err(error) = execution_result {
            Self::release_remaining_frames(&mut stack_frames);
            return Err(error);
        }

        Ok(destination)
    }

    /// The executing frame's class, cached on the frame after the first lookup.
    fn current_class(stack_frames: &StackFrames) -> Result<Arc<JavaClass>> {
        stack_frames
            .last()
            .ok_or_else(|| Error::new_execution("Error getting stack frame"))?
            .current_class()
    }

    fn release_remaining_frames(stack_frames: &mut StackFrames) {
        while !stack_frames.is_empty() {
            stack_frames.propagate_exception();
        }
    }

    fn enter_synchronized_method(stack_frames: &mut StackFrames) -> Result<()> {
        let (is_static, class_name, receiver) = {
            let frame = stack_frames
                .last_mut()
                .ok_or_else(|| Error::new_execution("Error getting stack frame"))?;
            if !frame.needs_synchronized_monitor() {
                return Ok(());
            }
            if frame.synchronized_static() {
                (true, frame.current_class_name().to_owned(), 0)
            } else {
                let receiver = frame.get_local_slot(0);
                if !receiver.is_ref() {
                    return Err(Error::new_execution(
                        "Synchronized instance method has no reference receiver",
                    ));
                }
                (false, String::new(), receiver.value())
            }
        };
        let reference = if is_static {
            CLASSES.class_mirror_ref(&class_name)?
        } else {
            receiver
        };
        if reference == 0 {
            return Err(Error::new_execution(
                "Synchronized method has a null monitor reference",
            ));
        }

        crate::vm::monitor::enter(reference);
        stack_frames
            .last_mut()
            .ok_or_else(|| Error::new_execution("Synchronized method frame disappeared"))?
            .set_synchronized_monitor(reference);
        Ok(())
    }
}
