//! Calls and returns between frames without the interpreter loop.
//!
//! The frame-local loop stops at every invoke and return. Most of them are warm: the site already
//! resolved its target, the target is interpreted and holds no monitor, and the result goes to a
//! caller in this segment. Such a call or return is done here, as its handler would do it, and the
//! frame-local loop goes on in the callee or the caller. Anything else (a first call, a native or
//! synchronized target, a null receiver, a return to native code) is left, untouched, for the
//! handler.

use crate::vm::error::Result;
use crate::vm::execution_engine::invoker::invoke_interpreted;
use crate::vm::execution_engine::ops_control_processor::return_to_caller;
use crate::vm::execution_engine::ops_reference_processor::cached_virtual_target;
use crate::vm::heap::heap::HEAP;
use crate::vm::method_area::java_class::MemberRefKind;
use crate::vm::method_area::java_method::{immortal, JavaMethod};
use crate::vm::stack::stack_frame::{FrameLocalStop, StackFrames};
use crate::vm::stack::stack_limit;

const INVOKEVIRTUAL: u8 = 182;
const INVOKESPECIAL: u8 = 183;
const INVOKESTATIC: u8 = 184;
const INVOKEINTERFACE: u8 = 185;

/// Runs frame-local instructions, and the warm calls and returns between them, until `budget`
/// instructions ran or the next one needs its handler. Returns how many ran.
pub(crate) fn run(stack_frames: &mut StackFrames, budget: u32) -> Result<u32> {
    let mut done = 0u32;
    while done < budget {
        let Some(frame) = stack_frames.last_mut() else {
            break;
        };
        let (ran, stop) = frame.run_local(budget - done);
        done += ran;
        let handled = match stop {
            FrameLocalStop::Other => false,
            FrameLocalStop::Invoke => invoke(stack_frames)?,
            FrameLocalStop::Return(width) => return_to_caller(stack_frames, width)?,
        };
        if !handled {
            break;
        }
        done += 1;
    }
    Ok(done)
}

/// The invoke at the top frame's pc, if its site already resolved a target that runs without the
/// handler's help. Returns `false`, having changed nothing, otherwise.
fn invoke(stack_frames: &mut StackFrames) -> Result<bool> {
    let Some((target, length)) = warm_target(stack_frames)? else {
        return Ok(false);
    };
    // A synchronized target needs its monitor entered by the interpreter loop before it runs; a
    // native or signature polymorphic one needs the handler's argument marshalling.
    if target.is_native() || target.is_synchronized() || target.is_polymorphic_signature() {
        return Ok(false);
    }
    // Leave the StackOverflowError to the handler, which throws it at the right pc.
    if stack_limit::would_overflow(stack_frames) {
        return Ok(false);
    }
    // As the handler does before calling: the invoke is the frame's current instruction and its
    // throw site while the callee runs, and the frame resumes after it.
    let Some(frame) = stack_frames.last_mut() else {
        return Ok(false);
    };
    let pc = frame.pc();
    frame.set_bci(pc);
    frame.store_ex_pc();
    frame.advance_pc(length);
    invoke_interpreted(stack_frames, target)
}

/// The target and instruction length of the invoke at the top frame's pc, if its site already
/// resolved one for this call.
fn warm_target(stack_frames: &StackFrames) -> Result<Option<(&'static JavaMethod, i16)>> {
    let Some(frame) = stack_frames.last() else {
        return Ok(None);
    };
    let Some(klass) = frame.resolved_class() else {
        return Ok(None);
    };
    let code = frame.code_bytes();
    let pc = frame.pc();
    let (Some(&opcode), Some(&high), Some(&low)) =
        (code.get(pc), code.get(pc + 1), code.get(pc + 2))
    else {
        return Ok(None);
    };
    let index = u16::from_be_bytes([high, low]);
    let (kind, length) = if opcode == INVOKEINTERFACE {
        (MemberRefKind::InterfaceMethod, 5)
    } else {
        (MemberRefKind::Method, 3)
    };
    let Some(member) = klass.resolved_member_ref(kind, index) else {
        return Ok(None);
    };
    let target = match opcode {
        INVOKESTATIC => {
            let Some((holder, method)) = member.static_target.get() else {
                return Ok(None);
            };
            // The handler initializes the class on every call; once initialized that is a no-op,
            // and anything else (in progress, failed) is for the handler.
            if !holder.static_fields_init_state().is_initialized() {
                return Ok(None);
            }
            immortal(method)
        }
        INVOKESPECIAL => match member.special_target.get() {
            Some(method) => immortal(method),
            None => return Ok(None),
        },
        INVOKEVIRTUAL | INVOKEINTERFACE => {
            let slots = if opcode == INVOKEINTERFACE {
                match code.get(pc + 3) {
                    Some(&count) => usize::from(count),
                    None => return Ok(None),
                }
            } else {
                member.arg_slots()? + 1
            };
            // A null receiver throws in the handler.
            let receiver = match frame.top_slots(slots) {
                Ok(args) => args.first().map_or(0, |slot| slot.value()),
                Err(_) => return Ok(None),
            };
            if receiver == 0 {
                return Ok(None);
            }
            let Ok(Some(klass_id)) = HEAP.instance_klass_id(receiver) else {
                return Ok(None);
            };
            match cached_virtual_target(member, klass_id) {
                Some(method) => method,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some((target, length)))
}
