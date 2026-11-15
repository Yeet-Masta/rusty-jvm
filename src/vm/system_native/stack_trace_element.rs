use crate::vm::error::Result;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::method_area::java_method::JavaMethod;
use crate::vm::system_native::throwable::{backtrace_frames, NATIVE_METHOD};

const NATIVE_MARKER: i32 = -2;
const CLASS_NAME: &str = "java/lang/StackTraceElement";

/// `java.lang.StackTraceElement.initStackTraceElements([Ljava/lang/StackTraceElement;Ljava/lang/Object;I)V`
pub(crate) fn init_stack_trace_elements(
    element_array_ref: i32,
    backtrace_ref: i32,
    depth: i32,
) -> Result<()> {
    let frames = backtrace_frames(backtrace_ref)?;
    for (index, frame) in frames.iter().take(depth.max(0) as usize).enumerate() {
        // SAFETY: class unloading is disabled, so the method registry keeps every `JavaMethod`
        // recorded in a backtrace alive for the life of the VM.
        let method_name = unsafe { (*(frame.method_ref as *const JavaMethod)).name() };

        let line_number = if frame.tag == NATIVE_METHOD {
            NATIVE_MARKER
        } else {
            frame.line_number
        };

        let element_ref = HEAP.get_array_value(element_array_ref, index as i32)?[0];
        fill_element(element_ref, frame.class_ref, method_name, line_number)?;
    }

    Ok(())
}

/// Sets every field of a `StackTraceElement` the VM is responsible for, as HotSpot's
/// `java_lang_StackTraceElement::fill_in` does: declaring class (object and name), class loader
/// name, module name and version, method name, file name and line number. The Java side's
/// `computeFormat()` then decides which of them `toString()` shows.
pub(crate) fn fill_element(
    element_ref: i32,
    class_ref: i32,
    method_name: &str,
    line_number: i32,
) -> Result<()> {
    let klass = klass(class_ref)?;
    let class_name_ref = StringPoolHelper::get_string(klass.external_name())?;
    let method_name_ref = StringPoolHelper::get_string(method_name)?;
    let file_name_ref = match klass.source_file().as_deref() {
        Some(source_file) => StringPoolHelper::get_string(source_file)?,
        None => 0,
    };

    let set = |field: &str, value: i32| {
        HEAP.set_object_field_value(element_ref, CLASS_NAME, field, vec![value])
    };
    set("declaringClassObject", class_ref)?;
    set("declaringClass", class_name_ref)?;
    set("methodName", method_name_ref)?;
    set("fileName", file_name_ref)?;
    set("lineNumber", line_number)?;

    let (module_name, module_version) = module_name_and_version(class_ref)?;
    set("moduleName", module_name)?;
    set("moduleVersion", module_version)?;
    set("classLoaderName", class_loader_name(class_ref)?)?;
    Ok(())
}

/// The named module's name and raw version string (each 0 when absent).
fn module_name_and_version(class_ref: i32) -> Result<(i32, i32)> {
    let module_ref = HEAP.get_object_field_value(class_ref, "java/lang/Class", "module")?[0];
    if module_ref == 0 {
        return Ok((0, 0));
    }
    let name_ref = HEAP.get_object_field_value(module_ref, "java/lang/Module", "name")?[0];
    if name_ref == 0 {
        return Ok((0, 0));
    }
    let descriptor_ref =
        HEAP.get_object_field_value(module_ref, "java/lang/Module", "descriptor")?[0];
    let version_ref = if descriptor_ref == 0 {
        0
    } else {
        HEAP.get_object_field_value(
            descriptor_ref,
            "java/lang/module/ModuleDescriptor",
            "rawVersionString",
        )
        .map(|value| value[0])
        .unwrap_or(0)
    };
    let version_ref = if version_ref != 0 || descriptor_ref == 0 {
        version_ref
    } else {
        let version = HEAP
            .get_object_field_value(
                descriptor_ref,
                "java/lang/module/ModuleDescriptor",
                "version",
            )
            .map(|value| value[0])
            .unwrap_or(0);
        if version == 0 {
            0
        } else {
            HEAP.get_object_field_value(
                version,
                "java/lang/module/ModuleDescriptor$Version",
                "version",
            )
            .map(|value| value[0])
            .unwrap_or(0)
        }
    };
    Ok((name_ref, version_ref))
}

/// The defining loader's name (`ClassLoader.name`), or 0 for the boot loader or an unnamed one.
fn class_loader_name(class_ref: i32) -> Result<i32> {
    let loader_ref = HEAP.get_object_field_value(class_ref, "java/lang/Class", "classLoader")?[0];
    if loader_ref == 0 {
        return Ok(0);
    }
    Ok(HEAP
        .get_object_field_value(loader_ref, "java/lang/ClassLoader", "name")
        .map(|value| value[0])
        .unwrap_or(0))
}
