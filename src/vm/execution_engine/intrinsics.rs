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
use crate::vm::stack::stack_value::StackValue;
use std::sync::{Arc, OnceLock};

const STRING: &str = "java/lang/String";
const INTEGER: &str = "java/lang/Integer";
const INTEGER_CACHE: &str = "java/lang/Integer$IntegerCache";
const LONG: &str = "java/lang/Long";
const MATH: &str = "java/lang/Math";
const STRICT_MATH: &str = "java/lang/StrictMath";
const FDLIBM_SQRT: &str = "java/lang/FdLibm$Sqrt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intrinsic {
    /// `String.hashCode()`
    StringHashCode,
    /// `String.length()`
    StringLength,
    /// `Integer.valueOf(int)`
    IntegerValueOf,
    /// A pure arithmetic method on primitives.
    Arithmetic(Arithmetic),
}

/// Methods of `Math`, `StrictMath`, `Integer` and `Long` that compute a primitive from primitives
/// and nothing else. Each is done here exactly as the Java code does it, bit for bit (so only
/// methods whose results IEEE 754 or the JLS fix exactly are here, not `sin` or `exp`). They are
/// the ones whose bytecode is slow next to the operation itself: `StrictMath.sqrt` takes the
/// square root one bit at a time, `Math.fma` goes through `BigDecimal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arithmetic {
    SqrtD,
    AbsI,
    AbsJ,
    AbsF,
    AbsD,
    MinI,
    MinJ,
    MinF,
    MinD,
    MaxI,
    MaxJ,
    MaxF,
    MaxD,
    FmaF,
    FmaD,
    FloorD,
    CeilD,
    RintD,
    RoundF,
    RoundD,
    NumberOfLeadingZerosI,
    NumberOfLeadingZerosJ,
    NumberOfTrailingZerosI,
    NumberOfTrailingZerosJ,
    BitCountI,
    BitCountJ,
}

fn math_method(name_signature: &str) -> Option<Arithmetic> {
    use Arithmetic::*;
    Some(match name_signature {
        "sqrt:(D)D" => SqrtD,
        "abs:(I)I" => AbsI,
        "abs:(J)J" => AbsJ,
        "abs:(F)F" => AbsF,
        "abs:(D)D" => AbsD,
        "min:(II)I" => MinI,
        "min:(JJ)J" => MinJ,
        "min:(FF)F" => MinF,
        "min:(DD)D" => MinD,
        "max:(II)I" => MaxI,
        "max:(JJ)J" => MaxJ,
        "max:(FF)F" => MaxF,
        "max:(DD)D" => MaxD,
        "fma:(FFF)F" => FmaF,
        "fma:(DDD)D" => FmaD,
        "floor:(D)D" => FloorD,
        "ceil:(D)D" => CeilD,
        "rint:(D)D" => RintD,
        "round:(F)I" => RoundF,
        "round:(D)J" => RoundD,
        _ => return None,
    })
}

/// The intrinsic for a method, by its class and `name:descriptor`.
pub(crate) fn lookup(class_name: &str, name_signature: &str) -> Option<Intrinsic> {
    match (class_name, name_signature) {
        (STRING, "hashCode:()I") => Some(Intrinsic::StringHashCode),
        (STRING, "length:()I") => Some(Intrinsic::StringLength),
        (INTEGER, "valueOf:(I)Ljava/lang/Integer;") => Some(Intrinsic::IntegerValueOf),
        (INTEGER, "numberOfLeadingZeros:(I)I") => {
            Some(Intrinsic::Arithmetic(Arithmetic::NumberOfLeadingZerosI))
        }
        (INTEGER, "numberOfTrailingZeros:(I)I") => {
            Some(Intrinsic::Arithmetic(Arithmetic::NumberOfTrailingZerosI))
        }
        (INTEGER, "bitCount:(I)I") => Some(Intrinsic::Arithmetic(Arithmetic::BitCountI)),
        (LONG, "numberOfLeadingZeros:(J)I") => {
            Some(Intrinsic::Arithmetic(Arithmetic::NumberOfLeadingZerosJ))
        }
        (LONG, "numberOfTrailingZeros:(J)I") => {
            Some(Intrinsic::Arithmetic(Arithmetic::NumberOfTrailingZerosJ))
        }
        (LONG, "bitCount:(J)I") => Some(Intrinsic::Arithmetic(Arithmetic::BitCountJ)),
        (FDLIBM_SQRT, "compute:(D)D") => Some(Intrinsic::Arithmetic(Arithmetic::SqrtD)),
        (MATH | STRICT_MATH, _) => math_method(name_signature).map(Intrinsic::Arithmetic),
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
        Intrinsic::Arithmetic(op) => arithmetic(stack_frames, op),
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
fn finish<T: StackValue>(stack_frames: &mut StackFrames, count: usize, result: T) -> Result<bool> {
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
    if cache == 0 || i64::from(HEAP.get_array_len(cache)?) != i64::from(high) - i64::from(low) + 1
    {
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

/// The `int` or `float` bits in `slots[i]`.
fn word_at(slots: &[Slot], i: usize) -> i32 {
    slots[i].value()
}

/// The `long` or `double` bits in `slots[i]` (low word) and `slots[i + 1]` (high word), as
/// [`StackValue`] pushes them.
fn wide_at(slots: &[Slot], i: usize) -> i64 {
    (i64::from(slots[i + 1].value()) << 32) | i64::from(slots[i].value() as u32)
}

fn float_at(slots: &[Slot], i: usize) -> f32 {
    f32::from_bits(word_at(slots, i) as u32)
}

fn double_at(slots: &[Slot], i: usize) -> f64 {
    f64::from_bits(wide_at(slots, i) as u64)
}

/// `Math.min(float, float)` and `Math.min(double, double)`: a NaN operand is the result (the
/// first one if both are), and -0.0 is less than 0.0.
macro_rules! java_min {
    ($a:expr, $b:expr) => {{
        let (a, b) = ($a, $b);
        if a.is_nan() {
            a
        } else if b.is_nan() {
            b
        } else if a == 0.0 && b == 0.0 {
            if a.is_sign_negative() {
                a
            } else {
                b
            }
        } else if a <= b {
            a
        } else {
            b
        }
    }};
}

/// `Math.max(float, float)` and `Math.max(double, double)`: a NaN operand is the result (the
/// first one if both are), and 0.0 is greater than -0.0.
macro_rules! java_max {
    ($a:expr, $b:expr) => {{
        let (a, b) = ($a, $b);
        if a.is_nan() {
            a
        } else if b.is_nan() {
            b
        } else if a == 0.0 && b == 0.0 {
            if a.is_sign_negative() {
                b
            } else {
                a
            }
        } else if a >= b {
            a
        } else {
            b
        }
    }};
}

/// `Math.round`: the closest integer, ties towards positive infinity, NaN as 0 and values out of
/// range as the type's bound (which an `as` cast does). `x - floor(x)` is exact, so this does not
/// round `0.49999999999999994` up, as `floor(x + 0.5)` would.
macro_rules! java_round {
    ($x:expr, $int:ty) => {{
        let x = $x;
        let floor = x.floor();
        (if x - floor >= 0.5 { floor + 1.0 } else { floor }) as $int
    }};
}

fn arithmetic(stack_frames: &mut StackFrames, op: Arithmetic) -> Result<bool> {
    use Arithmetic::*;
    let Some(frame) = stack_frames.last() else {
        return Ok(false);
    };
    let count = match op {
        AbsI | AbsF | RoundF | NumberOfLeadingZerosI | NumberOfTrailingZerosI | BitCountI => 1,
        SqrtD
        | AbsJ
        | AbsD
        | MinI
        | MaxI
        | MinF
        | MaxF
        | FloorD
        | CeilD
        | RintD
        | RoundD
        | NumberOfLeadingZerosJ
        | NumberOfTrailingZerosJ
        | BitCountJ => 2,
        FmaF => 3,
        MinJ | MaxJ | MinD | MaxD => 4,
        FmaD => 6,
    };
    let s = frame.top_slots(count)?;
    let result = match op {
        SqrtD => Ret::from(double_at(s, 0).sqrt()),
        AbsI => Ret::from(word_at(s, 0).wrapping_abs()),
        AbsJ => Ret::from(wide_at(s, 0).wrapping_abs()),
        AbsF => Ret::from(float_at(s, 0).abs()),
        AbsD => Ret::from(double_at(s, 0).abs()),
        MinI => Ret::from(word_at(s, 0).min(word_at(s, 1))),
        MaxI => Ret::from(word_at(s, 0).max(word_at(s, 1))),
        MinJ => Ret::from(wide_at(s, 0).min(wide_at(s, 2))),
        MaxJ => Ret::from(wide_at(s, 0).max(wide_at(s, 2))),
        MinF => Ret::from(java_min!(float_at(s, 0), float_at(s, 1))),
        MaxF => Ret::from(java_max!(float_at(s, 0), float_at(s, 1))),
        MinD => Ret::from(java_min!(double_at(s, 0), double_at(s, 2))),
        MaxD => Ret::from(java_max!(double_at(s, 0), double_at(s, 2))),
        // HotSpot runs `Math.fma` as the hardware instruction, interpreter included, and so
        // does this: a NaN operand passes through, where the Java code would return the
        // canonical NaN.
        FmaF => Ret::from(float_at(s, 0).mul_add(float_at(s, 1), float_at(s, 2))),
        FmaD => Ret::from(double_at(s, 0).mul_add(double_at(s, 2), double_at(s, 4))),
        // The Java code returns a NaN operand as it is, where the hardware would quiet a
        // signaling one; such a call runs the method.
        FloorD | CeilD | RintD if double_at(s, 0).is_nan() => return Ok(false),
        FloorD => Ret::from(double_at(s, 0).floor()),
        CeilD => Ret::from(double_at(s, 0).ceil()),
        RintD => Ret::from(double_at(s, 0).round_ties_even()),
        RoundF => Ret::from(java_round!(float_at(s, 0), i32)),
        RoundD => Ret::from(java_round!(double_at(s, 0), i64)),
        NumberOfLeadingZerosI => Ret::from(word_at(s, 0).leading_zeros() as i32),
        NumberOfLeadingZerosJ => Ret::from(wide_at(s, 0).leading_zeros() as i32),
        NumberOfTrailingZerosI => Ret::from(word_at(s, 0).trailing_zeros() as i32),
        NumberOfTrailingZerosJ => Ret::from(wide_at(s, 0).trailing_zeros() as i32),
        BitCountI => Ret::from(word_at(s, 0).count_ones() as i32),
        BitCountJ => Ret::from(wide_at(s, 0).count_ones() as i32),
    };
    match result {
        Ret::Int(v) => finish(stack_frames, count, v),
        Ret::Long(v) => finish(stack_frames, count, v),
        Ret::Float(v) => finish(stack_frames, count, v),
        Ret::Double(v) => finish(stack_frames, count, v),
    }
}

/// An arithmetic intrinsic's result, taken before the frame it was computed from is changed.
enum Ret {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}

macro_rules! ret_from {
    ($($ty:ty => $variant:ident),*) => {
        $(impl From<$ty> for Ret {
            fn from(value: $ty) -> Self {
                Ret::$variant(value)
            }
        })*
    };
}

ret_from!(i32 => Int, i64 => Long, f32 => Float, f64 => Double);
