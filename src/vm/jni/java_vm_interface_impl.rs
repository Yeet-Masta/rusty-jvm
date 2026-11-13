//! The JNI invocation interface (`JavaVM`), through which native code reaches the VM from any
//! thread: getting a thread's `JNIEnv`, and attaching native threads to (and detaching them from)
//! the VM. Follows HotSpot's `jni.cpp`.
use crate::vm::error::Result;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::jni::handles;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::jni::jni_env::get_jni_vm;
use crate::vm::jni::local_frames::NativeFrame;
use crate::vm::method_area::method_area::{try_with_method_area, with_method_area};
use crate::vm::safepoint;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::thread as java_lang_thread;
use crate::vm::threads;
use jni_sys::{
    jint, JNIEnv, JavaVM, JavaVMAttachArgs, JNI_EDETACHED, JNI_ERR, JNI_EVERSION, JNI_OK,
    JNI_VERSION_10, JNI_VERSION_19, JNI_VERSION_1_1, JNI_VERSION_1_2, JNI_VERSION_1_4,
    JNI_VERSION_1_6, JNI_VERSION_1_8, JNI_VERSION_20, JNI_VERSION_21, JNI_VERSION_24,
    JNI_VERSION_9,
};
use std::cell::RefCell;
use std::ffi::{c_void, CStr};
use std::ptr::null_mut;
use tracing::warn;

thread_local! {
    /// The base JNI local frame of a native thread attached by `AttachCurrentThread`: it holds the
    /// local references the thread creates outside any native method, until it detaches.
    static ATTACHED: RefCell<Option<NativeFrame>> = const { RefCell::new(None) };
}

/// The JNI versions a native thread can attach with, like HotSpot's
/// `Threads::is_supported_jni_version`.
pub(crate) fn is_supported_jni_version(version: jint) -> bool {
    matches!(
        version,
        JNI_VERSION_1_2
            | JNI_VERSION_1_4
            | JNI_VERSION_1_6
            | JNI_VERSION_1_8
            | JNI_VERSION_9
            | JNI_VERSION_10
            | JNI_VERSION_19
            | JNI_VERSION_20
            | JNI_VERSION_21
            | JNI_VERSION_24
    )
}

/// [`is_supported_jni_version`] plus version 1.1, which `GetEnv` and `JNI_OnLoad` also accept.
pub(crate) fn is_supported_jni_version_including_1_1(version: jint) -> bool {
    version == JNI_VERSION_1_1 || is_supported_jni_version(version)
}

/// Whether the calling OS thread is attached to the VM (a Java thread, or a native thread that
/// attached), and so has a `JNIEnv`.
fn attached() -> bool {
    safepoint::current().is_some()
}

pub(super) extern "system" fn get_java_vm(_env: *mut JNIEnv, vm: *mut *mut JavaVM) -> jint {
    let _managed = crate::vm::safepoint::ManagedAccess::enter();
    if vm.is_null() {
        return JNI_ERR;
    }

    let jni_vm = get_jni_vm();
    unsafe {
        *vm = jni_vm;
    }

    JNI_OK
}

/// `JavaVM::GetEnv`. Touches no managed state, so it runs on the caller's thread as it is.
pub(super) unsafe extern "system" fn get_env(
    _vm: *mut JavaVM,
    penv: *mut *mut c_void,
    version: jint,
) -> jint {
    if penv.is_null() {
        return JNI_ERR;
    }
    if !attached() {
        *penv = null_mut();
        return JNI_EDETACHED;
    }
    if !is_supported_jni_version_including_1_1(version) {
        // Includes the JVMTI versions: this VM has no tool interface
        *penv = null_mut();
        return JNI_EVERSION;
    }
    *penv = JavaThread::get_env_ptr().cast();
    JNI_OK
}

/// `JavaVM::AttachCurrentThread`
pub(super) unsafe extern "system" fn attach_current_thread(
    _vm: *mut JavaVM,
    penv: *mut *mut c_void,
    args: *mut c_void,
) -> jint {
    attach(penv, args.cast(), false)
}

/// `JavaVM::AttachCurrentThreadAsDaemon`
pub(super) unsafe extern "system" fn attach_current_thread_as_daemon(
    _vm: *mut JavaVM,
    penv: *mut *mut c_void,
    args: *mut c_void,
) -> jint {
    attach(penv, args.cast(), true)
}

unsafe fn attach(penv: *mut *mut c_void, args: *const JavaVMAttachArgs, daemon: bool) -> jint {
    if penv.is_null() {
        return JNI_ERR;
    }
    // Like HotSpot, which has long had its check of JavaVMAttachArgs.version commented out, any
    // version is accepted here. Attaching an attached thread just hands out its environment again
    if attached() {
        *penv = JavaThread::get_env_ptr().cast();
        return JNI_OK;
    }
    // A native thread can only join a VM that has finished creating its main thread group
    let Some(main_group) = main_thread_group() else {
        return JNI_ERR;
    };
    let (name, group) = if args.is_null() {
        (None, null_mut())
    } else {
        let name = (*args).name;
        let name = if name.is_null() {
            None
        } else {
            match cesu8::from_java_cesu8(CStr::from_ptr(name).to_bytes()) {
                Ok(name) => Some(name.into_owned()),
                Err(_) => return JNI_ERR,
            }
        };
        (name, (*args).group)
    };

    safepoint::attach();
    // The base frame for the local references this thread creates outside native methods
    let Some(frame) = NativeFrame::enter() else {
        abandon_attach();
        return JNI_ERR;
    };
    let group = match handles::resolve(group) {
        Some(0) => main_group,
        Some(group) => group,
        None => {
            drop(frame);
            abandon_attach();
            return JNI_ERR;
        }
    };
    match create_thread(group, name.as_deref(), daemon) {
        Ok(()) => {
            ATTACHED.with(|attached| *attached.borrow_mut() = Some(frame));
            *penv = JavaThread::get_env_ptr().cast();
            // Back to native code, which runs in the Safe state
            safepoint::enter_native();
            JNI_OK
        }
        Err(error) => {
            warn!("AttachCurrentThread failed to create the thread object: {error}");
            drop(frame);
            abandon_attach();
            JNI_ERR
        }
    }
}

fn main_thread_group() -> Option<i32> {
    try_with_method_area(|area| area.main_thread_group_id()).flatten()
}

/// Creates the `java.lang.Thread` of the calling native thread, like HotSpot's
/// `JavaThread::allocate_threadObj`: the thread is bound before its constructor runs, which
/// recognizes an attaching thread by `currentThread()` being the thread under construction.
fn create_thread(group: i32, name: Option<&str>, daemon: bool) -> Result<()> {
    let _group = JavaThread::hold_temp_roots(vec![Slot::Ref(group)]);
    let instance =
        with_method_area(|area| area.create_instance_with_default_fields("java/lang/Thread"))?;
    let mut destination = RootedSlots::new();
    let thread = HEAP.create_instance(instance, &mut destination)?;
    JavaThread::set_current_thread(thread);
    threads::register_parker(thread, std::thread::current());
    match name {
        Some(name) => {
            let name = StringPoolHelper::create_string(name)?;
            Executor::invoke_non_static_method(
                "java/lang/Thread",
                "<init>:(Ljava/lang/ThreadGroup;Ljava/lang/String;)V",
                thread,
                &[
                    StackValueKind::Ref(group),
                    StackValueKind::Ref(name[0].value()),
                ],
            )?;
        }
        // Thread has no constructor taking only a group; a null task names it "Thread-<n>"
        None => {
            Executor::invoke_non_static_method(
                "java/lang/Thread",
                "<init>:(Ljava/lang/ThreadGroup;Ljava/lang/Runnable;)V",
                thread,
                &[StackValueKind::Ref(group), StackValueKind::Ref(0)],
            )?;
        }
    }
    if daemon {
        let holder = HEAP.get_object_field_value(thread, "java/lang/Thread", "holder")?[0];
        HEAP.set_object_field_value(holder, "java/lang/Thread$FieldHolder", "daemon", vec![1])?;
    }
    java_lang_thread::mark_alive(thread)
}

/// Undoes a failed attach, leaving the thread as detached as it was before.
fn abandon_attach() {
    JavaThread::take_pending_exception();
    if let Some(thread) = JavaThread::current_thread() {
        threads::unregister_parker(thread);
    }
    JavaThread::clear_current_thread();
    safepoint::retire();
}

/// `JavaVM::DetachCurrentThread`
pub(super) unsafe extern "system" fn detach_current_thread(_vm: *mut JavaVM) -> jint {
    // Detaching a detached thread does nothing
    if !attached() {
        return JNI_OK;
    }
    // Like HotSpot, a thread running Java code (inside a native method) cannot detach, and
    // neither can the threads the VM started itself
    if JavaThread::has_java_frames() || ATTACHED.with(|attached| attached.borrow().is_none()) {
        return JNI_ERR;
    }
    {
        // Native code runs Safe; Thread.exit() is Java
        let _managed = safepoint::ManagedAccess::enter();
        if let Some(thread) = JavaThread::current_thread() {
            java_lang_thread::terminate(thread);
        }
        JavaThread::take_pending_exception();
        // The base frame's local references die with the attachment
        ATTACHED.with(|attached| attached.borrow_mut().take());
        JavaThread::clear_current_thread();
    }
    safepoint::retire_from_native();
    JNI_OK
}

/// `JavaVM::DestroyJavaVM`.
///
/// The VM is created, run and shut down by its own launcher, and there is no invocation API that
/// creates one, so native code cannot destroy it; HotSpot reports the same for a VM that cannot be
/// destroyed from the calling thread.
pub(super) unsafe extern "system" fn destroy_java_vm(_vm: *mut JavaVM) -> jint {
    JNI_ERR
}
