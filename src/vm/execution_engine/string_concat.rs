//! `StringConcatFactory.makeConcatWithConstants` call sites, run natively.
//!
//! javac compiles `"v" + i` to an `invokedynamic` whose bootstrap builds a method-handle tree that
//! formats and joins the arguments. Interpreting that tree costs tens of microseconds per
//! concatenation. When every dynamic argument is a primitive other than `float` and `double`, or a
//! `String`, its text is fixed by the language (`String.valueOf`), so such a site is linked to a
//! recipe instead, and each call builds the result string directly. Any other site keeps the
//! bootstrap's method handle.

use crate::vm::error::Result;
use crate::vm::execution_engine::common::last_frame_mut;
use crate::vm::execution_engine::ldc_resolution_manager::LdcConstant;
use crate::vm::heap::heap::HEAP;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::StackFrames;
use crate::vm::system_native::string::{compact_strings, get_raw_string_info};
use jdescriptor::{MethodDescriptor, TypeDescriptor};

const STRING: &str = "java/lang/String";
/// Marks a dynamic argument in a recipe.
const TAG_ARG: u16 = 1;
/// Marks a constant from the bootstrap arguments in a recipe.
const TAG_CONST: u16 = 2;

/// How a dynamic argument becomes text.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Arg {
    /// `int`, `short` or `byte`: one slot, in decimal.
    Int,
    /// `long`: two slots, in decimal.
    Long,
    Char,
    Boolean,
    /// A `String` reference, `"null"` for null.
    String,
}

impl Arg {
    fn of(descriptor: &TypeDescriptor) -> Option<Self> {
        Some(match descriptor {
            TypeDescriptor::Integer | TypeDescriptor::Short | TypeDescriptor::Byte => Arg::Int,
            TypeDescriptor::Long => Arg::Long,
            TypeDescriptor::Char => Arg::Char,
            TypeDescriptor::Boolean => Arg::Boolean,
            TypeDescriptor::Object(name) if name == STRING => Arg::String,
            _ => return None,
        })
    }

    fn slots(self) -> usize {
        if self == Arg::Long {
            2
        } else {
            1
        }
    }
}

#[derive(Debug)]
enum Part {
    Literal(Vec<u16>),
    /// The dynamic argument at this index.
    Arg(usize),
}

/// A linked concatenation site: what to emit, in order.
#[derive(Debug)]
pub(crate) struct ConcatRecipe {
    parts: Vec<Part>,
    args: Vec<Arg>,
    /// Operand-stack slots the arguments take.
    slots: usize,
}

impl ConcatRecipe {
    /// A recipe for a `makeConcatWithConstants` site, or `None` if the site needs its method
    /// handle: an argument type this does not format, or a constant other than a string or an
    /// `int`.
    pub(crate) fn link(
        descriptor: &MethodDescriptor,
        bootstrap_args: &[LdcConstant],
    ) -> Result<Option<Self>> {
        if !matches!(descriptor.return_type(), TypeDescriptor::Object(name) if name == STRING) {
            return Ok(None);
        }
        let Some(args) = descriptor
            .parameter_types()
            .iter()
            .map(Arg::of)
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        let Some((LdcConstant::Reference(recipe_ref), constants)) = bootstrap_args.split_first()
        else {
            return Ok(None);
        };
        let recipe = utf16_of(*recipe_ref)?;

        let mut parts = Vec::new();
        let mut literal = Vec::new();
        let mut next_arg = 0;
        let mut constants = constants.iter();
        for unit in recipe {
            match unit {
                TAG_ARG => {
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    if next_arg >= args.len() {
                        return Ok(None);
                    }
                    parts.push(Part::Arg(next_arg));
                    next_arg += 1;
                }
                TAG_CONST => match constants.next() {
                    Some(LdcConstant::Reference(string)) if is_string(*string)? => {
                        literal.extend(utf16_of(*string)?);
                    }
                    Some(LdcConstant::Int(value)) => {
                        literal.extend(value.to_string().encode_utf16());
                    }
                    _ => return Ok(None),
                },
                unit => literal.push(unit),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        if next_arg != args.len() {
            return Ok(None);
        }
        let slots = args.iter().map(|arg| arg.slots()).sum();
        Ok(Some(Self { parts, args, slots }))
    }

    /// Takes the site's arguments off the operand stack and pushes the joined string.
    pub(crate) fn run(&self, stack_frames: &mut StackFrames) -> Result<()> {
        let mut text = Vec::new();
        {
            let frame = last_frame_mut(stack_frames)?;
            let slots = frame.top_slots(self.slots)?;
            // Where each argument starts among the slots.
            let mut starts = Vec::with_capacity(self.args.len());
            let mut at = 0;
            for arg in &self.args {
                starts.push(at);
                at += arg.slots();
            }
            for part in &self.parts {
                match part {
                    Part::Literal(units) => text.extend_from_slice(units),
                    Part::Arg(index) => {
                        let at = starts[*index];
                        let word = slots[at].value();
                        match self.args[*index] {
                            Arg::Int => text.extend(word.to_string().encode_utf16()),
                            Arg::Long => {
                                let value = (i64::from(slots[at + 1].value()) << 32)
                                    | i64::from(word as u32);
                                text.extend(value.to_string().encode_utf16());
                            }
                            Arg::Char => text.push(word as u16),
                            Arg::Boolean => text
                                .extend(if word != 0 { "true" } else { "false" }.encode_utf16()),
                            Arg::String if word == 0 => text.extend("null".encode_utf16()),
                            Arg::String => text.extend(utf16_of(word)?),
                        }
                    }
                }
            }
            // Everything is copied out, so the arguments can leave the stack before the result
            // is allocated.
            frame.drop_top_slots(self.slots);
        }
        let result = new_string(&text)?;
        last_frame_mut(stack_frames)?.push(Slot::Ref(result[0].value()))
    }
}

fn is_string(reference: i32) -> Result<bool> {
    Ok(reference != 0 && HEAP.get_instance_name(reference)? == STRING)
}

/// A `String`'s UTF-16 code units.
fn utf16_of(string_ref: i32) -> Result<Vec<u16>> {
    let (latin, array_ref) = get_raw_string_info(string_ref)?;
    let bytes = HEAP.get_entire_raw_data(array_ref)?;
    Ok(if latin {
        bytes.iter().map(|&byte| u16::from(byte)).collect()
    } else {
        bytes
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect()
    })
}

/// A new `String` holding `units`, compact (Latin-1) when every unit fits, as `String`'s own
/// constructors store it. The returned destination roots it.
fn new_string(units: &[u16]) -> Result<RootedSlots> {
    let latin = compact_strings()? && units.iter().all(|&unit| unit <= 0xFF);
    let bytes: Vec<u8> = if latin {
        units.iter().map(|&unit| unit as u8).collect()
    } else {
        units.iter().flat_map(|unit| unit.to_ne_bytes()).collect()
    };
    let mut array = RootedSlots::new();
    let array_ref = HEAP.create_array("[B", bytes.len() as i32, &mut array)?;
    HEAP.get_entire_raw_data_mut(array_ref)?
        .copy_from_slice(&bytes);

    let (klass_id, _, klass) = CLASSES.get_full(STRING)?;
    let mut string = RootedSlots::new();
    let string_ref = HEAP.create_instance(klass.new_instance(klass_id)?, &mut string)?;
    HEAP.set_object_field_value(string_ref, STRING, "value", vec![array_ref])?;
    HEAP.set_object_field_value(string_ref, STRING, "coder", vec![i32::from(!latin)])?;
    Ok(string)
}
