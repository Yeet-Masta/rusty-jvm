//! A baseline just-in-time compiler for the frame-local loop.
//!
//! The interpreter's frame-local loop ([`StackFrame::run_frame_local`]) runs the instructions
//! that touch nothing but the executing frame, and stops at the first one that needs its
//! handler: an invoke, a return, or anything else. Once a method has run enough of those
//! instructions, it is compiled into native code that does exactly the same, from the same
//! state, and stops at the same places. The engine then runs the compiled code where it would
//! run the loop; everything around it (calls, returns, exceptions, monitors, safepoints, the
//! collector's root scans) is unchanged.
//!
//! The compiled code works on the frame's own slots, so the frame is always up to date for
//! anything that looks at it once the code has returned. It may stop anywhere: at an
//! instruction it does not compile, it returns, and the interpreter carries on from there. It
//! can be entered at any instruction, so execution goes back into it after a call returns or a
//! handler ran an instruction for it.
//!
//! Arithmetic, conversions, comparisons, branches, switches and moves between locals and the
//! operand stack are compiled to machine instructions. The frame-local instructions that touch
//! the heap or the class's caches (field and array access, `ldc`, `checkcast`, `instanceof`)
//! call back into the frame-local loop for that one instruction.
//!
//! Methods are compiled on a thread of their own, as in HotSpot, and go on being interpreted
//! until their code is ready: compiling takes long enough (a fraction of a millisecond for the
//! smallest method) that a program's start would otherwise wait on it. Nothing depends on when
//! the code arrives, because it does what the interpreter does.
//!
//! `-Xint` turns the compiler off; `-XX:CompileThreshold=<n>` sets how many frame-local
//! instructions a method runs interpreted before it is compiled; `-Xbatch` (or
//! `-XX:-BackgroundCompilation`) has the thread that reaches the threshold wait for the code;
//! `-XX:+PrintCompilation` prints each method as it is compiled, and each that could not be.
//!
//! [`StackFrame::run_frame_local`]: crate::vm::stack::stack_frame::StackFrame

mod compiler;

use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::{FrameCode, StackFrame};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

/// Frame-local instructions a method runs interpreted before it is compiled.
const DEFAULT_COMPILE_THRESHOLD: u32 = 10_000;

static ENABLED: AtomicBool = AtomicBool::new(true);
static PRINT_COMPILATION: AtomicBool = AtomicBool::new(false);
/// Whether methods are compiled on the compiler's thread, rather than by the thread that
/// found them warm.
static BACKGROUND: AtomicBool = AtomicBool::new(true);
static COMPILE_THRESHOLD: AtomicU32 = AtomicU32::new(DEFAULT_COMPILE_THRESHOLD);
/// Methods compiled so far, numbering `-XX:+PrintCompilation`'s lines.
static COMPILED: AtomicU32 = AtomicU32::new(0);

/// Reads the compiler's options: `-Xint` and `-Xbatch` among the `-X` options,
/// `-XX:CompileThreshold=<n>`, `-XX:+PrintCompilation` and `-XX:-BackgroundCompilation` among the
/// `-XX` ones. Others are left for whoever reads them.
///
/// The `RUSTY_JVM_COMPILE_THRESHOLD` environment variable sets the threshold where no option
/// does, and has methods compiled by the thread that reaches it. It is there for the test suite,
/// which runs every program again with the threshold at 1, so that nearly every method runs
/// compiled from its second visit on.
pub(crate) fn configure(jvm_options: &[String], advanced_jvm_options: &[String]) {
    if let Some(threshold) = std::env::var_os("RUSTY_JVM_COMPILE_THRESHOLD")
        .and_then(|value| value.to_str()?.parse::<u32>().ok())
    {
        COMPILE_THRESHOLD.store(threshold.max(1), Ordering::Relaxed);
        BACKGROUND.store(false, Ordering::Relaxed);
    }
    for option in jvm_options {
        match option.as_str() {
            "-Xint" => ENABLED.store(false, Ordering::Relaxed),
            "-Xbatch" => BACKGROUND.store(false, Ordering::Relaxed),
            _ => {}
        }
    }
    for option in advanced_jvm_options {
        if let Some(value) = option.strip_prefix("-XX:CompileThreshold=") {
            if let Ok(threshold) = value.parse::<u32>() {
                COMPILE_THRESHOLD.store(threshold.max(1), Ordering::Relaxed);
            }
        } else if option == "-XX:+PrintCompilation" {
            PRINT_COMPILATION.store(true, Ordering::Relaxed);
        } else if option == "-XX:-PrintCompilation" {
            PRINT_COMPILATION.store(false, Ordering::Relaxed);
        } else if option == "-XX:+BackgroundCompilation" {
            BACKGROUND.store(true, Ordering::Relaxed);
        } else if option == "-XX:-BackgroundCompilation" {
            BACKGROUND.store(false, Ordering::Relaxed);
        }
    }
}

const UNTRIED: u8 = 0;
const COMPILING: u8 = 1;
const READY: u8 = 2;
const FAILED: u8 = 3;

/// A method's compiler state, kept with the rest of its per-method frame metadata.
#[derive(Debug, Default)]
pub(crate) struct JitState {
    /// Frame-local instructions the method has run interpreted, until it is compiled.
    interpreted: AtomicU32,
    status: AtomicU8,
    /// The compiled code, once `status` is `READY`.
    entry: AtomicPtr<u8>,
}

/// What the compiled code reads on entry and writes back on return. The layout is shared with
/// the code the compiler emits, which addresses the fields by offset.
#[repr(C)]
pub(crate) struct JitIo {
    /// The frame's slots: its locals, then its operand stack.
    pub(crate) slots: *mut Slot,
    /// The frame, for the instructions the compiled code hands back to the frame-local loop.
    pub(crate) frame: *mut StackFrame,
    pub(crate) pc: u32,
    /// The index in `slots` one past the top of the operand stack.
    pub(crate) top: u32,
    /// How many slots there are.
    pub(crate) limit: u32,
    /// At most how many instructions to run (checked at backward branches).
    pub(crate) budget: u32,
    /// How many instructions ran.
    pub(crate) done: u32,
    /// Why the code stopped: one of the `STOP_*` values.
    pub(crate) stop: u32,
}

/// The next instruction is not one the code runs (or the budget ran out).
pub(crate) const STOP_OTHER: u32 = 0;
/// The next instruction is an invoke.
pub(crate) const STOP_INVOKE: u32 = 1;
/// The next instruction is a return of `STOP_RETURN + n` result slots, `n` from 0 to 2.
pub(crate) const STOP_RETURN: u32 = 2;

/// The compiled code's signature.
pub(crate) type Entry = unsafe extern "C" fn(*mut JitIo);

/// The function the compiled code calls to have the frame-local loop run one instruction: the
/// frame's pc and operand stack top are in `io`, and so are they afterwards, with `stop` when
/// the instruction did not run. Returns how many instructions ran (0 or 1).
pub(crate) type Step = unsafe extern "C" fn(*mut JitIo) -> u32;

/// The method's compiled code, if it has been compiled.
#[inline]
pub(crate) fn entry(code: &FrameCode) -> Option<Entry> {
    let state = &code.jit;
    if state.status.load(Ordering::Acquire) != READY {
        return None;
    }
    let pointer = state.entry.load(Ordering::Relaxed);
    // SAFETY: `entry` holds a function the compiler finalized with the `Entry` signature, and
    // compiled code is never freed.
    Some(unsafe { std::mem::transmute::<*mut u8, Entry>(pointer) })
}

/// Records that the method ran `ran` frame-local instructions interpreted, and compiles it once
/// they reach the threshold.
#[inline]
pub(crate) fn note_interpreted(code: &'static FrameCode, ran: u32, step: Step) {
    if ran == 0 || !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let state = &code.jit;
    let before = state.interpreted.fetch_add(ran, Ordering::Relaxed);
    let threshold = COMPILE_THRESHOLD.load(Ordering::Relaxed);
    if before < threshold && before.saturating_add(ran) >= threshold {
        request(code, step);
    }
}

/// The methods waiting for the compiler's thread, once there is one.
static QUEUE: OnceLock<Option<Sender<&'static FrameCode>>> = OnceLock::new();

/// Has the method compiled: by the compiler's thread, or here and now when compilation is not
/// in the background (or the thread could not be started).
#[cold]
fn request(code: &'static FrameCode, step: Step) {
    if code
        .jit
        .status
        .compare_exchange(UNTRIED, COMPILING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    if BACKGROUND.load(Ordering::Relaxed) {
        let queue = QUEUE.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<&'static FrameCode>();
            std::thread::Builder::new()
                .name("JIT Compiler".to_string())
                .spawn(move || {
                    for code in receiver {
                        compile(code, step);
                    }
                })
                .ok()
                .map(|_| sender)
        });
        if let Some(queue) = queue {
            if queue.send(code).is_ok() {
                return;
            }
        }
    }
    compile(code, step);
}

struct Compiler(compiler::Compiler);

// SAFETY: the compiler's module holds raw pointers into the code memory it owns; it is only
// used under the `COMPILER` lock.
unsafe impl Send for Compiler {}

static COMPILER: Mutex<Option<Compiler>> = Mutex::new(None);

/// Compiles a method whose state `request` set to `COMPILING`.
fn compile(code: &'static FrameCode, step: Step) {
    let state = &code.jit;
    let mut guard = COMPILER.lock();
    let compiler = match guard.as_mut() {
        Some(compiler) => compiler,
        None => match compiler::Compiler::new(step) {
            Ok(compiler) => guard.insert(Compiler(compiler)),
            Err(error) => {
                // No compiler for this machine: interpret everything.
                ENABLED.store(false, Ordering::Relaxed);
                state.status.store(FAILED, Ordering::Release);
                if PRINT_COMPILATION.load(Ordering::Relaxed) {
                    println!("JIT unavailable: {error}");
                }
                return;
            }
        },
    };
    let result = compiler.0.compile(&code.bytecode);
    drop(guard);
    let print = PRINT_COMPILATION.load(Ordering::Relaxed);
    match result {
        Ok(pointer) => {
            state.entry.store(pointer as *mut u8, Ordering::Relaxed);
            state.status.store(READY, Ordering::Release);
            if print {
                let id = COMPILED.fetch_add(1, Ordering::Relaxed) + 1;
                println!(
                    "{id:6}       {}::{} ({} bytes)",
                    code.class_name.replace('/', "."),
                    code.method_name.split(':').next().unwrap_or_default(),
                    code.bytecode.len()
                );
            }
        }
        Err(reason) => {
            state.status.store(FAILED, Ordering::Release);
            if print {
                println!(
                    "              {}::{} not compiled: {reason}",
                    code.class_name.replace('/', "."),
                    code.method_name.split(':').next().unwrap_or_default(),
                );
            }
        }
    }
}
