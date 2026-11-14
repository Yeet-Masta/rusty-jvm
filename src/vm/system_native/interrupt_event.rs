//! Windows interrupt events: a manual-reset event per Java thread, signalled by
//! `Thread.interrupt` and reset by `Thread.clearInterruptEvent`, as in HotSpot. JDK native code
//! waits on it next to its own handles (`ProcessImpl.waitForInterruptibly` waits for the child
//! process *or* an interrupt), and gets it through `JVM_GetThreadInterruptEvent`.

use dashmap::DashMap;
use std::ptr::null_mut;
use std::sync::LazyLock;
use winapi::um::handleapi::CloseHandle;
use winapi::um::synchapi::{CreateEventW, ResetEvent, SetEvent};
use winapi::um::winnt::HANDLE;

/// Java `Thread` object -> its event. Thread objects keep their identity for their lifetime.
static EVENTS: LazyLock<DashMap<i32, usize>> = LazyLock::new(DashMap::new);

fn event_for(thread: i32) -> HANDLE {
    *EVENTS.entry(thread).or_insert_with(|| {
        // Manual reset, initially non-signalled.
        unsafe { CreateEventW(null_mut(), 1, 0, null_mut()) as usize }
    }) as HANDLE
}

/// Signals `thread`'s event (from `Thread.interrupt0`).
pub(crate) fn signal(thread: i32) {
    let event = event_for(thread);
    if !event.is_null() {
        unsafe { SetEvent(event) };
    }
}

/// Resets the calling thread's event (`Thread.clearInterruptEvent`).
pub(crate) fn clear(thread: i32) {
    if let Some(event) = EVENTS.get(&thread) {
        unsafe { ResetEvent(*event as HANDLE) };
    }
}

/// Releases the event of a thread that has terminated.
pub(crate) fn release(thread: i32) {
    if let Some((_, event)) = EVENTS.remove(&thread) {
        unsafe { CloseHandle(event as HANDLE) };
    }
}

/// `JVM_GetThreadInterruptEvent()`: the calling thread's event.
pub(crate) extern "C" fn jvm_get_thread_interrupt_event() -> *mut std::ffi::c_void {
    match crate::vm::jni::java_thread::JavaThread::current_thread() {
        Some(thread) => event_for(thread) as *mut std::ffi::c_void,
        None => null_mut(),
    }
}
