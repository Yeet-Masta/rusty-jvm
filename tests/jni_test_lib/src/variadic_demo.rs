use jni::sys::{jclass, jint, jobject, JNIEnv};

extern "C" {
    /// In `variadic_demo.c`: C-variadic calls can only be made from C.
    fn jni_variadic_demo(env: *mut JNIEnv, receiver: jobject, base: jclass) -> jint;
}

#[no_mangle]
unsafe extern "system" fn Java_samples_jni_variadic_JniVariadicDemo_verify(
    env: *mut JNIEnv,
    _class: jclass,
    receiver: jobject,
    base: jclass,
) -> jint {
    jni_variadic_demo(env, receiver, base)
}
