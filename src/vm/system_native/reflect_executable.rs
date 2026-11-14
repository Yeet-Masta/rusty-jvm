//! Natives of `java.lang.reflect.Executable` and `java.lang.reflect.Field` that read class-file
//! metadata the reflection objects do not carry themselves.

use crate::vm::error::Result;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use std::sync::Arc;

fn method_of(executable_ref: i32) -> Result<Arc<JavaMethod>> {
    let class_name = HEAP.get_instance_name(executable_ref)?;
    let declaring_class = HEAP.get_object_field_value(executable_ref, &class_name, "clazz")?[0];
    let slot = HEAP.get_object_field_value(executable_ref, &class_name, "slot")?[0];
    klass(declaring_class)?.get_method_by_index(slot as i64)
}

/// `java.lang.reflect.Executable.getParameters0()[Ljava/lang/reflect/Parameter;`
///
/// Null when the method has no `MethodParameters` attribute (the Java side then synthesizes
/// `arg0`, `arg1`, …), as HotSpot's `JVM_GetMethodParameters` does.
pub(crate) fn get_parameters0(executable_ref: i32) -> Result<i32> {
    let method = method_of(executable_ref)?;
    let Some(parameters) = method.parameters() else {
        return Ok(0);
    };

    let mut roots = JavaThread::hold_temp_roots(vec![Slot::Ref(executable_ref)]);
    let mut parameter_refs = Vec::with_capacity(parameters.len());
    for (index, (name, flags)) in parameters.iter().enumerate() {
        let name_ref = match name.as_deref() {
            Some(name) => StringPoolHelper::get_string(name)?,
            None => 0,
        };
        roots.add_root(Slot::Ref(name_ref));
        let parameter = Executor::invoke_args_constructor(
            "java/lang/reflect/Parameter",
            "<init>:(Ljava/lang/String;ILjava/lang/reflect/Executable;I)V",
            &[
                StackValueKind::Ref(name_ref),
                (*flags as i32).into(),
                StackValueKind::Ref(executable_ref),
                (index as i32).into(),
            ],
            None,
        )?[0]
            .value();
        roots.add_root(Slot::Ref(parameter));
        parameter_refs.push(parameter);
    }

    HEAP.create_array_with_values(
        "[Ljava/lang/reflect/Parameter;",
        &parameter_refs,
        &mut RootedSlots::new(),
    )
}

/// `Executable.getTypeAnnotationBytes0()[B` / `Field.getTypeAnnotationBytes0()[B`.
///
/// The class parser does not retain raw type annotations, so none are reported (null).
pub(crate) fn get_type_annotation_bytes0(_member_ref: i32) -> Result<i32> {
    Ok(0)
}
