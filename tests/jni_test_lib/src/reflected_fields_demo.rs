use jni::sys::{jboolean, jclass, jint, jobject, jstring, JNIEnv};
use std::ptr::null_mut;

#[no_mangle]
unsafe extern "system" fn Java_samples_jni_reflectedfields_JniReflectedFieldsDemo_roundTrip(
    env: *mut JNIEnv,
    _class: jclass,
    field: jobject,
    target: jclass,
    is_static: jboolean,
) -> jobject {
    let id = ((*(*env)).v24.FromReflectedField)(env, field);
    if ((*(*env)).v24.ExceptionCheck)(env) {
        return null_mut();
    }
    ((*(*env)).v24.ToReflectedField)(env, target, id, is_static)
}

#[no_mangle]
unsafe extern "system" fn Java_samples_jni_reflectedfields_JniReflectedFieldsDemo_sameId(
    env: *mut JNIEnv,
    _class: jclass,
    field: jobject,
    target: jclass,
    name: jstring,
    signature: jstring,
    is_static: jboolean,
) -> jboolean {
    let name_chars = ((*(*env)).v24.GetStringUTFChars)(env, name, null_mut());
    let signature_chars = ((*(*env)).v24.GetStringUTFChars)(env, signature, null_mut());
    let lookup = if is_static {
        ((*(*env)).v24.GetStaticFieldID)(env, target, name_chars, signature_chars)
    } else {
        ((*(*env)).v24.GetFieldID)(env, target, name_chars, signature_chars)
    };
    ((*(*env)).v24.ReleaseStringUTFChars)(env, name, name_chars);
    ((*(*env)).v24.ReleaseStringUTFChars)(env, signature, signature_chars);
    let reflected = ((*(*env)).v24.FromReflectedField)(env, field);
    !reflected.is_null() && reflected == lookup
}

#[no_mangle]
unsafe extern "system" fn Java_samples_jni_reflectedfields_JniReflectedFieldsDemo_update(
    env: *mut JNIEnv,
    _class: jclass,
    field: jobject,
    receiver: jobject,
    target: jclass,
    value: jint,
    is_static: jboolean,
) -> jint {
    let id = ((*(*env)).v24.FromReflectedField)(env, field);
    if is_static {
        ((*(*env)).v24.SetStaticIntField)(env, target, id, value);
        ((*(*env)).v24.GetStaticIntField)(env, target, id)
    } else {
        ((*(*env)).v24.SetIntField)(env, receiver, id, value);
        ((*(*env)).v24.GetIntField)(env, receiver, id)
    }
}
