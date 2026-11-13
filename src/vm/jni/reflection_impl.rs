use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::static_init::StaticInit;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::jni::handles;
use crate::vm::jni::utils::{
    decode_method_id, encode_method_id, set_pending_internal_error, set_pending_vm_error,
};
use crate::vm::jni::{object_fields_impl, static_fields_impl};
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use jni_sys::{jboolean, jclass, jfieldID, jmethodID, jobject, JNIEnv};
use std::ptr::null_mut;

pub(super) extern "system" fn from_reflected_field(
    _env: *mut JNIEnv,
    reflected: jobject,
) -> jfieldID {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let reflected = handles::resolve_or_return!(reflected);
    match from_reflected_field_impl(reflected) {
        Ok(field) => field,
        Err(error) => {
            if error.throwable_ref().is_some() {
                set_pending_vm_error(&error);
                return null_mut();
            }
            set_pending_internal_error(&format!("Failed to convert reflected field: {error}"));
            null_mut()
        }
    }
}

fn from_reflected_field_impl(reflected: i32) -> Result<jfieldID> {
    let class_ref = HEAP.get_object_field_value(reflected, "java/lang/reflect/Field", "clazz")?[0];
    let name_ref = HEAP.get_object_field_value(reflected, "java/lang/reflect/Field", "name")?[0];
    let name = get_utf8_string_by_ref(name_ref)?;
    let class = klass(class_ref)?;
    // Like HotSpot's jni_FromReflectedField (and our GetFieldID/GetStaticFieldID), initialize the
    // declaring class before handing out an ID: `getDeclaredField` does not initialize, and a value
    // set through the ID would otherwise be overwritten by a later `<clinit>`.
    StaticInit::initialize_java_class(&class)?;
    let field = class.field_info(&name).ok_or_else(|| {
        Error::new_execution("Reflected field is missing from its declaring class")
    })?;
    if field.is_static() {
        Ok(static_fields_impl::encode_field_id(
            class_ref,
            class.get_static_field_offset(&name)?,
        ))
    } else {
        Ok(object_fields_impl::encode_field_id(
            class.get_field_offset(&format!("{}.{name}", class.this_class_name()))?,
        ))
    }
}

pub(super) extern "system" fn to_reflected_field(
    _env: *mut JNIEnv,
    target: jclass,
    field_id: jfieldID,
    is_static: jboolean,
) -> jobject {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let result = if is_static {
        let (class_ref, offset) = static_fields_impl::decode_field_id(field_id);
        klass(class_ref).and_then(|class| {
            class
                .get_static_field_info_by_offset(offset)?
                .reflection_ref()
        })
    } else {
        let target = handles::resolve_or_return!(target);
        klass(target).and_then(|class| {
            let (owner, name) =
                class.get_field_name_by_offset(object_fields_impl::decode_field_id(field_id))?;
            let owner = CLASSES.get(&owner)?;
            owner
                .field_info(&name)
                .ok_or_else(|| {
                    Error::new_execution("JNI field is missing from its declaring class")
                })?
                .reflection_ref()
        })
    };
    match result {
        Ok(reference) => handles::new_local(reference),
        Err(error) => {
            if error.throwable_ref().is_some() {
                set_pending_vm_error(&error);
                return null_mut();
            }
            set_pending_internal_error(&format!("Failed to create reflected field: {error}"));
            null_mut()
        }
    }
}

pub(super) extern "system" fn from_reflected_method(
    _env: *mut JNIEnv,
    reflected: jobject,
) -> jmethodID {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let reflected = handles::resolve_or_return!(reflected);
    let result = HEAP.get_instance_name(reflected).and_then(|class_name| {
        let declaring_class = HEAP.get_object_field_value(reflected, &class_name, "clazz")?;
        let slot = HEAP.get_object_field_value(reflected, &class_name, "slot")?;
        Ok(encode_method_id(declaring_class[0], slot[0] as usize) as jmethodID)
    });
    match result {
        Ok(method_id) => method_id,
        Err(error) => {
            set_pending_internal_error(&format!("Failed to convert reflected method: {error}"));
            null_mut()
        }
    }
}

pub(super) extern "system" fn to_reflected_method(
    _env: *mut JNIEnv,
    _clazz: jclass,
    method_id: jmethodID,
    _is_static: jboolean,
) -> jobject {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let (class_ref, method_index) = decode_method_id(method_id as usize);
    let result = klass(class_ref)
        .and_then(|class| class.get_method_by_index(method_index))
        .and_then(|method| method.reflection_ref());
    match result {
        Ok(reference) => handles::new_local(reference),
        Err(error) => {
            if error.throwable_ref().is_some() {
                set_pending_vm_error(&error);
                return null_mut();
            }
            set_pending_internal_error(&format!("Failed to create reflected method: {error}"));
            null_mut()
        }
    }
}
