//! Library methods the interpreter runs natively once their classes are ready.
//!
//! A call to one of these methods, made after the classes it reads are initialized and its
//! receiver is a plain object, is done here on the caller's operand stack, as the method's
//! bytecode would do it, without a frame. Anything else (a class not initialized yet, a field not
//! found) leaves the call, untouched, to the method itself.

use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::StackFrames;
use std::sync::{Arc, OnceLock};

const STRING: &str = "java/lang/String";
const INTEGER: &str = "java/lang/Integer";
const INTEGER_CACHE: &str = "java/lang/Integer$IntegerCache";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intrinsic {
    /// `String.hashCode()`
    StringHashCode,
    /// `String.length()`
    StringLength,
    /// `Integer.valueOf(int)`
    IntegerValueOf,
}

/// The intrinsic for a method, by its class and `name:descriptor`.
pub(crate) fn lookup(class_name: &str, name_signature: &str) -> Option<Intrinsic> {
    match (class_name, name_signature) {
        (STRING, "hashCode:()I") => Some(Intrinsic::StringHashCode),
        (STRING, "length:()I") => Some(Intrinsic::StringLength),
        (INTEGER, "valueOf:(I)Ljava/lang/Integer;") => Some(Intrinsic::IntegerValueOf),
        _ => None,
    }
}

/// Runs `intrinsic` on the top frame's operand stack, which holds the call's arguments. Returns
/// `false`, having changed nothing, when the method has to run instead.
pub(crate) fn run(stack_frames: &mut StackFrames, intrinsic: Intrinsic) -> Result<bool> {
    match intrinsic {
        Intrinsic::StringHashCode => string_hash_code(stack_frames),
        Intrinsic::StringLength => string_length(stack_frames),
        Intrinsic::IntegerValueOf => integer_value_of(stack_frames),
    }
}

/// Where `String`'s fields are, found from the first `String` an intrinsic sees. The layout is
/// the class's, so every `String` shares it.
struct StringFields {
    value: usize,
    coder: usize,
    hash: usize,
    hash_is_zero: usize,
}

static STRING_FIELDS: OnceLock<Option<StringFields>> = OnceLock::new();

fn string_fields(string: i32) -> Option<&'static StringFields> {
    STRING_FIELDS
        .get_or_init(|| {
            let at = |name| match HEAP.locate_field(string, STRING, name) {
                Some((offset, 1)) => Some(offset),
                _ => None,
            };
            Some(StringFields {
                value: at("value")?,
                coder: at("coder")?,
                hash: at("hash")?,
                hash_is_zero: at("hashIsZero")?,
            })
        })
        .as_ref()
}

/// The receiver on top of the stack, if it is a non-null `String` whose fields are known.
fn string_receiver(stack_frames: &StackFrames) -> Option<(i32, &'static StringFields)> {
    let frame = stack_frames.last()?;
    let receiver = frame.top_slots(1).ok()?[0].value();
    if receiver == 0 {
        return None;
    }
    Some((receiver, string_fields(receiver)?))
}

fn read(object: i32, offset: usize) -> Option<i32> {
    HEAP.read_field_at(object, offset, 1).map(|words| words[0])
}

/// Replaces the top `count` slots of the top frame with `result`, ending the call as a return
/// would.
fn finish(stack_frames: &mut StackFrames, count: usize, result: Slot) -> Result<bool> {
    let Some(frame) = stack_frames.last_mut() else {
        return Ok(false);
    };
    frame.drop_top_slots(count);
    frame.push(result)?;
    frame.reset_ex_pc();
    Ok(true)
}

fn string_length(stack_frames: &mut StackFrames) -> Result<bool> {
    let Some((string, fields)) = string_receiver(stack_frames) else {
        return Ok(false);
    };
    let (Some(value), Some(coder)) = (read(string, fields.value), read(string, fields.coder))
    else {
        return Ok(false);
    };
    if value == 0 || !(0..=1).contains(&coder) {
        return Ok(false);
    }
    let length = HEAP.get_array_len(value)? >> coder;
    finish(stack_frames, 1, Slot::Value(length))
}

fn string_hash_code(stack_frames: &mut StackFrames) -> Result<bool> {
    let Some((string, fields)) = string_receiver(stack_frames) else {
        return Ok(false);
    };
    let Some(hash) = read(string, fields.hash) else {
        return Ok(false);
    };
    if hash != 0 {
        return finish(stack_frames, 1, Slot::Value(hash));
    }
    let (Some(hash_is_zero), Some(value), Some(coder)) = (
        read(string, fields.hash_is_zero),
        read(string, fields.value),
        read(string, fields.coder),
    ) else {
        return Ok(false);
    };
    if hash_is_zero != 0 {
        return finish(stack_frames, 1, Slot::Value(0));
    }
    if value == 0 {
        return Ok(false);
    }
    let hash = {
        let bytes = HEAP.get_entire_raw_data(value)?;
        match coder {
            // LATIN1: one byte per char.
            0 => bytes
                .iter()
                .fold(0i32, |h, &b| h.wrapping_mul(31).wrapping_add(i32::from(b))),
            // UTF16: two bytes per char, in the platform's byte order, as `StringUTF16` stores
            // them.
            1 => bytes.chunks_exact(2).fold(0i32, |h, pair| {
                let char = u16::from_ne_bytes([pair[0], pair[1]]);
                h.wrapping_mul(31).wrapping_add(i32::from(char))
            }),
            _ => return Ok(false),
        }
    };
    // As `String.hashCode` caches it.
    let field = if hash == 0 {
        (fields.hash_is_zero, 1)
    } else {
        (fields.hash, hash)
    };
    if HEAP
        .write_field_at(string, field.0, 1, [field.1, 0])
        .is_none()
    {
        return Ok(false);
    }
    finish(stack_frames, 1, Slot::Value(hash))
}

/// What `Integer.valueOf` reads, once `Integer.IntegerCache` is initialized: its bounds and
/// cache array never change after that.
struct Integers {
    low: i32,
    high: i32,
    cache: i32,
    integer: Arc<JavaClass>,
    integer_id: usize,
    value: usize,
}

/// `None` once `Integer.IntegerCache` turned out not to be laid out as this expects; the method
/// then always runs.
static INTEGERS: OnceLock<Option<Integers>> = OnceLock::new();

fn integers() -> Result<Option<&'static Integers>> {
    if let Some(integers) = INTEGERS.get() {
        return Ok(integers.as_ref());
    }
    // Until both classes are initialized, the method runs (and initializes them).
    let Some(cache_class) = CLASSES.get_loaded(INTEGER_CACHE) else {
        return Ok(None);
    };
    if !cache_class.static_fields_init_state().is_initialized() {
        return Ok(None);
    }
    let (integer_id, _, integer) = CLASSES.get_full(INTEGER)?;
    if !integer.static_fields_init_state().is_initialized() {
        return Ok(None);
    }
    let found = describe_integers(&cache_class, integer, integer_id)?;
    Ok(INTEGERS.get_or_init(|| found).as_ref())
}

fn describe_integers(
    cache_class: &JavaClass,
    integer: Arc<JavaClass>,
    integer_id: usize,
) -> Result<Option<Integers>> {
    let field = |name| -> Result<Option<i32>> {
        Ok(match cache_class.static_field(name) {
            Some(value) => Some(value.words()?[0]),
            None => None,
        })
    };
    // `low` is the compile-time constant -128 in every JDK, which `javac` inlines into its
    // readers: its field need not hold it.
    let low = -128;
    let (Some(high), Some(cache)) = (field("high")?, field("cache")?) else {
        return Ok(None);
    };
    // The bounds must describe the array, or the cache is not what this reads it as.
    if cache == 0 || i64::from(HEAP.get_array_len(cache)?) != i64::from(high) - i64::from(low) + 1 {
        return Ok(None);
    }
    // The `value` field's position, from an instance's layout.
    let plan = integer.new_instance(integer_id)?;
    let mut destination = RootedSlots::new();
    let probe = HEAP.create_instance(plan, &mut destination)?;
    let Some((value, 1)) = HEAP.locate_field(probe, INTEGER, "value") else {
        return Ok(None);
    };
    Ok(Some(Integers {
        low,
        high,
        cache,
        integer,
        integer_id,
        value,
    }))
}

fn integer_value_of(stack_frames: &mut StackFrames) -> Result<bool> {
    let Some(integers) = integers()? else {
        return Ok(false);
    };
    let Some(frame) = stack_frames.last() else {
        return Ok(false);
    };
    let i = frame.top_slots(1)?[0].value();
    if i >= integers.low && i <= integers.high {
        let index = i.wrapping_sub(integers.low);
        return match HEAP.load_array_element(integers.cache, index)? {
            Ok(words) if words[0] != 0 => finish(stack_frames, 1, Slot::Ref(words[0])),
            _ => Ok(false),
        };
    }
    let plan = integers.integer.new_instance(integers.integer_id)?;
    let mut destination = RootedSlots::new();
    let boxed = HEAP.create_instance(plan, &mut destination)?;
    if HEAP
        .write_field_at(boxed, integers.value, 1, [i, 0])
        .is_none()
    {
        return Ok(false);
    }
    finish(stack_frames, 1, Slot::Ref(boxed))
}
