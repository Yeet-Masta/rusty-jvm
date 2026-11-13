//! The JNI invocation interface: `JNI_OnLoad`, and native threads that attach to the VM.
//!
//! `JNI_OnLoad` runs whenever a fixture loads this library. It registers a native method of
//! `samples.jni.invocation.JniInvocationDemo` only when that class can be found, and leaves no
//! exception behind when it cannot.
use jni::sys::{
    jboolean, jclass, jint, jstring, jvalue, JNIEnv, JNINativeMethod, JavaVM, JavaVMAttachArgs,
    JNI_OK, JNI_VERSION_1_8,
};
use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::null_mut;
use std::sync::Mutex;

/// What `JNI_OnLoad` observed, for the fixture to print.
static ON_LOAD_REPORT: Mutex<Option<String>> = Mutex::new(None);

/// A JNI version no VM supports.
const UNSUPPORTED_VERSION: jint = 0x7fff_0000;

unsafe fn new_string(env: *mut JNIEnv, text: &str) -> jstring {
    let text = CString::new(text).expect("no NUL in the report");
    ((*(*env)).v24.NewStringUTF)(env, text.as_ptr())
}

unsafe fn get_env(vm: *mut JavaVM, env: &mut *mut JNIEnv, version: jint) -> jint {
    ((*(*vm)).v1_4.GetEnv)(vm, env as *mut *mut JNIEnv as *mut *mut c_void, version)
}

unsafe extern "system" fn registered_by_on_load(
    _env: *mut JNIEnv,
    _class: jclass,
    value: jint,
) -> jint {
    value * 2 + 2
}

#[no_mangle]
pub unsafe extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _reserved: *mut c_void) -> jint {
    let mut env = null_mut();
    let result = get_env(vm, &mut env, JNI_VERSION_1_8);
    let mut report = format!("JNI_OnLoad: GetEnv={result} env={}", !env.is_null());
    if result == JNI_OK {
        let class =
            ((*(*env)).v24.FindClass)(env, c"samples/jni/invocation/JniInvocationDemo".as_ptr());
        if class.is_null() {
            ((*(*env)).v24.ExceptionClear)(env);
        } else {
            let methods = [JNINativeMethod {
                name: c"registeredByOnLoad".as_ptr() as *mut c_char,
                signature: c"(I)I".as_ptr() as *mut c_char,
                fnPtr: registered_by_on_load as *const () as *mut c_void,
            }];
            let registered = ((*(*env)).v24.RegisterNatives)(env, class, methods.as_ptr(), 1);
            report.push_str(&format!(" RegisterNatives={registered}"));
            ((*(*env)).v24.DeleteLocalRef)(env, class);
        }
    }
    *ON_LOAD_REPORT.lock().unwrap() = Some(report);
    JNI_VERSION_1_8
}

#[no_mangle]
unsafe extern "system" fn Java_samples_jni_invocation_JniInvocationDemo_onLoadReport(
    env: *mut JNIEnv,
    _class: jclass,
) -> jstring {
    let report = ON_LOAD_REPORT
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "JNI_OnLoad was not called".to_string());
    new_string(env, &report)
}

/// GetEnv, AttachCurrentThread and DetachCurrentThread on a Java thread, inside a native method.
#[no_mangle]
unsafe extern "system" fn Java_samples_jni_invocation_JniInvocationDemo_javaThreadReport(
    env: *mut JNIEnv,
    _class: jclass,
) -> jstring {
    let mut vm = null_mut();
    ((*(*env)).v24.GetJavaVM)(env, &mut vm);
    let mut current = null_mut();
    let got = get_env(vm, &mut current, JNI_VERSION_1_8);
    let mut unsupported = env;
    let unsupported_result = get_env(vm, &mut unsupported, UNSUPPORTED_VERSION);
    let mut again = null_mut();
    let attached = ((*(*vm)).v1_4.AttachCurrentThread)(
        vm,
        &mut again as *mut *mut JNIEnv as *mut *mut c_void,
        null_mut(),
    );
    // A thread running Java code cannot detach
    let detached = ((*(*vm)).v1_4.DetachCurrentThread)(vm);
    let report = format!(
        "Java thread: GetEnv={got} same={} unsupported={unsupported_result} cleared={} \
         attach={attached} same={} detach={detached}",
        current == env,
        unsupported.is_null(),
        again == env,
    );
    new_string(env, &report)
}

/// Runs a native thread that attaches to the VM, calls `JniInvocationDemo.callback`, and
/// detaches, and reports what the invocation interface returned to it.
#[no_mangle]
unsafe extern "system" fn Java_samples_jni_invocation_JniInvocationDemo_attachNativeThread(
    env: *mut JNIEnv,
    class: jclass,
    daemon: jboolean,
    name: jstring,
) -> jstring {
    let mut vm = null_mut();
    ((*(*env)).v24.GetJavaVM)(env, &mut vm);
    // A local reference belongs to this thread; the native thread needs a global one
    let class = ((*(*env)).v24.NewGlobalRef)(env, class);
    let name = if name.is_null() {
        None
    } else {
        let chars = ((*(*env)).v24.GetStringUTFChars)(env, name, null_mut());
        let owned = CStr::from_ptr(chars).to_owned();
        ((*(*env)).v24.ReleaseStringUTFChars)(env, name, chars);
        Some(owned)
    };
    let (vm_address, class_address) = (vm as usize, class as usize);
    let report = std::thread::spawn(move || {
        native_thread(
            vm_address as *mut JavaVM,
            class_address as jclass,
            daemon,
            name,
        )
    })
    .join()
    .unwrap_or_else(|_| "native thread panicked".to_string());
    ((*(*env)).v24.DeleteGlobalRef)(env, class);
    new_string(env, &report)
}

unsafe fn native_thread(
    vm: *mut JavaVM,
    class: jclass,
    daemon: jboolean,
    name: Option<CString>,
) -> String {
    let mut env = null_mut();
    let before = get_env(vm, &mut env, JNI_VERSION_1_8);
    let before_cleared = env.is_null();
    let attach = if daemon {
        (*(*vm)).v1_4.AttachCurrentThreadAsDaemon
    } else {
        (*(*vm)).v1_4.AttachCurrentThread
    };
    let mut args = JavaVMAttachArgs {
        version: JNI_VERSION_1_8,
        name: name
            .as_ref()
            .map_or(null_mut(), |name| name.as_ptr() as *mut c_char),
        group: null_mut(),
    };
    let attached = attach(
        vm,
        &mut env as *mut *mut JNIEnv as *mut *mut c_void,
        &mut args as *mut JavaVMAttachArgs as *mut c_void,
    );
    if attached != JNI_OK {
        return format!("native thread: attach={attached}");
    }
    let mut again = null_mut();
    let attached_again = ((*(*vm)).v1_4.AttachCurrentThread)(
        vm,
        &mut again as *mut *mut JNIEnv as *mut *mut c_void,
        null_mut(),
    );
    let mut current = null_mut();
    let got = get_env(vm, &mut current, JNI_VERSION_1_8);

    let method = ((*(*env)).v24.GetStaticMethodID)(
        env,
        class,
        c"callback".as_ptr(),
        c"(Ljava/lang/String;)V".as_ptr(),
    );
    let message = ((*(*env)).v24.NewStringUTF)(env, c"hello from a native thread".as_ptr());
    let arguments = [jvalue { l: message }];
    ((*(*env)).v24.CallStaticVoidMethodA)(env, class, method, arguments.as_ptr());
    let threw = ((*(*env)).v24.ExceptionCheck)(env);
    if threw {
        ((*(*env)).v24.ExceptionDescribe)(env);
        ((*(*env)).v24.ExceptionClear)(env);
    }

    let detached = ((*(*vm)).v1_4.DetachCurrentThread)(vm);
    let mut after = env;
    let after_detach = get_env(vm, &mut after, JNI_VERSION_1_8);
    let detached_again = ((*(*vm)).v1_4.DetachCurrentThread)(vm);
    format!(
        "native thread: GetEnv={before} cleared={before_cleared} attach={attached} \
         again={attached_again} same={} GetEnv={got} same={} threw={threw} detach={detached} \
         GetEnv={after_detach} cleared={} detach={detached_again}",
        again == env,
        current == env,
        after.is_null(),
    )
}
