//! Translates a method's bytecode into native code with Cranelift.
//!
//! Every instruction becomes a block, entered either from the instruction before it, from a
//! branch, or from the function's entry, which jumps to the block of the pc it is called at.
//! The operand stack and the locals stay in the frame's slots: an instruction loads what it
//! pops from them and stores what it pushes, with the slot's tag, exactly as the frame-local
//! loop does. Only the index of the stack's top, the pointer to the slots and the count of
//! instructions run live in registers between instructions, and they are written back to the
//! [`JitIo`] whenever the code returns.

use super::{JitIo, Step, STOP_INVOKE, STOP_OTHER, STOP_RETURN};
use crate::vm::stack::slot::Slot;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types::{F32, F64, I32, I64, I8};
use cranelift_codegen::ir::{AbiParam, Block, FuncRef, InstBuilder, MemFlagsData, Type, Value};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::BTreeMap;
use std::mem::offset_of;

/// Where a slot's tag and value are: `Slot` is `repr(u32)`, a `u32` tag then the `i32`.
const SLOT_SIZE: i64 = std::mem::size_of::<Slot>() as i64;
const TAG: i32 = 0;
const VALUE: i32 = 4;
const TAG_VALUE: i64 = 0;
const TAG_REF: i64 = 1;
const _: () = assert!(std::mem::size_of::<Slot>() == 8 && std::mem::align_of::<Slot>() == 4);

pub(super) struct Compiler {
    module: JITModule,
    context: Context,
    builder_context: FunctionBuilderContext,
    step: FuncId,
    pointer: Type,
    methods: usize,
}

impl Compiler {
    pub(super) fn new(step: Step) -> Result<Self, String> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").map_err(|e| e.to_string())?;
        // The verifier checks the code this compiler hands over, which is this compiler's own:
        // debug builds run it, release builds save its time (a fifth of a compilation).
        let verify = if cfg!(debug_assertions) {
            "true"
        } else {
            "false"
        };
        flags
            .set("enable_verifier", verify)
            .map_err(|e| e.to_string())?;
        // Compiled code is only ever called from Rust, in this process.
        flags.set("is_pic", "false").map_err(|e| e.to_string())?;
        let isa = cranelift_native::builder()
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())?;
        let pointer = isa.pointer_type();
        let mut jit_builder = JITBuilder::with_isa(isa, default_libcall_names());
        jit_builder.symbol("rusty_jvm_jit_step", step as *const u8);
        let mut module = JITModule::new(jit_builder);
        let mut signature = module.make_signature();
        signature.params.push(AbiParam::new(pointer));
        signature.returns.push(AbiParam::new(I32));
        let step = module
            .declare_function("rusty_jvm_jit_step", Linkage::Import, &signature)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            context: module.make_context(),
            module,
            builder_context: FunctionBuilderContext::new(),
            step,
            pointer,
            methods: 0,
        })
    }

    /// Compiles `code`, returning the address of its entry.
    pub(super) fn compile(&mut self, code: &[u8]) -> Result<*const u8, String> {
        let instructions = decode(code)?;
        self.module.clear_context(&mut self.context);
        self.context
            .func
            .signature
            .params
            .push(AbiParam::new(self.pointer));
        self.methods += 1;
        let name = format!("jit_method_{}", self.methods);
        let id = self
            .module
            .declare_function(&name, Linkage::Local, &self.context.func.signature)
            .map_err(|e| e.to_string())?;
        {
            let step = self
                .module
                .declare_func_in_func(self.step, &mut self.context.func);
            let builder = FunctionBuilder::new(&mut self.context.func, &mut self.builder_context);
            let mut translator =
                Translator::new(builder, code, &instructions, step, self.pointer)?;
            translator.translate()?;
            translator.builder.seal_all_blocks();
            translator
                .builder
                .finalize(self.module.isa().frontend_config());
        }
        self.module
            .define_function(id, &mut self.context)
            .map_err(|e| format!("{e:?}"))?;
        self.module.clear_context(&mut self.context);
        self.module
            .finalize_definitions()
            .map_err(|e| e.to_string())?;
        Ok(self.module.get_finalized_function(id))
    }
}

/// The instructions' start pcs, each with its length.
fn decode(code: &[u8]) -> Result<BTreeMap<usize, usize>, String> {
    let mut instructions = BTreeMap::new();
    let mut pc = 0;
    while pc < code.len() {
        let length = instruction_length(code, pc)
            .ok_or_else(|| format!("cannot decode the instruction at {pc}"))?;
        instructions.insert(pc, length);
        pc += length;
    }
    Ok(instructions)
}

fn instruction_length(code: &[u8], pc: usize) -> Option<usize> {
    let opcode = *code.get(pc)?;
    Some(match opcode {
        16 | 18 | 21..=25 | 54..=58 | 169 | 188 => 2,
        17 | 19 | 20 | 132 | 153..=168 | 178..=184 | 187 | 189 | 192 | 193 | 198 | 199 => 3,
        197 => 4,
        185 | 186 | 200 | 201 => 5,
        196 => {
            if *code.get(pc + 1)? == 132 {
                6
            } else {
                4
            }
        }
        170 => {
            let base = (pc + 4) & !3;
            let low = read_i32(code, base + 4)?;
            let high = read_i32(code, base + 8)?;
            let count = usize::try_from(i64::from(high) - i64::from(low) + 1).ok()?;
            base + 12 + count * 4 - pc
        }
        171 => {
            let base = (pc + 4) & !3;
            let pairs = usize::try_from(read_i32(code, base + 4)?).ok()?;
            base + 8 + pairs * 8 - pc
        }
        202..=255 => return None,
        _ => 1,
    })
}

fn read_i32(code: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(code.get(at..at + 4)?.try_into().ok()?))
}

fn read_i16(code: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([code[at], code[at + 1]])
}

struct Translator<'a, 'b> {
    builder: FunctionBuilder<'b>,
    code: &'a [u8],
    instructions: &'a BTreeMap<usize, usize>,
    blocks: BTreeMap<usize, Block>,
    step: FuncRef,
    pointer: Type,
    io: Value,
    slots: Value,
    limit: Value,
    budget: Value,
    top: Variable,
    done: Variable,
}

impl<'a, 'b> Translator<'a, 'b> {
    fn new(
        mut builder: FunctionBuilder<'b>,
        code: &'a [u8],
        instructions: &'a BTreeMap<usize, usize>,
        step: FuncRef,
        pointer: Type,
    ) -> Result<Self, String> {
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let io = builder.block_params(entry)[0];
        let flags = MemFlagsData::trusted();
        let slots = builder
            .ins()
            .load(pointer, flags, io, offset_of!(JitIo, slots) as i32);
        let limit = builder
            .ins()
            .load(I32, flags, io, offset_of!(JitIo, limit) as i32);
        let limit = builder.ins().uextend(I64, limit);
        let budget = builder
            .ins()
            .load(I32, flags, io, offset_of!(JitIo, budget) as i32);
        let top = builder.declare_var(I64);
        let done = builder.declare_var(I32);
        let start_top = builder
            .ins()
            .load(I32, flags, io, offset_of!(JitIo, top) as i32);
        let start_top = builder.ins().uextend(I64, start_top);
        builder.def_var(top, start_top);
        let zero = builder.ins().iconst(I32, 0);
        builder.def_var(done, zero);
        let blocks = instructions
            .keys()
            .map(|&pc| (pc, builder.create_block()))
            .collect();
        Ok(Self {
            builder,
            code,
            instructions,
            blocks,
            step,
            pointer,
            io,
            slots,
            limit,
            budget,
            top,
            done,
        })
    }

    fn translate(&mut self) -> Result<(), String> {
        // The entry: jump to the block of the pc the code is called at.
        let start_pc = self.load_io(offset_of!(JitIo, pc));
        let decline = self.builder.create_block();
        let mut switch = Switch::new();
        for (&pc, &block) in &self.blocks {
            switch.set_entry(pc as u128, block);
        }
        switch.emit(&mut self.builder, start_pc, decline);
        self.builder.switch_to_block(decline);
        // Not an instruction start: run nothing, and let the interpreter have it.
        let zero = self.builder.ins().iconst(I32, 0);
        self.store_io(offset_of!(JitIo, done), zero);
        let other = self.builder.ins().iconst(I32, i64::from(STOP_OTHER));
        self.store_io(offset_of!(JitIo, stop), other);
        self.builder.ins().return_(&[]);

        let instructions: Vec<(usize, usize)> = self
            .instructions
            .iter()
            .map(|(&pc, &len)| (pc, len))
            .collect();
        for (pc, length) in instructions {
            let block = self.blocks[&pc];
            self.builder.switch_to_block(block);
            self.instruction(pc, length)?;
        }
        Ok(())
    }

    // ---- io and slots ----

    fn load_io(&mut self, offset: usize) -> Value {
        self.builder
            .ins()
            .load(I32, MemFlagsData::trusted(), self.io, offset as i32)
    }

    fn store_io(&mut self, offset: usize, value: Value) {
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, self.io, offset as i32);
    }

    /// Writes the top and the count back, then `pc` and `stop`, and returns.
    fn exit(&mut self, pc: usize, stop: u32) {
        self.write_back_top();
        let done = self.builder.use_var(self.done);
        self.store_io(offset_of!(JitIo, done), done);
        let pc = self.builder.ins().iconst(I32, pc as i64);
        self.store_io(offset_of!(JitIo, pc), pc);
        let stop = self.builder.ins().iconst(I32, i64::from(stop));
        self.store_io(offset_of!(JitIo, stop), stop);
        self.builder.ins().return_(&[]);
    }

    fn write_back_top(&mut self) {
        let top = self.builder.use_var(self.top);
        let top = self.builder.ins().ireduce(I32, top);
        self.store_io(offset_of!(JitIo, top), top);
    }

    /// Exits at `pc` with `STOP_OTHER` when `condition` holds; carries on otherwise.
    fn exit_if(&mut self, condition: Value, pc: usize) {
        let exit = self.builder.create_block();
        let carry_on = self.builder.create_block();
        self.builder.ins().brif(condition, exit, &[], carry_on, &[]);
        self.builder.switch_to_block(exit);
        self.exit(pc, STOP_OTHER);
        self.builder.switch_to_block(carry_on);
    }

    /// Exits before the instruction at `pc` unless the operand stack has room for `slots` more.
    fn room(&mut self, slots: i64, pc: usize) {
        let top = self.builder.use_var(self.top);
        let needed = self.builder.ins().iadd_imm(top, slots);
        let full = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, needed, self.limit);
        self.exit_if(full, pc);
    }

    /// The address of the slot `depth` below the top (1 is the top slot).
    fn stack_addr(&mut self, depth: i64) -> Value {
        let top = self.builder.use_var(self.top);
        let index = self.builder.ins().iadd_imm(top, -depth);
        let offset = self.builder.ins().imul_imm(index, SLOT_SIZE);
        let offset = self.cast_pointer(offset);
        self.builder.ins().iadd(self.slots, offset)
    }

    fn local_addr(&mut self, index: usize) -> Value {
        self.builder
            .ins()
            .iadd_imm(self.slots, index as i64 * SLOT_SIZE)
    }

    fn cast_pointer(&mut self, value: Value) -> Value {
        if self.pointer == I64 {
            value
        } else {
            self.builder.ins().ireduce(self.pointer, value)
        }
    }

    fn adjust_top(&mut self, delta: i64) {
        let top = self.builder.use_var(self.top);
        let top = self.builder.ins().iadd_imm(top, delta);
        self.builder.def_var(self.top, top);
    }

    fn read_value(&mut self, addr: Value) -> Value {
        self.builder
            .ins()
            .load(I32, MemFlagsData::trusted(), addr, VALUE)
    }

    fn write_slot(&mut self, addr: Value, value: Value, tag: i64) {
        let tag = self.builder.ins().iconst(I32, tag);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), tag, addr, TAG);
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), value, addr, VALUE);
    }

    /// A whole slot, tag and value, as one 64-bit word.
    fn read_slot(&mut self, addr: Value) -> Value {
        self.builder
            .ins()
            .load(I64, MemFlagsData::trusted(), addr, 0)
    }

    fn copy_slot_to(&mut self, addr: Value, slot: Value) {
        self.builder
            .ins()
            .store(MemFlagsData::trusted(), slot, addr, 0);
    }

    fn push_tagged(&mut self, value: Value, tag: i64) {
        let addr = self.stack_addr(0);
        self.write_slot(addr, value, tag);
        self.adjust_top(1);
    }

    fn push(&mut self, value: Value) {
        self.push_tagged(value, TAG_VALUE);
    }

    fn pop(&mut self) -> Value {
        let addr = self.stack_addr(1);
        let value = self.read_value(addr);
        self.adjust_top(-1);
        value
    }

    /// The `i32` slot `depth` below the top, without popping it.
    fn peek(&mut self, depth: i64) -> Value {
        let addr = self.stack_addr(depth);
        self.read_value(addr)
    }

    fn push_long(&mut self, value: Value) {
        let low = self.builder.ins().ireduce(I32, value);
        let high = self.builder.ins().ushr_imm(value, 32);
        let high = self.builder.ins().ireduce(I32, high);
        self.push(low);
        self.push(high);
    }

    fn pop_long(&mut self) -> Value {
        let high = self.pop();
        let low = self.pop();
        self.join_long(low, high)
    }

    fn join_long(&mut self, low: Value, high: Value) -> Value {
        let low = self.builder.ins().uextend(I64, low);
        let high = self.builder.ins().uextend(I64, high);
        let high = self.builder.ins().ishl_imm(high, 32);
        self.builder.ins().bor(low, high)
    }

    fn push_float(&mut self, value: Value) {
        let bits = self.builder.ins().bitcast(I32, MemFlagsData::new(), value);
        self.push(bits);
    }

    fn pop_float(&mut self) -> Value {
        let bits = self.pop();
        self.builder.ins().bitcast(F32, MemFlagsData::new(), bits)
    }

    fn push_double(&mut self, value: Value) {
        let bits = self.builder.ins().bitcast(I64, MemFlagsData::new(), value);
        self.push_long(bits);
    }

    fn pop_double(&mut self) -> Value {
        let bits = self.pop_long();
        self.builder.ins().bitcast(F64, MemFlagsData::new(), bits)
    }

    fn local(&mut self, index: usize) -> Value {
        let addr = self.local_addr(index);
        self.read_value(addr)
    }

    fn local_long(&mut self, index: usize) -> Value {
        let low = self.local(index);
        let high = self.local(index + 1);
        self.join_long(low, high)
    }

    fn set_local(&mut self, index: usize, value: Value) {
        let addr = self.local_addr(index);
        self.write_slot(addr, value, TAG_VALUE);
    }

    fn set_local_long(&mut self, index: usize, value: Value) {
        let low = self.builder.ins().ireduce(I32, value);
        let high = self.builder.ins().ushr_imm(value, 32);
        let high = self.builder.ins().ireduce(I32, high);
        self.set_local(index, low);
        self.set_local(index + 1, high);
    }

    fn count_instruction(&mut self) {
        let done = self.builder.use_var(self.done);
        let done = self.builder.ins().iadd_imm(done, 1);
        self.builder.def_var(self.done, done);
    }

    fn block_at(&self, pc: usize) -> Result<Block, String> {
        self.blocks
            .get(&pc)
            .copied()
            .ok_or_else(|| format!("branch to {pc}, which starts no instruction"))
    }

    /// Goes on at `target` after the instruction at `pc`, which has been counted. A backward
    /// branch first checks the budget, and exits at `target` once it is spent.
    fn goto(&mut self, pc: usize, target: usize) -> Result<(), String> {
        let block = self.block_at(target)?;
        if target <= pc {
            let done = self.builder.use_var(self.done);
            let spent =
                self.builder
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThanOrEqual, done, self.budget);
            let exit = self.builder.create_block();
            self.builder.ins().brif(spent, exit, &[], block, &[]);
            self.builder.switch_to_block(exit);
            self.exit(target, STOP_OTHER);
        } else {
            self.builder.ins().jump(block, &[]);
        }
        Ok(())
    }

    fn fall_through(&mut self, pc: usize, length: usize) -> Result<(), String> {
        let next = pc + length;
        match self.blocks.get(&next) {
            Some(&block) => {
                self.builder.ins().jump(block, &[]);
                Ok(())
            }
            None => {
                // Off the end of the code: leave it to the interpreter.
                self.exit(next, STOP_OTHER);
                Ok(())
            }
        }
    }

    /// A conditional branch at `pc`: to its target when `taken`, else to the next instruction.
    fn branch(&mut self, pc: usize, length: usize, taken: Value) -> Result<(), String> {
        self.count_instruction();
        let target = (pc as isize + isize::from(read_i16(self.code, pc + 1))) as usize;
        let taken_block = self.builder.create_block();
        let not_taken = self.builder.create_block();
        self.builder
            .ins()
            .brif(taken, taken_block, &[], not_taken, &[]);
        self.builder.switch_to_block(taken_block);
        self.goto(pc, target)?;
        self.builder.switch_to_block(not_taken);
        self.fall_through(pc, length)
    }

    /// Has the frame-local loop run the instruction at `pc`, and goes on after it if it ran.
    fn step(&mut self, pc: usize, length: usize) -> Result<(), String> {
        self.write_back_top();
        let pc_value = self.builder.ins().iconst(I32, pc as i64);
        self.store_io(offset_of!(JitIo, pc), pc_value);
        let call = self.builder.ins().call(self.step, &[self.io]);
        let ran = self.builder.inst_results(call)[0];
        let ran_block = self.builder.create_block();
        let declined = self.builder.create_block();
        self.builder.ins().brif(ran, ran_block, &[], declined, &[]);
        // Declined: the step wrote the pc, the top and why; only the count is left.
        self.builder.switch_to_block(declined);
        let done = self.builder.use_var(self.done);
        self.store_io(offset_of!(JitIo, done), done);
        self.builder.ins().return_(&[]);
        self.builder.switch_to_block(ran_block);
        let top = self.load_io(offset_of!(JitIo, top));
        let top = self.builder.ins().uextend(I64, top);
        self.builder.def_var(self.top, top);
        self.count_instruction();
        self.fall_through(pc, length)
    }

    fn int_binary(&mut self, op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value) {
        let b = self.pop();
        let a = self.pop();
        let result = op(&mut self.builder, a, b);
        self.push(result);
    }

    fn long_binary(&mut self, op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value) {
        let b = self.pop_long();
        let a = self.pop_long();
        let result = op(&mut self.builder, a, b);
        self.push_long(result);
    }

    fn long_shift(&mut self, op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value) {
        let b = self.pop();
        let b = self.builder.ins().band_imm(b, 63);
        let b = self.builder.ins().uextend(I64, b);
        let a = self.pop_long();
        let result = op(&mut self.builder, a, b);
        self.push_long(result);
    }

    fn float_binary(&mut self, op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value) {
        let b = self.pop_float();
        let a = self.pop_float();
        let result = op(&mut self.builder, a, b);
        self.push_float(result);
    }

    fn double_binary(&mut self, op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value) {
        let b = self.pop_double();
        let a = self.pop_double();
        let result = op(&mut self.builder, a, b);
        self.push_double(result);
    }

    /// A floating-point addition or multiplication of `a` and `b`, the second being the top of
    /// the operand stack.
    ///
    /// Of two NaN operands, an x86-64 processor returns the first of the machine instruction,
    /// and HotSpot's interpreter has the top of the stack there (see `float_arithmetic`). The
    /// compiler keeps the operands of these instructions in the order it is given them, unless
    /// the first is loaded from memory as a float, which the frame's slots never are; the
    /// `JitOpcodes` test would show if that changed.
    fn commutative(
        builder: &mut FunctionBuilder<'b>,
        a: Value,
        b: Value,
        op: impl FnOnce(&mut FunctionBuilder<'b>, Value, Value) -> Value,
    ) -> Value {
        if cfg!(target_arch = "x86_64") {
            op(builder, b, a)
        } else {
            op(builder, a, b)
        }
    }

    /// `idiv`/`irem`/`ldiv`/`lrem` once the divisor is known not to be zero: Java's result
    /// for the one overflowing case (the minimum divided by -1), where the machine would trap.
    fn divide(
        builder: &mut FunctionBuilder<'b>,
        ty: Type,
        a: Value,
        b: Value,
        remainder: bool,
    ) -> Value {
        let minus_one = builder.ins().icmp_imm(IntCC::Equal, b, -1);
        let one = builder.ins().iconst(ty, 1);
        let divisor = builder.ins().select(minus_one, one, b);
        if remainder {
            let rem = builder.ins().srem(a, divisor);
            let zero = builder.ins().iconst(ty, 0);
            builder.ins().select(minus_one, zero, rem)
        } else {
            let quotient = builder.ins().sdiv(a, divisor);
            let negated = builder.ins().ineg(a);
            builder.ins().select(minus_one, negated, quotient)
        }
    }

    /// `fcmpl`/`fcmpg`/`dcmpl`/`dcmpg`: 1, 0 or -1, and `nan` when either value is NaN.
    fn float_compare(&mut self, a: Value, b: Value, nan: i64) -> Value {
        let ins = self.builder.ins();
        let greater = ins.fcmp(FloatCC::GreaterThan, a, b);
        let ins = self.builder.ins();
        let less = ins.fcmp(FloatCC::LessThan, a, b);
        let equal = self.builder.ins().fcmp(FloatCC::Equal, a, b);
        let one = self.builder.ins().iconst(I32, 1);
        let minus_one = self.builder.ins().iconst(I32, -1);
        let zero = self.builder.ins().iconst(I32, 0);
        let nan = self.builder.ins().iconst(I32, nan);
        let result = self.builder.ins().select(equal, zero, nan);
        let result = self.builder.ins().select(less, minus_one, result);
        self.builder.ins().select(greater, one, result)
    }

    fn boolean(&mut self, condition: Value) -> Value {
        // Comparisons produce an `i8`; branches take any integer.
        debug_assert_eq!(self.builder.func.dfg.value_type(condition), I8);
        condition
    }

    fn instruction(&mut self, pc: usize, length: usize) -> Result<(), String> {
        let code = self.code;
        let opcode = code[pc];
        match opcode {
            // nop
            0 => {}
            // aconst_null
            1 => {
                self.room(1, pc);
                let null = self.builder.ins().iconst(I32, 0);
                self.push_tagged(null, TAG_REF);
            }
            // iconst_m1 .. iconst_5
            2..=8 => {
                self.room(1, pc);
                let value = self.builder.ins().iconst(I32, i64::from(opcode) - 3);
                self.push(value);
            }
            // lconst_0, lconst_1
            9 | 10 => {
                self.room(2, pc);
                let value = self.builder.ins().iconst(I64, i64::from(opcode - 9));
                self.push_long(value);
            }
            // fconst_0 .. fconst_2
            11..=13 => {
                self.room(1, pc);
                let bits = f32::from(opcode - 11).to_bits();
                let value = self.builder.ins().iconst(I32, i64::from(bits));
                self.push(value);
            }
            // dconst_0, dconst_1
            14 | 15 => {
                self.room(2, pc);
                let bits = f64::from(opcode - 14).to_bits();
                let value = self.builder.ins().iconst(I64, bits as i64);
                self.push_long(value);
            }
            // bipush
            16 => {
                self.room(1, pc);
                let value = self
                    .builder
                    .ins()
                    .iconst(I32, i64::from(code[pc + 1] as i8));
                self.push(value);
            }
            // sipush
            17 => {
                self.room(1, pc);
                let value = self
                    .builder
                    .ins()
                    .iconst(I32, i64::from(read_i16(code, pc + 1)));
                self.push(value);
            }
            // iload, fload, and their _0 .. _3 forms
            21 | 23 | 26..=29 | 34..=37 => {
                let index = match opcode {
                    21 | 23 => usize::from(code[pc + 1]),
                    26..=29 => usize::from(opcode - 26),
                    _ => usize::from(opcode - 34),
                };
                self.room(1, pc);
                let value = self.local(index);
                self.push(value);
            }
            // lload, dload, and their _0 .. _3 forms
            22 | 24 | 30..=33 | 38..=41 => {
                let index = match opcode {
                    22 | 24 => usize::from(code[pc + 1]),
                    30..=33 => usize::from(opcode - 30),
                    _ => usize::from(opcode - 38),
                };
                self.room(2, pc);
                let value = self.local_long(index);
                self.push_long(value);
            }
            // aload, aload_0 .. aload_3: the slot as it is, tag included
            25 | 42..=45 => {
                let index = if opcode == 25 {
                    usize::from(code[pc + 1])
                } else {
                    usize::from(opcode - 42)
                };
                self.room(1, pc);
                let from = self.local_addr(index);
                let slot = self.read_slot(from);
                let to = self.stack_addr(0);
                self.copy_slot_to(to, slot);
                self.adjust_top(1);
            }
            // istore, fstore, and their _0 .. _3 forms
            54 | 56 | 59..=62 | 67..=70 => {
                let index = match opcode {
                    54 | 56 => usize::from(code[pc + 1]),
                    59..=62 => usize::from(opcode - 59),
                    _ => usize::from(opcode - 67),
                };
                let value = self.pop();
                self.set_local(index, value);
            }
            // lstore, dstore, and their _0 .. _3 forms
            55 | 57 | 63..=66 | 71..=74 => {
                let index = match opcode {
                    55 | 57 => usize::from(code[pc + 1]),
                    63..=66 => usize::from(opcode - 63),
                    _ => usize::from(opcode - 71),
                };
                let value = self.pop_long();
                self.set_local_long(index, value);
            }
            // astore, astore_0 .. astore_3: the slot as it is
            58 | 75..=78 => {
                let index = if opcode == 58 {
                    usize::from(code[pc + 1])
                } else {
                    usize::from(opcode - 75)
                };
                let from = self.stack_addr(1);
                let slot = self.read_slot(from);
                let to = self.local_addr(index);
                self.copy_slot_to(to, slot);
                self.adjust_top(-1);
            }
            // pop
            87 => self.adjust_top(-1),
            // pop2
            88 => self.adjust_top(-2),
            // dup, dup_x1, dup_x2, dup2, dup2_x1, dup2_x2, swap: rearrange whole slots
            89..=95 => {
                // How many slots the instruction reads from the top, and which of them (by
                // depth, 1 the top) it leaves from the bottom of its window up.
                let (read, layout): (i64, &[i64]) = match opcode {
                    89 => (1, &[1, 1]),
                    90 => (2, &[1, 2, 1]),
                    91 => (3, &[1, 3, 2, 1]),
                    92 => (2, &[2, 1, 2, 1]),
                    93 => (3, &[2, 1, 3, 2, 1]),
                    94 => (4, &[2, 1, 4, 3, 2, 1]),
                    _ => (2, &[1, 2]),
                };
                let grow = layout.len() as i64 - read;
                if grow > 0 {
                    self.room(grow, pc);
                }
                let mut slots = Vec::with_capacity(read as usize);
                for depth in 1..=read {
                    let addr = self.stack_addr(depth);
                    slots.push(self.read_slot(addr));
                }
                // `slots[d - 1]` is the slot at depth `d`; write the window from its bottom.
                for (i, &depth) in layout.iter().enumerate() {
                    let addr = self.stack_addr(read - i as i64);
                    self.copy_slot_to(addr, slots[depth as usize - 1]);
                }
                self.adjust_top(grow);
            }
            96 => self.int_binary(|b, x, y| b.ins().iadd(x, y)),
            97 => self.long_binary(|b, x, y| b.ins().iadd(x, y)),
            98 => self
                .float_binary(|b, x, y| Self::commutative(b, x, y, |b, x, y| b.ins().fadd(x, y))),
            99 => self
                .double_binary(|b, x, y| Self::commutative(b, x, y, |b, x, y| b.ins().fadd(x, y))),
            100 => self.int_binary(|b, x, y| b.ins().isub(x, y)),
            101 => self.long_binary(|b, x, y| b.ins().isub(x, y)),
            102 => self.float_binary(|b, x, y| b.ins().fsub(x, y)),
            103 => self.double_binary(|b, x, y| b.ins().fsub(x, y)),
            104 => self.int_binary(|b, x, y| b.ins().imul(x, y)),
            105 => self.long_binary(|b, x, y| b.ins().imul(x, y)),
            106 => self
                .float_binary(|b, x, y| Self::commutative(b, x, y, |b, x, y| b.ins().fmul(x, y))),
            107 => self
                .double_binary(|b, x, y| Self::commutative(b, x, y, |b, x, y| b.ins().fmul(x, y))),
            // idiv, irem: a zero divisor throws, which the handler does
            108 | 112 => {
                let divisor = self.peek(1);
                let zero = self.builder.ins().icmp_imm(IntCC::Equal, divisor, 0);
                self.exit_if(zero, pc);
                let remainder = opcode == 112;
                self.int_binary(|b, x, y| Self::divide(b, I32, x, y, remainder));
            }
            // ldiv, lrem
            109 | 113 => {
                let high = self.peek(1);
                let low = self.peek(2);
                let either = self.builder.ins().bor(high, low);
                let zero = self.builder.ins().icmp_imm(IntCC::Equal, either, 0);
                self.exit_if(zero, pc);
                let remainder = opcode == 113;
                self.long_binary(|b, x, y| Self::divide(b, I64, x, y, remainder));
            }
            110 => self.float_binary(|b, x, y| b.ins().fdiv(x, y)),
            111 => self.double_binary(|b, x, y| b.ins().fdiv(x, y)),
            // ineg, lneg, fneg, dneg
            116 => {
                let value = self.pop();
                let result = self.builder.ins().ineg(value);
                self.push(result);
            }
            117 => {
                let value = self.pop_long();
                let result = self.builder.ins().ineg(value);
                self.push_long(result);
            }
            118 => {
                let value = self.pop_float();
                let result = self.builder.ins().fneg(value);
                self.push_float(result);
            }
            119 => {
                let value = self.pop_double();
                let result = self.builder.ins().fneg(value);
                self.push_double(result);
            }
            // Shifts: Cranelift, like Java, takes the distance modulo the width.
            120 => self.int_binary(|b, x, y| b.ins().ishl(x, y)),
            121 => self.long_shift(|b, x, y| b.ins().ishl(x, y)),
            122 => self.int_binary(|b, x, y| b.ins().sshr(x, y)),
            123 => self.long_shift(|b, x, y| b.ins().sshr(x, y)),
            124 => self.int_binary(|b, x, y| b.ins().ushr(x, y)),
            125 => self.long_shift(|b, x, y| b.ins().ushr(x, y)),
            126 => self.int_binary(|b, x, y| b.ins().band(x, y)),
            127 => self.long_binary(|b, x, y| b.ins().band(x, y)),
            128 => self.int_binary(|b, x, y| b.ins().bor(x, y)),
            129 => self.long_binary(|b, x, y| b.ins().bor(x, y)),
            130 => self.int_binary(|b, x, y| b.ins().bxor(x, y)),
            131 => self.long_binary(|b, x, y| b.ins().bxor(x, y)),
            // iinc
            132 => {
                let index = usize::from(code[pc + 1]);
                let delta = i64::from(code[pc + 2] as i8);
                let value = self.local(index);
                let value = self.builder.ins().iadd_imm(value, delta);
                self.set_local(index, value);
            }
            // i2l
            133 => {
                self.room(1, pc);
                let value = self.pop();
                let value = self.builder.ins().sextend(I64, value);
                self.push_long(value);
            }
            // i2f
            134 => {
                let value = self.pop();
                let value = self.builder.ins().fcvt_from_sint(F32, value);
                self.push_float(value);
            }
            // i2d
            135 => {
                self.room(1, pc);
                let value = self.pop();
                let value = self.builder.ins().fcvt_from_sint(F64, value);
                self.push_double(value);
            }
            // l2i
            136 => {
                let value = self.pop_long();
                let value = self.builder.ins().ireduce(I32, value);
                self.push(value);
            }
            // l2f
            137 => {
                let value = self.pop_long();
                let value = self.builder.ins().fcvt_from_sint(F32, value);
                self.push_float(value);
            }
            // l2d
            138 => {
                let value = self.pop_long();
                let value = self.builder.ins().fcvt_from_sint(F64, value);
                self.push_double(value);
            }
            // f2i: saturating, NaN to 0, as Java (and Rust's `as`) convert
            139 => {
                let value = self.pop_float();
                let value = self.builder.ins().fcvt_to_sint_sat(I32, value);
                self.push(value);
            }
            // f2l
            140 => {
                self.room(1, pc);
                let value = self.pop_float();
                let value = self.builder.ins().fcvt_to_sint_sat(I64, value);
                self.push_long(value);
            }
            // f2d
            141 => {
                self.room(1, pc);
                let value = self.pop_float();
                let value = self.builder.ins().fpromote(F64, value);
                self.push_double(value);
            }
            // d2i
            142 => {
                let value = self.pop_double();
                let value = self.builder.ins().fcvt_to_sint_sat(I32, value);
                self.push(value);
            }
            // d2l
            143 => {
                let value = self.pop_double();
                let value = self.builder.ins().fcvt_to_sint_sat(I64, value);
                self.push_long(value);
            }
            // d2f
            144 => {
                let value = self.pop_double();
                let value = self.builder.ins().fdemote(F32, value);
                self.push_float(value);
            }
            // i2b, i2c, i2s
            145..=147 => {
                let value = self.pop();
                let narrow = if opcode == 145 {
                    I8
                } else {
                    cranelift_codegen::ir::types::I16
                };
                let value = self.builder.ins().ireduce(narrow, value);
                let value = if opcode == 146 {
                    self.builder.ins().uextend(I32, value)
                } else {
                    self.builder.ins().sextend(I32, value)
                };
                self.push(value);
            }
            // lcmp
            148 => {
                let b = self.pop_long();
                let a = self.pop_long();
                let greater = self.builder.ins().icmp(IntCC::SignedGreaterThan, a, b);
                let less = self.builder.ins().icmp(IntCC::SignedLessThan, a, b);
                let greater = self.builder.ins().uextend(I32, greater);
                let less = self.builder.ins().uextend(I32, less);
                let result = self.builder.ins().isub(greater, less);
                self.push(result);
            }
            // fcmpl, fcmpg
            149 | 150 => {
                let b = self.pop_float();
                let a = self.pop_float();
                let result = self.float_compare(a, b, if opcode == 150 { 1 } else { -1 });
                self.push(result);
            }
            // dcmpl, dcmpg
            151 | 152 => {
                let b = self.pop_double();
                let a = self.pop_double();
                let result = self.float_compare(a, b, if opcode == 152 { 1 } else { -1 });
                self.push(result);
            }
            // ifeq .. ifle
            153..=158 => {
                let value = self.pop();
                let cc = match opcode {
                    153 => IntCC::Equal,
                    154 => IntCC::NotEqual,
                    155 => IntCC::SignedLessThan,
                    156 => IntCC::SignedGreaterThanOrEqual,
                    157 => IntCC::SignedGreaterThan,
                    _ => IntCC::SignedLessThanOrEqual,
                };
                let taken = self.builder.ins().icmp_imm(cc, value, 0);
                let taken = self.boolean(taken);
                return self.branch(pc, length, taken);
            }
            // if_icmpeq .. if_icmple, if_acmpeq, if_acmpne
            159..=166 => {
                let b = self.pop();
                let a = self.pop();
                let cc = match opcode {
                    159 | 165 => IntCC::Equal,
                    160 | 166 => IntCC::NotEqual,
                    161 => IntCC::SignedLessThan,
                    162 => IntCC::SignedGreaterThanOrEqual,
                    163 => IntCC::SignedGreaterThan,
                    _ => IntCC::SignedLessThanOrEqual,
                };
                let taken = self.builder.ins().icmp(cc, a, b);
                return self.branch(pc, length, taken);
            }
            // goto
            167 => {
                self.count_instruction();
                let target = (pc as isize + isize::from(read_i16(code, pc + 1))) as usize;
                return self.goto(pc, target);
            }
            // goto_w
            200 => {
                self.count_instruction();
                let offset = read_i32(code, pc + 1).ok_or("truncated goto_w")?;
                let target = (pc as isize + offset as isize) as usize;
                return self.goto(pc, target);
            }
            // ifnull, ifnonnull
            198 | 199 => {
                let value = self.pop();
                let cc = if opcode == 198 {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let taken = self.builder.ins().icmp_imm(cc, value, 0);
                return self.branch(pc, length, taken);
            }
            // tableswitch, lookupswitch
            170 | 171 => return self.switch(pc, opcode),
            // ireturn, lreturn, freturn, dreturn, areturn, return
            172..=177 => {
                let slots = match opcode {
                    173 | 175 => 2,
                    177 => 0,
                    _ => 1,
                };
                self.exit(pc, STOP_RETURN + slots);
                return Ok(());
            }
            // invokevirtual, invokespecial, invokestatic, invokeinterface
            182..=185 => {
                self.exit(pc, STOP_INVOKE);
                return Ok(());
            }
            // The frame-local instructions that read the heap or the class's caches.
            18..=20 | 46..=53 | 79..=86 | 178..=181 | 190 | 192 | 193 => {
                return self.step(pc, length);
            }
            // Everything else is for its handler.
            _ => {
                self.exit(pc, STOP_OTHER);
                return Ok(());
            }
        }
        self.count_instruction();
        self.fall_through(pc, length)
    }

    fn switch(&mut self, pc: usize, opcode: u8) -> Result<(), String> {
        let code = self.code;
        let base = (pc + 4) & !3;
        let default = read_i32(code, base).ok_or("truncated switch")?;
        let mut targets: Vec<(i64, usize)> = Vec::new();
        if opcode == 170 {
            let low = read_i32(code, base + 4).ok_or("truncated tableswitch")?;
            let high = read_i32(code, base + 8).ok_or("truncated tableswitch")?;
            for (i, key) in (i64::from(low)..=i64::from(high)).enumerate() {
                let offset = read_i32(code, base + 12 + i * 4).ok_or("truncated tableswitch")?;
                targets.push((key, (pc as isize + offset as isize) as usize));
            }
        } else {
            let pairs = read_i32(code, base + 4).ok_or("truncated lookupswitch")?;
            for i in 0..pairs.max(0) as usize {
                let key = read_i32(code, base + 8 + i * 8).ok_or("truncated lookupswitch")?;
                let offset = read_i32(code, base + 12 + i * 8).ok_or("truncated lookupswitch")?;
                targets.push((i64::from(key), (pc as isize + offset as isize) as usize));
            }
        }
        let default_target = (pc as isize + default as isize) as usize;
        let key = self.pop();
        self.count_instruction();
        // Each target gets a block of its own that checks the budget if it branches back.
        let mut switch = Switch::new();
        let mut landing: BTreeMap<usize, Block> = BTreeMap::new();
        for &(_, target) in targets.iter().chain(std::iter::once(&(0, default_target))) {
            if !landing.contains_key(&target) {
                self.block_at(target)?;
                landing.insert(target, self.builder.create_block());
            }
        }
        for &(key, target) in &targets {
            // `Switch` takes unsigned keys: the key's 32 bits, as the operand's.
            switch.set_entry(u128::from(key as i32 as u32), landing[&target]);
        }
        let key = self.builder.ins().uextend(I64, key);
        switch.emit(&mut self.builder, key, landing[&default_target]);
        for (target, block) in landing {
            self.builder.switch_to_block(block);
            self.goto(pc, target)?;
        }
        Ok(())
    }
}
