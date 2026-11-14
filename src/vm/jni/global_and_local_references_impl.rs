use crate::vm::jni::handles::{self, HandleResult, ReferenceKind};
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::jni::local_frames::MAX_LOCAL_CAPACITY;
use jni_sys::{jint, jobject, jweak, JNIEnv, JNI_ERR, JNI_OK};

/// A capacity `EnsureLocalCapacity` or `PushLocalFrame` accepts. Like HotSpot, these refuse a
/// negative capacity or one above `MaxJNILocalCapacity` with `JNI_ERR` and no exception.
fn capacity(value: jint) -> Option<usize> {
    usize::try_from(value)
        .ok()
        .filter(|&capacity| capacity <= MAX_LOCAL_CAPACITY)
}

fn status(result: HandleResult<()>) -> jint {
    match result {
        Ok(()) => JNI_OK,
        Err(error) => {
            error.set_pending();
            JNI_ERR
        }
    }
}

pub(super) extern "system" fn push_local_frame(_env: *mut JNIEnv, count: jint) -> jint {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let Some(count) = capacity(count) else {
        return JNI_ERR;
    };
    status(JavaThread::with_jni_frames(|frames| {
        frames.push(count, false).map(|_| ())
    }))
}

pub(super) extern "system" fn pop_local_frame(_env: *mut JNIEnv, result: jobject) -> jobject {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::finish(JavaThread::with_jni_frames(|frames| frames.pop(result)))
}

pub(super) extern "system" fn ensure_local_capacity(_env: *mut JNIEnv, count: jint) -> jint {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    let Some(count) = capacity(count) else {
        return JNI_ERR;
    };
    status(JavaThread::with_jni_frames(|frames| {
        frames.ensure_capacity(count)
    }))
}

pub(super) extern "system" fn new_local_ref(_env: *mut JNIEnv, object: jobject) -> jobject {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::new_reference(object, ReferenceKind::Local)
}

pub(super) extern "system" fn new_global_ref(_env: *mut JNIEnv, object: jobject) -> jobject {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::new_reference(object, ReferenceKind::Global)
}

pub(super) extern "system" fn new_weak_global_ref(_env: *mut JNIEnv, object: jobject) -> jweak {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::new_reference(object, ReferenceKind::WeakGlobal)
}

pub(super) extern "system" fn delete_local_ref(_env: *mut JNIEnv, object: jobject) {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::delete_reference(object, ReferenceKind::Local);
}

pub(super) extern "system" fn delete_global_ref(_env: *mut JNIEnv, object: jobject) {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::delete_reference(object, ReferenceKind::Global);
}

pub(super) extern "system" fn delete_weak_global_ref(_env: *mut JNIEnv, object: jweak) {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    handles::delete_reference(object, ReferenceKind::WeakGlobal);
}
