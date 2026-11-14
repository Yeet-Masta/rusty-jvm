use crate::vm::error::{Error, Result};
use crate::vm::exception::class_cast::throw_class_cast_exception;
use crate::vm::exception::common::throw_exception_with_ref;
use crate::vm::exception::helpers::throw_null_pointer_exception;
use crate::vm::execution_engine::common::{last_frame_mut, push_words, store_ex_pc};
use crate::vm::execution_engine::invoker::{invoke, SignatureOrigin};
use crate::vm::execution_engine::opcode::*;
use crate::vm::execution_engine::static_init::StaticInit;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{argument_length, get_length, is_reference};
use crate::vm::method_area::instance_checker::InstanceChecker;
use crate::vm::method_area::java_class::{
    InstanceField, JavaClass, MemberRef, MemberRefKind, StaticField,
};
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::lookup;
use crate::vm::method_area::method_area::with_method_area;
use crate::vm::monitor;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frame::{StackFrame, StackFrames};
use jclassmodel::constant_pool::ConstantPoolLookup;
use jdescriptor::MethodDescriptor;
use std::sync::Arc;
use tracing::trace;

pub(crate) fn process(
    code: u8,
    current_class_name: &str,
    current_class: &Arc<JavaClass>,
    stack_frames: &mut StackFrames,
) -> Result<()> {
    match code {
        GETSTATIC => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let member = get_field_info(stack_frame, current_class)?;
            let field = static_field(member)?;
            StaticInit::initialize_java_class(&field.holder)?;

            let words = field.value.words()?;
            push_words(stack_frame, words, field.wide, field.reference)?;

            stack_frame.incr_pc();
            trace!(
                "GETSTATIC -> {}.{} is {words:?}",
                member.class_name,
                member.name
            );
        }
        PUTSTATIC => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let member = get_field_info(stack_frame, current_class)?;
            let field = static_field(member)?;
            // The operands stay on the stack, and so rooted, while the class initializes.
            StaticInit::initialize_java_class(&field.holder)?;

            let words = pop_words(stack_frame, field.wide);
            field.value.set_words(words)?;

            stack_frame.incr_pc();
            trace!(
                "PUTSTATIC -> {}.{} = {words:?}",
                member.class_name,
                member.name
            );
        }
        GETFIELD => {
            let (member, field, objectref) = {
                let stack_frame = last_frame_mut(stack_frames)?;
                let member = get_field_info(stack_frame, current_class)?;
                // Metadata lookup can wait for class publication and cross a GC stop.
                // Keep the receiver on the frame until it completes.
                let field = instance_field(member)?;
                let objectref: i32 = stack_frame.pop();
                (member, field, objectref)
            };
            if objectref == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            let words =
                HEAP.read_field(objectref, &member.class_name, &member.name, &field.position)?;

            let stack_frame = last_frame_mut(stack_frames)?;
            push_words(stack_frame, words, field.wide, field.reference)?;

            stack_frame.incr_pc();
            trace!(
                "GETFIELD -> objectref={objectref}, class_name={}, field_name={}, value={words:?}",
                member.class_name,
                member.name
            );
        }
        PUTFIELD => {
            let (member, field, words, objectref) = {
                let stack_frame = last_frame_mut(stack_frames)?;
                let member = get_field_info(stack_frame, current_class)?;
                let field = instance_field(member)?;
                let words = pop_words(stack_frame, field.wide);
                let objectref: i32 = stack_frame.pop();
                (member, field, words, objectref)
            };
            if objectref == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            HEAP.write_field(
                objectref,
                &member.class_name,
                &member.name,
                &field.position,
                words,
            )?;

            trace!(
                "PUTFIELD -> objectref={objectref}, class_name={}, field_name={} value={words:?}",
                member.class_name,
                member.name
            );
            last_frame_mut(stack_frames)?.incr_pc();
        }
        INVOKEVIRTUAL => {
            store_ex_pc(stack_frames)?;
            let member =
                get_class_name_and_signature(stack_frames, current_class, MemberRefKind::Method)?;
            let (class_name_by_ref_type, full_signature) =
                (&member.class_name, &member.full_signature);
            let method_args =
                prepare_invoke_context(stack_frames, member.method_descriptor()?, true)?;
            let reference = method_args
                .first()
                .ok_or_else(|| Error::new_execution("Error getting reference from method_args"))?;

            if reference.value() == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            let exact_implementation =
                virtual_target(member, reference.value(), "virtual_method")?;

            let class_name = exact_implementation.class_name();
            invoke(
                stack_frames,
                &full_signature,
                SignatureOrigin::CallSite,
                &method_args,
                Arc::clone(&exact_implementation),
                class_name,
            )?;
            trace!("INVOKEVIRTUAL -> invoked {class_name}.{full_signature}({method_args:?}) via {class_name_by_ref_type}.{full_signature}");
        }
        INVOKESPECIAL => {
            store_ex_pc(stack_frames)?;
            let member =
                get_class_name_and_signature(stack_frames, current_class, MemberRefKind::Method)?;
            let (class_name_to_start_lookup_from, full_signature) =
                (&member.class_name, &member.full_signature);
            let java_method = match member.special_target.get() {
                Some(cached) => Arc::clone(cached),
                None => {
                    let resolved = lookup::lookup_instance_method(&class_name_to_start_lookup_from, &full_signature)?
                        .ok_or_else(|| Error::new_constant_pool(&format!("Error getting instance type JavaMethod by class name {class_name_to_start_lookup_from} and full signature {full_signature} calling invokespecial")))?;
                    Arc::clone(member.special_target.get_or_init(|| resolved))
                }
            };
            let method_args =
                prepare_invoke_context(stack_frames, java_method.get_method_descriptor(), true)?;
            let class_name = java_method.class_name();
            invoke(
                stack_frames,
                &full_signature,
                SignatureOrigin::CallSite,
                &method_args,
                Arc::clone(&java_method),
                class_name,
            )?;
            trace!("INVOKESPECIAL -> {class_name}.{full_signature}({method_args:?})");
        }
        INVOKESTATIC => {
            store_ex_pc(stack_frames)?;
            let member =
                get_class_name_and_signature(stack_frames, current_class, MemberRefKind::Method)?;
            let (class_name_to_start_lookup_from, full_signature) =
                (&member.class_name, &member.full_signature);
            let java_method = match member.static_target.get() {
                Some((klass, cached)) => {
                    // Still required on every call: a no-op once the class is initialized, and
                    // it rethrows for a class whose initialization failed.
                    StaticInit::initialize_java_class(klass)?;
                    Arc::clone(cached)
                }
                None => {
                    let klass = CLASSES.get(&class_name_to_start_lookup_from)?;
                    StaticInit::initialize_java_class(&klass)?;
                    let resolved = lookup::lookup_static_method(&class_name_to_start_lookup_from, &full_signature)?
                        .ok_or_else(|| Error::new_constant_pool(&format!("Error getting static type JavaMethod by class name {class_name_to_start_lookup_from} and full signature {full_signature} calling invokestatic")))?;
                    Arc::clone(&member.static_target.get_or_init(|| (klass, resolved)).1)
                }
            };
            let method_args =
                prepare_invoke_context(stack_frames, java_method.get_method_descriptor(), false)?;
            invoke(
                stack_frames,
                &full_signature,
                SignatureOrigin::CallSite,
                &method_args,
                Arc::clone(&java_method),
                &class_name_to_start_lookup_from,
            )?;
            trace!("INVOKESTATIC -> {class_name_to_start_lookup_from}.{full_signature}({method_args:?})");
        }
        INVOKEINTERFACE => {
            store_ex_pc(stack_frames)?;
            let index = last_frame_mut(stack_frames)?.extract_two_bytes() as u16;
            let arg_num = last_frame_mut(stack_frames)?.extract_one_byte() as usize;
            let method_args = get_args(stack_frames, arg_num)?;

            let zero = last_frame_mut(stack_frames)?.extract_one_byte();
            last_frame_mut(stack_frames)?.incr_pc();
            if zero != 0 {
                return Err(Error::new_execution(&format!(
                    "Error calling interface method by index {index}"
                )));
            }

            let member = current_class.member_ref(MemberRefKind::InterfaceMethod, index)?;
            let full_signature = &member.full_signature;
            let reference = method_args
                .first()
                .ok_or_else(|| Error::new_execution("Error getting reference from method_args"))?;

            if reference.value() == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            let java_method =
                virtual_target(member, reference.value(), "interface implementation")?;

            let exact_class_name = java_method.class_name();
            invoke(
                stack_frames,
                &full_signature,
                SignatureOrigin::CallSite,
                &method_args,
                Arc::clone(&java_method),
                exact_class_name,
            )?;
            trace!("INVOKEINTERFACE -> {exact_class_name}.{full_signature}({method_args:?})");
        }
        INVOKEDYNAMIC => {
            store_ex_pc(stack_frames)?;
            let stack_frame = last_frame_mut(stack_frames)?;
            let invokedynamic_index = stack_frame.extract_two_bytes() as u16;
            stack_frame.incr_pc();

            let klass = Arc::clone(current_class);
            let invoke_dynamic_runner = klass.invoke_dynamic_runner();
            invoke_dynamic_runner
                .run(stack_frames, current_class_name, invokedynamic_index)
                .map_err(|e| {
                    if e.is_uncaught_exception() {
                        return e;
                    }
                    Error::new_execution(&format!(
                        "Error running invokedynamic for class {current_class_name} on index {invokedynamic_index}: {e}"
                    ))
                })?;

            trace!("INVOKEDYNAMIC -> {current_class_name} on index {invokedynamic_index}");
        }
        NEW => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let class_constpool_index = stack_frame.extract_two_bytes() as u16;

            let klass = Arc::clone(current_class);
            let constant_pool = klass.constant_pool();

            let class_to_invoke_new_for = constant_pool
                .get_class_name(class_constpool_index)
                .ok_or_else(|| {
                    Error::new_constant_pool(&format!(
                        "Error getting class name by index {class_constpool_index}"
                    ))
                })?;
            let class_to_invoke_new_for = klass.resolve_class_name(&class_to_invoke_new_for)?;

            StaticInit::initialize(&class_to_invoke_new_for)?;

            let instance_with_default_fields = with_method_area(|method_area| {
                method_area.create_instance_with_default_fields(&class_to_invoke_new_for)
            })?;

            let mut destination = RootedSlots::new();
            let instanceref =
                HEAP.create_instance(instance_with_default_fields, &mut destination)?;
            stack_frame.push(Slot::Ref(instanceref))?;

            trace!("NEW -> class={class_to_invoke_new_for}, reference={instanceref}");
            stack_frame.incr_pc();
        }
        NEWARRAY => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let atype = stack_frame.extract_one_byte();

            let type_name = match atype {
                4 => "[Z",
                5 => "[C",
                6 => "[F",
                7 => "[D",
                8 => "[B",
                9 => "[S",
                10 => "[I",
                11 => "[J",
                _ => {
                    return Err(Error::new_execution(&format!(
                        "Error creating array of type {atype}"
                    )))
                }
            };

            let length = stack_frame.pop();

            let mut destination = RootedSlots::new();
            let arrayref = HEAP.create_array(type_name, length, &mut destination)?;
            stack_frame.push(Slot::Ref(arrayref))?;

            stack_frame.incr_pc();
            trace!("NEWARRAY -> atype={atype}, length={length}, arrayref={arrayref}");
        }
        ANEWARRAY => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let length = stack_frame.pop();

            let class_constpool_index = stack_frame.extract_two_bytes() as u16;
            let klass = Arc::clone(current_class);
            let constant_pool = klass.constant_pool();

            let class_of_array = constant_pool
                .get_class_name(class_constpool_index)
                .ok_or_else(|| {
                    Error::new_constant_pool(&format!(
                        "Error getting class name by index {class_constpool_index}"
                    ))
                })?;
            let class_of_array = klass.resolve_class_name(&class_of_array)?;
            let class_of_array = if class_of_array.starts_with("[") {
                format!("[{class_of_array}")
            } else {
                format!("[L{class_of_array};")
            };
            let mut destination = RootedSlots::new();
            let arrayref = HEAP.create_array(&class_of_array, length, &mut destination)?;
            stack_frame.push(Slot::Ref(arrayref))?;

            stack_frame.incr_pc();
            trace!("ANEWARRAY -> class_of_array={class_of_array}, length={length}, arrayref={arrayref}");
        }
        ARRAYLENGTH => {
            let arrayref = {
                let stack_frame = last_frame_mut(stack_frames)?;
                stack_frame.pop()
            };

            let Some(len) = HEAP.get_array_len_throwing(arrayref, stack_frames)? else {
                return Ok(());
            };

            let stack_frame = last_frame_mut(stack_frames)?;
            stack_frame.push(len)?;
            stack_frame.incr_pc();
            trace!("ARRAYLENGTH -> arrayref={arrayref}, len={len}");
        }
        ATHROW => {
            store_ex_pc(stack_frames)?;
            let mut destination = RootedSlots::new();
            let throwable_ref = {
                let stack_frame = last_frame_mut(stack_frames)?;
                let throwable_ref: Slot = stack_frame.pop();
                throwable_ref
            };
            destination.replace(vec![throwable_ref]);

            if throwable_ref.value() == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            let (exception_name, found_exception_handler) =
                throw_exception_with_ref(throwable_ref, stack_frames)?;
            trace!("ATHROW -> throwable_ref={throwable_ref}, exception_name={exception_name}, found_exception_handler={found_exception_handler}");
        }
        CHECKCAST => {
            let (operand, class_constpool_index) = {
                let stack_frame = last_frame_mut(stack_frames)?;
                let class_constpool_index = stack_frame.extract_two_bytes() as u16;
                // Keep the operand rooted while the lookup below may load classes or run Java.
                let mut operand = RootedSlots::new();
                operand.replace(vec![stack_frame.pop_slot()]);
                (operand, class_constpool_index)
            };
            let objectref = operand[0].value();

            if objectref != 0 {
                let klass = Arc::clone(current_class);
                let constant_pool = klass.constant_pool();
                let class_name = constant_pool
                    .get_class_name(class_constpool_index)
                    .ok_or_else(|| {
                        Error::new_constant_pool(&format!(
                            "Error getting class name by index {class_constpool_index}"
                        ))
                    })?;
                let class_name = klass.resolve_class_name(&class_name)?;

                let instance_class_name = HEAP.get_instance_name(objectref)?;

                let possible_cast = InstanceChecker::checkcast(&instance_class_name, &class_name)?;
                if !possible_cast {
                    // Thrown before moving past the instruction, so its own handler range applies
                    throw_class_cast_exception(&instance_class_name, &class_name, stack_frames)?;
                    return Ok(());
                }
            }

            let stack_frame = last_frame_mut(stack_frames)?;
            stack_frame.push(Slot::Ref(objectref))?;
            stack_frame.incr_pc();

            trace!("CHECKCAST -> class_constpool_index={class_constpool_index}, objectref={objectref}");
        }
        INSTANCEOF => {
            let stack_frame = last_frame_mut(stack_frames)?;
            // todo: merge me with CHECKCAST
            let class_constpool_index = stack_frame.extract_two_bytes() as u16;
            stack_frame.incr_pc();
            let mut operand = RootedSlots::new();
            operand.replace(vec![stack_frame.pop_slot()]);
            let objectref = operand[0].value();

            // Unlike CHECKCAST, this consumes the reference and yields a boolean, so the result is
            // pushed untagged however the operand was tagged.
            let instance_of = if objectref != 0 {
                let klass = Arc::clone(current_class);
                let constant_pool = klass.constant_pool();
                let class_name = constant_pool
                    .get_class_name(class_constpool_index)
                    .ok_or_else(|| {
                        Error::new_constant_pool(&format!(
                            "Error getting class name by index {class_constpool_index}"
                        ))
                    })?;
                let class_name = klass.resolve_class_name(&class_name)?;

                let instance_class_name = HEAP.get_instance_name(objectref)?;

                i32::from(InstanceChecker::checkcast(
                    &instance_class_name,
                    &class_name,
                )?)
            } else {
                0
            };

            stack_frame.push(instance_of)?;

            trace!("INSTANCEOF -> class_constpool_index={class_constpool_index}, objectref={objectref}, instance_of={instance_of}");
        }
        MONITORENTER => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let objectref: i32 = stack_frame.pop();
            if objectref == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }

            stack_frame.incr_pc();
            // Acquire after advancing pc; enter() may block this OS thread until the monitor is free.
            monitor::enter(objectref);
            trace!("MONITORENTER -> objectref={objectref}");
        }
        MONITOREXIT => {
            let stack_frame = last_frame_mut(stack_frames)?;
            let objectref: i32 = stack_frame.pop();
            if objectref == 0 {
                throw_null_pointer_exception(stack_frames)?;
                return Ok(());
            }
            stack_frame.incr_pc();
            monitor::exit(objectref)?;
            trace!("MONITOREXIT -> objectref={objectref}");
        }
        _ => {
            return Err(Error::new_execution(&format!(
                "Unknown reference opcode: {}",
                code
            )));
        }
    }

    Ok(())
}

pub fn prepare_invoke_context(
    stack_frames: &mut StackFrames,
    method_descriptor: &MethodDescriptor,
    use_self_ref: bool,
) -> Result<RootedSlots> {
    let arg_num = argument_length(method_descriptor.parameter_types())?;
    let arg_num = arg_num + if use_self_ref { 1 } else { 0 };

    get_args(stack_frames, arg_num as usize)
}

fn get_args(stack_frames: &mut StackFrames, arg_num: usize) -> Result<RootedSlots> {
    let mut destination = RootedSlots::new();
    let mut method_args = Vec::with_capacity(arg_num);
    for _ in 0..arg_num {
        let val = last_frame_mut(stack_frames)?.pop_slot();
        method_args.push(val);
    }
    method_args.reverse();

    destination.replace(method_args);
    Ok(destination)
}

/// Selects the implementation for a virtual or interface call on `receiver`, through the call
/// site's monomorphic inline cache when the receiver's class matches it.
fn virtual_target(member: &MemberRef, receiver: i32, what: &str) -> Result<Arc<JavaMethod>> {
    let klass_id = HEAP.instance_klass_id(receiver)?;
    if let (Some(id), Some((cached_id, cached))) = (klass_id, member.virtual_target.get()) {
        if id == *cached_id {
            return Ok(Arc::clone(cached));
        }
    }
    let instance_name = HEAP.get_instance_name(receiver)?;
    let resolved = lookup::lookup_instance_method(&instance_name, &member.full_signature)?
        .ok_or_else(|| {
            Error::new_constant_pool(&format!(
                "Error getting instance type JavaMethod by class name {instance_name} and full signature {} getting {what}",
                member.full_signature
            ))
        })?;
    if let Some(id) = klass_id {
        let _ = member.virtual_target.set((id, Arc::clone(&resolved)));
    }
    Ok(resolved)
}

fn get_class_name_and_signature<'a>(
    stack_frames: &mut StackFrames,
    current_class: &'a JavaClass,
    kind: MemberRefKind,
) -> Result<&'a MemberRef> {
    let stack_frame = last_frame_mut(stack_frames)?;
    let index = stack_frame.extract_two_bytes() as u16;
    stack_frame.incr_pc();

    current_class.member_ref(kind, index)
}

/// Pops a field value off the operand stack as JVM words: the high word of a wide value is on top.
fn pop_words(stack_frame: &mut StackFrame, wide: bool) -> [i32; 2] {
    let first: i32 = stack_frame.pop();
    if wide {
        [first, stack_frame.pop()]
    } else {
        [first, 0]
    }
}

/// The static field a `getstatic`/`putstatic` site refers to, resolved on first use.
fn static_field(member: &MemberRef) -> Result<&StaticField> {
    if let Some(resolved) = member.static_field.get() {
        return Ok(resolved);
    }
    let (class_name, field_name) = (&member.class_name, &member.name);
    let (holder_name, value) = lookup::lookup_for_static_field(class_name, field_name)?
        .ok_or_else(|| {
            Error::new_constant_pool(&format!("Static field {class_name}.{field_name} not found"))
        })?;
    let holder = CLASSES.get(&holder_name)?;
    let descriptor = holder
        .field_info(field_name)
        .ok_or_else(|| Error::new_execution("Error getting field info"))?
        .type_descriptor()
        .clone();
    let resolved = StaticField {
        holder,
        value,
        wide: get_length(&descriptor)? == 2,
        reference: is_reference(&descriptor),
    };
    Ok(member.static_field.get_or_init(|| resolved))
}

/// The instance field a `getfield`/`putfield` site refers to, resolved on first use.
fn instance_field(member: &MemberRef) -> Result<&InstanceField> {
    if let Some(resolved) = member.instance_field.get() {
        return Ok(resolved);
    }
    let (class_name, field_name) = (&member.class_name, &member.name);
    let descriptor =
        lookup::lookup_for_field_descriptor(class_name, field_name).ok_or_else(|| {
            Error::new_constant_pool(&format!(
                "Error getting type descriptor for {class_name}.{field_name}"
            ))
        })?;
    let resolved = InstanceField {
        wide: get_length(&descriptor)? == 2,
        reference: is_reference(&descriptor),
        position: std::sync::OnceLock::new(),
    };
    Ok(member.instance_field.get_or_init(|| resolved))
}

fn get_field_info<'a>(
    stack_frame: &mut StackFrame,
    current_class: &'a JavaClass,
) -> Result<&'a MemberRef> {
    let fieldref_constpool_index = stack_frame.extract_two_bytes() as u16;
    current_class.member_ref(MemberRefKind::Field, fieldref_constpool_index)
}

#[cfg(test)]
mod ref_transfer_tests {
    use super::*;
    use crate::vm::jni::java_thread::JavaThread;

    #[test]
    fn ref_transfer_mixed_arguments_keep_published_tags() {
        let object = HEAP
            .create_array(
                "[I",
                1,
                &mut crate::vm::stack::rooted_slots::RootedSlots::new(),
            )
            .unwrap();
        let expected = [
            Slot::Ref(object),
            Slot::Value(17),
            Slot::Value(0x1234),
            Slot::Value(0x5678),
            Slot::Ref(0),
        ];
        let mut caller = StackFrame::for_test(0, expected.len());
        for slot in expected {
            caller.push(slot).unwrap();
        }
        let mut frames = StackFrames::new(vec![caller]);

        let arguments = get_args(&mut frames, expected.len()).unwrap();
        let callee = StackFrame::new_native(
            Arc::new("mixed:(Ljava/lang/Object;IJ[Ljava/lang/Object;)V".into()),
            Arc::new("Test".into()),
            arguments.to_vec(),
        );

        assert_eq!(&*arguments, &expected);
        assert_eq!(JavaThread::temp_root_refs(), vec![object, 0]);
        assert_eq!(callee.ref_slots().collect::<Vec<_>>(), vec![object, 0]);
        assert_eq!(frames.last().unwrap().ref_slots().count(), 0);
    }

    #[test]
    fn ref_transfer_live_heap_key_remains_a_primitive_nonroot() {
        let live_key = HEAP
            .create_array(
                "[I",
                1,
                &mut crate::vm::stack::rooted_slots::RootedSlots::new(),
            )
            .unwrap();
        assert_eq!(HEAP.get_array_len(live_key).unwrap(), 1);
        let mut caller = StackFrame::for_test(0, 1);
        caller.push(live_key).unwrap();
        let mut frames = StackFrames::new(vec![caller]);

        let arguments = get_args(&mut frames, 1).unwrap();

        assert_eq!(arguments[0], Slot::Value(live_key));
        assert!(JavaThread::temp_root_refs().is_empty());
    }

    #[test]
    fn ref_transfer_athrow_keeps_the_original_throwable_tag() {
        let throwable = HEAP
            .create_array(
                "[Ljava/lang/Object;",
                0,
                &mut crate::vm::stack::rooted_slots::RootedSlots::new(),
            )
            .unwrap();
        let mut source = StackFrame::for_test(0, 1);
        source.push(Slot::Ref(throwable)).unwrap();
        let mut frames = StackFrames::new(vec![source]);

        let class = Arc::new(JavaClass::synthetic(
            "Test".to_string(),
            "Test".to_string(),
            None,
            indexmap::IndexSet::new(),
        ));
        let error = process(ATHROW, "Test", &class, &mut frames).unwrap_err();

        assert_eq!(error.throwable_ref(), Some(throwable));
        assert!(frames.is_empty());
        assert_eq!(JavaThread::temp_root_refs(), vec![throwable]);
    }
}
