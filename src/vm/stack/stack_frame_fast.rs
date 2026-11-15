//! A tight loop over the bytecodes that touch nothing but the executing frame.
//!
//! Loads, stores, constants, primitive arithmetic, conversions, comparisons and branches read and
//! write only the frame's locals and operand stack: they cannot throw, allocate, call or block.
//! Running a stretch of them here, with the frame's parts borrowed once, spares each of them the
//! interpreter loop's per-instruction work (frame lookup, class tracking, safepoint counting,
//! error plumbing), which costs several times what the instruction itself does.
//!
//! Every instruction here does exactly what its handler in the `ops_*_processor` modules does,
//! including which slots come out tagged as references. Anything else, and any instruction that
//! could fail (a full operand stack, a zero divisor), is left for those handlers.
//!
//! Field and array instructions also run here once their site is resolved: the field's position
//! (or static storage) is cached, the class it needs is initialized, and the object or array is
//! present. They read or write the heap without allocating or polling. A null reference, an index
//! out of bounds or anything not yet resolved stops the loop before the instruction changes
//! anything, and its handler then does the rest, exceptions included.

use super::StackFrame;
use crate::vm::heap::heap::HEAP;
use crate::vm::method_area::java_class::{InstanceField, JavaClass, MemberRefKind, StaticField};
use crate::vm::stack::slot::Slot;

/// The constant-pool index an instruction at `pc` names in its two operand bytes.
#[inline(always)]
fn pool_index(code: &[u8], pc: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*code.get(pc + 1)?, *code.get(pc + 2)?]))
}

/// A `getfield`/`putfield` site's field and its position, if the site has been resolved.
#[inline(always)]
fn resolved_instance_field<'a>(
    klass: Option<&'a JavaClass>,
    code: &[u8],
    pc: usize,
) -> Option<(&'a InstanceField, usize, usize)> {
    let member = klass?.resolved_member_ref(MemberRefKind::Field, pool_index(code, pc)?)?;
    let field = member.instance_field.get()?;
    let &(offset, width) = field.position.get()?;
    Some((field, offset as usize, width as usize))
}

/// A `getstatic`/`putstatic` site's field, if the site has been resolved and its class is
/// initialized.
#[inline(always)]
fn resolved_static_field<'a>(
    klass: Option<&'a JavaClass>,
    code: &[u8],
    pc: usize,
) -> Option<&'a StaticField> {
    let member = klass?.resolved_member_ref(MemberRefKind::Field, pool_index(code, pc)?)?;
    let field = member.static_field.get()?;
    field
        .holder
        .static_fields_init_state()
        .is_initialized()
        .then_some(field)
}

/// Pushes field or array words as their handlers do: a wide value low word first, a reference
/// tagged.
#[inline(always)]
fn push_words(stack: &mut Vec<Slot>, words: [i32; 2], wide: bool, reference: bool) {
    if wide {
        stack.push(Slot::Value(words[1]));
        stack.push(Slot::Value(words[0]));
    } else if reference {
        stack.push(Slot::Ref(words[0]));
    } else {
        stack.push(Slot::Value(words[0]));
    }
}

/// The value words on top of the stack, top first, as the handlers pop them.
#[inline(always)]
fn peek_words(stack: &[Slot], wide: bool) -> Option<[i32; 2]> {
    let depth = stack.len();
    let first = stack.get(depth.checked_sub(1)?)?.value();
    Some(if wide {
        [first, stack.get(depth.checked_sub(2)?)?.value()]
    } else {
        [first, 0]
    })
}

/// The slot `depth` below the top, `1` being the top itself.
#[inline(always)]
fn peek(stack: &[Slot], depth: usize) -> Option<i32> {
    Some(stack.get(stack.len().checked_sub(depth)?)?.value())
}

#[inline(always)]
fn pop(stack: &mut Vec<Slot>) -> i32 {
    stack.pop().expect("Empty stack").value()
}

#[inline(always)]
fn pop_slot(stack: &mut Vec<Slot>) -> Slot {
    stack.pop().expect("Empty stack")
}

#[inline(always)]
fn pop_long(stack: &mut Vec<Slot>) -> i64 {
    let high = pop(stack);
    let low = pop(stack);
    (i64::from(high) << 32) | i64::from(low as u32)
}

#[inline(always)]
fn push(stack: &mut Vec<Slot>, value: i32) {
    stack.push(Slot::Value(value));
}

#[inline(always)]
fn push_long(stack: &mut Vec<Slot>, value: i64) {
    stack.push(Slot::Value(value as i32));
    stack.push(Slot::Value((value >> 32) as i32));
}

#[inline(always)]
fn local_long(locals: &[Slot], index: usize) -> i64 {
    (i64::from(locals[index + 1].value()) << 32) | i64::from(locals[index].value() as u32)
}

#[inline(always)]
fn set_local_long(locals: &mut [Slot], index: usize, value: i64) {
    locals[index] = Slot::Value(value as i32);
    locals[index + 1] = Slot::Value((value >> 32) as i32);
}

impl StackFrame {
    /// Runs frame-local instructions from the current pc, at most `budget` of them, stopping
    /// before the first instruction that is not one. Returns how many ran.
    pub(crate) fn run_frame_local(&mut self, budget: u32) -> u32 {
        let code: &[u8] = &self.code.bytecode;
        let klass: Option<&JavaClass> = self.code.klass.0.get().map(|klass| &**klass);
        let locals: &mut [Slot] = &mut self.locals;
        let (stack, max_stack) = self.operand_stack.parts();
        let mut pc = self.pc;
        let mut done = 0u32;

        // Leaves an instruction that would overflow the operand stack to its handler, which
        // reports it.
        macro_rules! room {
            ($slots:expr) => {
                if stack.len() + $slots > max_stack {
                    break;
                }
            };
        }
        macro_rules! branch {
            ($taken:expr) => {{
                if $taken {
                    let offset = i16::from_be_bytes([code[pc + 1], code[pc + 2]]);
                    pc = (pc as isize + offset as isize) as usize;
                } else {
                    pc += 3;
                }
            }};
        }
        macro_rules! int_binary {
            (|$a:ident, $b:ident| $result:expr) => {{
                let $b = pop(stack);
                let $a = pop(stack);
                push(stack, $result);
                pc += 1;
            }};
        }
        macro_rules! long_binary {
            (|$a:ident, $b:ident| $result:expr) => {{
                let $b = pop_long(stack);
                let $a = pop_long(stack);
                push_long(stack, $result);
                pc += 1;
            }};
        }
        macro_rules! long_shift {
            (|$a:ident, $b:ident| $result:expr) => {{
                let $b = pop(stack) as u32 & 63;
                let $a = pop_long(stack);
                push_long(stack, $result);
                pc += 1;
            }};
        }
        macro_rules! float_binary {
            (|$a:ident, $b:ident| $result:expr) => {{
                let $b = f32::from_bits(pop(stack) as u32);
                let $a = f32::from_bits(pop(stack) as u32);
                let result: f32 = $result;
                push(stack, result.to_bits() as i32);
                pc += 1;
            }};
        }
        macro_rules! double_binary {
            (|$a:ident, $b:ident| $result:expr) => {{
                let $b = f64::from_bits(pop_long(stack) as u64);
                let $a = f64::from_bits(pop_long(stack) as u64);
                let result: f64 = $result;
                push_long(stack, result.to_bits() as i64);
                pc += 1;
            }};
        }

        while done < budget {
            let Some(&opcode) = code.get(pc) else { break };
            match opcode {
                // nop
                0 => pc += 1,
                // aconst_null
                1 => {
                    room!(1);
                    stack.push(Slot::Ref(0));
                    pc += 1;
                }
                // iconst_m1 .. iconst_5
                2..=8 => {
                    room!(1);
                    push(stack, i32::from(opcode) - 3);
                    pc += 1;
                }
                // lconst_0, lconst_1
                9 | 10 => {
                    room!(2);
                    push_long(stack, i64::from(opcode - 9));
                    pc += 1;
                }
                // fconst_0 .. fconst_2
                11..=13 => {
                    room!(1);
                    push(stack, f32::from(opcode - 11).to_bits() as i32);
                    pc += 1;
                }
                // dconst_0, dconst_1
                14 | 15 => {
                    room!(2);
                    push_long(stack, f64::from(opcode - 14).to_bits() as i64);
                    pc += 1;
                }
                // bipush
                16 => {
                    room!(1);
                    push(stack, i32::from(code[pc + 1] as i8));
                    pc += 2;
                }
                // sipush
                17 => {
                    room!(1);
                    push(
                        stack,
                        i32::from(i16::from_be_bytes([code[pc + 1], code[pc + 2]])),
                    );
                    pc += 3;
                }
                // iload, fload
                21 | 23 => {
                    room!(1);
                    push(stack, locals[usize::from(code[pc + 1])].value());
                    pc += 2;
                }
                // lload, dload
                22 | 24 => {
                    room!(2);
                    push_long(stack, local_long(locals, usize::from(code[pc + 1])));
                    pc += 2;
                }
                // aload
                25 => {
                    room!(1);
                    stack.push(locals[usize::from(code[pc + 1])]);
                    pc += 2;
                }
                // iload_0 .. iload_3
                26..=29 => {
                    room!(1);
                    push(stack, locals[usize::from(opcode - 26)].value());
                    pc += 1;
                }
                // lload_0 .. lload_3
                30..=33 => {
                    room!(2);
                    push_long(stack, local_long(locals, usize::from(opcode - 30)));
                    pc += 1;
                }
                // fload_0 .. fload_3
                34..=37 => {
                    room!(1);
                    push(stack, locals[usize::from(opcode - 34)].value());
                    pc += 1;
                }
                // dload_0 .. dload_3
                38..=41 => {
                    room!(2);
                    push_long(stack, local_long(locals, usize::from(opcode - 38)));
                    pc += 1;
                }
                // aload_0 .. aload_3
                42..=45 => {
                    room!(1);
                    stack.push(locals[usize::from(opcode - 42)]);
                    pc += 1;
                }
                // istore, fstore
                54 | 56 => {
                    locals[usize::from(code[pc + 1])] = Slot::Value(pop(stack));
                    pc += 2;
                }
                // lstore, dstore
                55 | 57 => {
                    let value = pop_long(stack);
                    set_local_long(locals, usize::from(code[pc + 1]), value);
                    pc += 2;
                }
                // astore
                58 => {
                    locals[usize::from(code[pc + 1])] = pop_slot(stack);
                    pc += 2;
                }
                // istore_0 .. istore_3
                59..=62 => {
                    locals[usize::from(opcode - 59)] = Slot::Value(pop(stack));
                    pc += 1;
                }
                // lstore_0 .. lstore_3
                63..=66 => {
                    let value = pop_long(stack);
                    set_local_long(locals, usize::from(opcode - 63), value);
                    pc += 1;
                }
                // fstore_0 .. fstore_3
                67..=70 => {
                    locals[usize::from(opcode - 67)] = Slot::Value(pop(stack));
                    pc += 1;
                }
                // dstore_0 .. dstore_3
                71..=74 => {
                    let value = pop_long(stack);
                    set_local_long(locals, usize::from(opcode - 71), value);
                    pc += 1;
                }
                // astore_0 .. astore_3
                75..=78 => {
                    locals[usize::from(opcode - 75)] = pop_slot(stack);
                    pc += 1;
                }
                // pop
                87 => {
                    pop_slot(stack);
                    pc += 1;
                }
                // pop2
                88 => {
                    pop_slot(stack);
                    pop_slot(stack);
                    pc += 1;
                }
                // dup
                89 => {
                    room!(1);
                    let value = *stack.last().expect("Empty stack");
                    stack.push(value);
                    pc += 1;
                }
                // dup_x1
                90 => {
                    room!(1);
                    let value1 = pop_slot(stack);
                    let value2 = pop_slot(stack);
                    stack.push(value1);
                    stack.push(value2);
                    stack.push(value1);
                    pc += 1;
                }
                // dup2
                92 => {
                    room!(2);
                    let value1 = pop_slot(stack);
                    let value2 = pop_slot(stack);
                    stack.push(value2);
                    stack.push(value1);
                    stack.push(value2);
                    stack.push(value1);
                    pc += 1;
                }
                // swap
                95 => {
                    let value1 = pop_slot(stack);
                    let value2 = pop_slot(stack);
                    stack.push(value1);
                    stack.push(value2);
                    pc += 1;
                }
                96 => int_binary!(|a, b| a.wrapping_add(b)),
                97 => long_binary!(|a, b| a.wrapping_add(b)),
                98 => float_binary!(|a, b| a + b),
                99 => double_binary!(|a, b| a + b),
                100 => int_binary!(|a, b| a.wrapping_sub(b)),
                101 => long_binary!(|a, b| a.wrapping_sub(b)),
                102 => float_binary!(|a, b| a - b),
                103 => double_binary!(|a, b| a - b),
                104 => int_binary!(|a, b| a.wrapping_mul(b)),
                105 => long_binary!(|a, b| a.wrapping_mul(b)),
                106 => float_binary!(|a, b| a * b),
                107 => double_binary!(|a, b| a * b),
                // idiv, irem: a zero divisor throws, which the handler does
                108 | 112 => {
                    if stack.last().expect("Empty stack").value() == 0 {
                        break;
                    }
                    if opcode == 108 {
                        int_binary!(|a, b| a.wrapping_div(b))
                    } else {
                        int_binary!(|a, b| a.wrapping_rem(b))
                    }
                }
                // ldiv, lrem
                109 | 113 => {
                    let depth = stack.len();
                    assert!(depth >= 2, "Empty stack");
                    if stack[depth - 1].value() == 0 && stack[depth - 2].value() == 0 {
                        break;
                    }
                    if opcode == 109 {
                        long_binary!(|a, b| a.wrapping_div(b))
                    } else {
                        long_binary!(|a, b| a.wrapping_rem(b))
                    }
                }
                110 => float_binary!(|a, b| a / b),
                111 => double_binary!(|a, b| a / b),
                // ineg
                116 => {
                    let value = pop(stack);
                    push(stack, value.wrapping_neg());
                    pc += 1;
                }
                // lneg
                117 => {
                    let value = pop_long(stack);
                    push_long(stack, value.wrapping_neg());
                    pc += 1;
                }
                120 => int_binary!(|a, b| a << (b & 31)),
                121 => long_shift!(|a, b| a << b),
                122 => int_binary!(|a, b| a >> (b & 31)),
                123 => long_shift!(|a, b| a >> b),
                124 => int_binary!(|a, b| ((a as u32) >> (b as u32 & 31)) as i32),
                125 => long_shift!(|a, b| ((a as u64) >> b) as i64),
                126 => int_binary!(|a, b| a & b),
                127 => long_binary!(|a, b| a & b),
                128 => int_binary!(|a, b| a | b),
                129 => long_binary!(|a, b| a | b),
                130 => int_binary!(|a, b| a ^ b),
                131 => long_binary!(|a, b| a ^ b),
                // iinc
                132 => {
                    let index = usize::from(code[pc + 1]);
                    let delta = i32::from(code[pc + 2] as i8);
                    locals[index] = Slot::Value(locals[index].value().wrapping_add(delta));
                    pc += 3;
                }
                // i2l
                133 => {
                    room!(1);
                    let value = pop(stack);
                    push_long(stack, i64::from(value));
                    pc += 1;
                }
                // i2f
                134 => {
                    let value = pop(stack);
                    push(stack, (value as f32).to_bits() as i32);
                    pc += 1;
                }
                // i2d
                135 => {
                    room!(1);
                    let value = pop(stack);
                    push_long(stack, f64::from(value).to_bits() as i64);
                    pc += 1;
                }
                // l2i
                136 => {
                    let value = pop_long(stack);
                    push(stack, value as i32);
                    pc += 1;
                }
                // l2f
                137 => {
                    let value = pop_long(stack);
                    push(stack, (value as f32).to_bits() as i32);
                    pc += 1;
                }
                // l2d
                138 => {
                    let value = pop_long(stack);
                    push_long(stack, (value as f64).to_bits() as i64);
                    pc += 1;
                }
                // f2i
                139 => {
                    let value = f32::from_bits(pop(stack) as u32);
                    push(stack, value as i32);
                    pc += 1;
                }
                // f2l
                140 => {
                    room!(1);
                    let value = f32::from_bits(pop(stack) as u32);
                    push_long(stack, value as i64);
                    pc += 1;
                }
                // f2d
                141 => {
                    room!(1);
                    let value = f32::from_bits(pop(stack) as u32);
                    push_long(stack, f64::from(value).to_bits() as i64);
                    pc += 1;
                }
                // d2i
                142 => {
                    let value = f64::from_bits(pop_long(stack) as u64);
                    push(stack, value as i32);
                    pc += 1;
                }
                // d2l
                143 => {
                    let value = f64::from_bits(pop_long(stack) as u64);
                    push_long(stack, value as i64);
                    pc += 1;
                }
                // d2f
                144 => {
                    let value = f64::from_bits(pop_long(stack) as u64);
                    push(stack, (value as f32).to_bits() as i32);
                    pc += 1;
                }
                // i2b
                145 => {
                    let value = pop(stack);
                    push(stack, i32::from(value as i8));
                    pc += 1;
                }
                // i2c
                146 => {
                    let value = pop(stack);
                    push(stack, i32::from(value as u16));
                    pc += 1;
                }
                // i2s
                147 => {
                    let value = pop(stack);
                    push(stack, i32::from(value as i16));
                    pc += 1;
                }
                // lcmp
                148 => {
                    let value2 = pop_long(stack);
                    let value1 = pop_long(stack);
                    push(stack, value1.cmp(&value2) as i32);
                    pc += 1;
                }
                // ifeq .. ifle
                153..=158 => {
                    let value = pop(stack);
                    branch!(match opcode {
                        153 => value == 0,
                        154 => value != 0,
                        155 => value < 0,
                        156 => value >= 0,
                        157 => value > 0,
                        _ => value <= 0,
                    });
                }
                // if_icmpeq .. if_icmple, if_acmpeq, if_acmpne
                159..=166 => {
                    let value2 = pop(stack);
                    let value1 = pop(stack);
                    branch!(match opcode {
                        159 | 165 => value1 == value2,
                        160 | 166 => value1 != value2,
                        161 => value1 < value2,
                        162 => value1 >= value2,
                        163 => value1 > value2,
                        _ => value1 <= value2,
                    });
                }
                // goto
                167 => branch!(true),
                // ifnull, ifnonnull
                198 | 199 => {
                    let value = pop(stack);
                    branch!((value == 0) == (opcode == 198));
                }
                // iaload, laload, faload, daload, aaload, baload, caload, saload
                46..=53 => {
                    let (Some(index), Some(array)) = (peek(stack, 1), peek(stack, 2)) else {
                        break;
                    };
                    if array == 0 {
                        break;
                    }
                    let Ok(Ok(words)) = HEAP.load_array_element(array, index) else {
                        break;
                    };
                    stack.truncate(stack.len() - 2);
                    push_words(stack, words, matches!(opcode, 47 | 49), opcode == 50);
                    pc += 1;
                }
                // iastore, lastore, fastore, dastore, bastore, castore, sastore (not aastore,
                // which checks the element type)
                79..=82 | 84..=86 => {
                    let wide = matches!(opcode, 80 | 82);
                    let width = if wide { 2 } else { 1 };
                    let (Some(words), Some(index), Some(array)) = (
                        peek_words(stack, wide),
                        peek(stack, width + 1),
                        peek(stack, width + 2),
                    ) else {
                        break;
                    };
                    if array == 0 {
                        break;
                    }
                    let Ok(Ok(())) = HEAP.store_array_element(array, index, words) else {
                        break;
                    };
                    stack.truncate(stack.len() - width - 2);
                    pc += 1;
                }
                // arraylength
                190 => {
                    let Some(array) = peek(stack, 1).filter(|array| *array != 0) else {
                        break;
                    };
                    let Ok(length) = HEAP.get_array_len(array) else {
                        break;
                    };
                    stack.pop();
                    push(stack, length);
                    pc += 1;
                }
                // getstatic
                178 => {
                    let Some(field) = resolved_static_field(klass, code, pc) else {
                        break;
                    };
                    room!(if field.wide { 2 } else { 1 });
                    let Ok(words) = field.value.words() else {
                        break;
                    };
                    push_words(stack, words, field.wide, field.reference);
                    pc += 3;
                }
                // putstatic
                179 => {
                    let Some(field) = resolved_static_field(klass, code, pc) else {
                        break;
                    };
                    let Some(words) = peek_words(stack, field.wide) else {
                        break;
                    };
                    if field.value.set_words(words).is_err() {
                        break;
                    }
                    stack.truncate(stack.len() - if field.wide { 2 } else { 1 });
                    pc += 3;
                }
                // getfield
                180 => {
                    let Some((field, offset, width)) = resolved_instance_field(klass, code, pc)
                    else {
                        break;
                    };
                    let Some(object) = peek(stack, 1).filter(|object| *object != 0) else {
                        break;
                    };
                    if field.wide {
                        room!(1);
                    }
                    let Some(words) = HEAP.read_field_at(object, offset, width) else {
                        break;
                    };
                    stack.pop();
                    push_words(stack, words, field.wide, field.reference);
                    pc += 3;
                }
                // putfield
                181 => {
                    let Some((field, offset, width)) = resolved_instance_field(klass, code, pc)
                    else {
                        break;
                    };
                    let slots = if field.wide { 2 } else { 1 };
                    let (Some(words), Some(object)) =
                        (peek_words(stack, field.wide), peek(stack, slots + 1))
                    else {
                        break;
                    };
                    if object == 0 {
                        break;
                    }
                    if HEAP.write_field_at(object, offset, width, words).is_none() {
                        break;
                    }
                    stack.truncate(stack.len() - slots - 1);
                    pc += 3;
                }
                _ => break,
            }
            done += 1;
        }

        self.pc = pc;
        done
    }
}
