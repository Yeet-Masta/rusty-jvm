use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::common::last_frame_mut;
use crate::vm::execution_engine::opcode::*;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::method_area::with_method_area;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::{StackFrame, StackFrames};
use crate::vm::stack::stack_value::StackValue;
use std::fmt::Display;
use tracing::trace;

pub(crate) fn process(
    code: u8,
    current_class_name: &str,
    klass: Option<&JavaClass>,
    stack_frames: &mut StackFrames,
) -> Result<()> {
    let stack_frame = last_frame_mut(stack_frames)?;
    match code {
        NOP => {
            stack_frame.incr_pc();
            trace!("NOP");
            Ok(())
        }
        ACONST_NULL => push_constant(stack_frame, Slot::Ref(0), "ACONST_NULL"),
        ICONST_M1 => push_constant(stack_frame, -1i32, "ICONST_M1"),
        ICONST_0 => push_constant(stack_frame, 0i32, "ICONST_0"),
        ICONST_1 => push_constant(stack_frame, 1i32, "ICONST_1"),
        ICONST_2 => push_constant(stack_frame, 2i32, "ICONST_2"),
        ICONST_3 => push_constant(stack_frame, 3i32, "ICONST_3"),
        ICONST_4 => push_constant(stack_frame, 4i32, "ICONST_4"),
        ICONST_5 => push_constant(stack_frame, 5i32, "ICONST_5"),
        LCONST_0 => push_constant(stack_frame, 0i64, "LCONST_0"),
        LCONST_1 => push_constant(stack_frame, 1i64, "LCONST_1"),
        FCONST_0 => push_constant(stack_frame, 0.0f32, "FCONST_0"),
        FCONST_1 => push_constant(stack_frame, 1.0f32, "FCONST_1"),
        FCONST_2 => push_constant(stack_frame, 2.0f32, "FCONST_2"),
        DCONST_0 => push_constant(stack_frame, 0.0f64, "DCONST_0"),
        DCONST_1 => push_constant(stack_frame, 1.0f64, "DCONST_1"),
        BIPUSH => handle_push(
            stack_frame,
            |stack_frame| stack_frame.extract_one_byte() as i8 as i32,
            "BIPUSH",
        ),
        SIPUSH => handle_push(
            stack_frame,
            |stack_frame| stack_frame.extract_two_bytes() as i32,
            "SIPUSH",
        ),
        LDC | LDC_W | LDC2_W => handle_ldc_cases(stack_frame, code, current_class_name, klass),
        _ => Err(Error::new_execution(&format!(
            "Unknown constant opcode: {}",
            code
        ))),
    }
}

fn push_constant<T: StackValue>(stack_frame: &mut StackFrame, value: T, name: &str) -> Result<()> {
    stack_frame.push(value)?;
    stack_frame.incr_pc();
    trace!("{name}");
    Ok(())
}

fn handle_push<T: StackValue + Display + Copy>(
    stack_frame: &mut StackFrame,
    extractor: impl FnOnce(&mut StackFrame) -> T,
    name: &str,
) -> Result<()> {
    let value = extractor(stack_frame);
    stack_frame.push(value)?;
    stack_frame.incr_pc();
    trace!("{name} -> value={value}");
    Ok(())
}

/// `ldc`, `ldc_w` and `ldc2_w`. A constant is resolved once per class and constant pool index;
/// after that it comes straight from the class, tagged in bit 32 when it is a reference.
fn handle_ldc_cases(
    stack_frame: &mut StackFrame,
    code: u8,
    current_class_name: &str,
    klass: Option<&JavaClass>,
) -> Result<()> {
    const REFERENCE: i64 = 1 << 32;
    let cpoolindex = if code == LDC {
        stack_frame.extract_one_byte() as u16
    } else {
        stack_frame.extract_two_bytes() as u16
    };
    let cached = klass.and_then(|klass| klass.cached_constant(cpoolindex));
    if code == LDC2_W {
        let value = match cached {
            Some(value) => value,
            None => {
                let value = with_method_area(|method_area| {
                    method_area.resolve_ldc2_w(current_class_name, cpoolindex)
                })?;
                if let Some(klass) = klass {
                    klass.cache_constant(cpoolindex, value);
                }
                value
            }
        };
        stack_frame.push(value)?;
        trace!("LDC2_W -> cpoolindex={cpoolindex}, value={value}");
    } else {
        let slot = match cached {
            Some(bits) if bits & REFERENCE != 0 => Slot::Ref(bits as i32),
            Some(bits) => Slot::Value(bits as i32),
            None => {
                let slot = with_method_area(|method_area| {
                    method_area.resolve_ldc(current_class_name, cpoolindex)
                })?;
                if let Some(klass) = klass {
                    let tag = if slot.is_ref() { REFERENCE } else { 0 };
                    klass.cache_constant(cpoolindex, tag | i64::from(slot.value() as u32));
                }
                slot
            }
        };
        stack_frame.push(slot)?;
        trace!("LDC -> cpoolindex={cpoolindex}, value={slot}");
    }
    stack_frame.incr_pc();
    Ok(())
}
