use crate::vm::heap::heap::HEAP;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::safepoint::{Blocking, Reason};
use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::LazyLock;
use std::thread::JoinHandle;
use tracing::{debug, error};

/// `Thread.State` encodings written to `Thread$FieldHolder.threadStatus`, using the JVMTI bit
/// layout that `jdk.internal.misc.VM.toThreadState` decodes (`getState()` reads it). `ALIVE` (1) is
/// set for every live state so `isAlive`-style checks see it; `NEW` is the field's default of 0.
pub(crate) mod thread_status {
    pub(crate) const NEW: i32 = 0x00; // no bits set (the field's default)
    pub(crate) const RUNNABLE: i32 = 0x05; // ALIVE | RUNNABLE(4)
    pub(crate) const BLOCKED: i32 = 0x401; // ALIVE | BLOCKED_ON_MONITOR_ENTER(1024)
                                           // The waiting states carry HotSpot's `JavaThreadStatus` detail bits, which say what the
                                           // thread waits in; `Thread.getState()` only looks at the indefinite/timeout bit.
    const WAITING: i32 = 0x80;
    const INDEFINITELY: i32 = 0x10;
    const WITH_TIMEOUT: i32 = 0x20;
    pub(crate) const IN_OBJECT_WAIT_BIT: i32 = 0x100;
    pub(crate) const PARKED_BIT: i32 = 0x200;
    pub(crate) const BLOCKED_BIT: i32 = 0x400;
    pub(crate) const SLEEPING: i32 = 0x01 | WAITING | WITH_TIMEOUT | 0x40;
    pub(crate) const IN_OBJECT_WAIT: i32 = 0x01 | WAITING | INDEFINITELY | IN_OBJECT_WAIT_BIT;
    pub(crate) const IN_OBJECT_WAIT_TIMED: i32 =
        0x01 | WAITING | WITH_TIMEOUT | IN_OBJECT_WAIT_BIT;
    pub(crate) const PARKED: i32 = 0x01 | WAITING | INDEFINITELY | PARKED_BIT;
    pub(crate) const PARKED_TIMED: i32 = 0x01 | WAITING | WITH_TIMEOUT | PARKED_BIT;
    pub(crate) const TERMINATED: i32 = 0x02; // TERMINATED(2)
}

/// Monotonic source of non-zero `Thread.eetop` tokens.
///
/// `Thread.isAlive()` is defined as `eetop != 0`, so every started thread is stamped with a unique
/// non-zero value on start and cleared back to `0` on termination. The concrete value is otherwise
/// opaque (HotSpot stores the native thread pointer here); we only need it to be non-zero and
/// distinct per thread.
static NEXT_EETOP: AtomicI64 = AtomicI64::new(1);

/// Join handles of live **non-daemon** platform threads.
///
/// Per the JVM spec the VM keeps running until the last non-daemon thread dies, so the main thread
/// awaits all of these (via [`join_all_non_daemon`]) before running shutdown hooks. Daemon threads
/// are intentionally *not* tracked: they are abandoned at process exit, matching Java semantics.
static NON_DAEMON_THREADS: LazyLock<Mutex<Vec<JoinHandle<()>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Maps each live thread's `java.lang.Thread` reference to its OS thread handle, so `Unsafe.unpark`
/// can wake a specific thread. Rust's `std::thread` park/unpark implement exactly the permit-based
/// semantics of `LockSupport` (unpark grants a permit that a later park consumes), so the two map
/// directly. Every thread registers itself once it is attached (identity is set) and unregisters on
/// termination.
static PARKERS: LazyLock<DashMap<i32, std::thread::Thread>> = LazyLock::new(DashMap::new);

/// Threads ever started (for `ThreadMXBean.getTotalStartedThreadCount`).
pub(crate) static STARTED_THREADS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
/// Most threads alive at once (for `ThreadMXBean.getPeakThreadCount`).
pub(crate) static PEAK_THREADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// What a thread has been blocked by, for `ThreadMXBean` (`ThreadInfo`, deadlock detection).
#[derive(Debug, Default)]
pub(crate) struct ThreadActivity {
    /// Times the thread blocked to enter a monitor.
    pub(crate) blocked_count: AtomicI64,
    /// Times the thread waited: `Object.wait`, `LockSupport.park` and `Thread.sleep`.
    pub(crate) waited_count: AtomicI64,
    /// The object whose monitor the thread is entering or waiting on, or 0. The object is
    /// rooted by the blocked thread itself for as long as this is set.
    pub(crate) contended_monitor: std::sync::atomic::AtomicI32,
}

static ACTIVITY: LazyLock<DashMap<i32, std::sync::Arc<ThreadActivity>>> =
    LazyLock::new(DashMap::new);

/// The `java.lang.Thread` of each registered OS thread, to name the owner of a monitor.
static OS_THREADS: LazyLock<DashMap<std::thread::ThreadId, i32>> = LazyLock::new(DashMap::new);

/// Forgets the activity records of `Thread` objects `live` rejects. Called by the collector, which
/// frees them; their references may be issued again.
pub(crate) fn prune_activity(mut live: impl FnMut(i32) -> bool) {
    ACTIVITY.retain(|thread_ref, _| live(*thread_ref));
}

/// The activity record of `thread_ref`, created on first use.
pub(crate) fn activity(thread_ref: i32) -> std::sync::Arc<ThreadActivity> {
    if let Some(found) = ACTIVITY.get(&thread_ref) {
        return std::sync::Arc::clone(found.value());
    }
    std::sync::Arc::clone(ACTIVITY.entry(thread_ref).or_default().value())
}

/// [`activity`] of the calling thread, when it is attached.
pub(crate) fn current_activity() -> Option<std::sync::Arc<ThreadActivity>> {
    JavaThread::current_thread().map(activity)
}

/// The `java.lang.Thread` running on the OS thread `id`, when it is a registered Java thread.
pub(crate) fn thread_of(id: std::thread::ThreadId) -> Option<i32> {
    OS_THREADS.get(&id).map(|entry| *entry.value())
}

/// The OS thread running the live `java.lang.Thread` `thread_ref`.
pub(crate) fn os_thread_of(thread_ref: i32) -> Option<std::thread::ThreadId> {
    PARKERS.get(&thread_ref).map(|handle| handle.id())
}

/// Keys are retained until exit; no parker guard spans a Safe transition.
pub(crate) fn parker_refs() -> Vec<i32> {
    PARKERS.iter().map(|entry| *entry.key()).collect()
}

/// Returns the next non-zero `eetop` token for a starting thread.
pub(crate) fn next_eetop() -> i64 {
    NEXT_EETOP.fetch_add(1, Ordering::Relaxed)
}

/// Writes `status` (a [`thread_status`] value) into `thread_ref`'s `Thread$FieldHolder.threadStatus`,
/// which is what `Thread.getState()` reports. Best-effort: any failure to reach the field is ignored
/// (thread state is observational and must never break the operation that updates it).
pub(crate) fn set_thread_status(thread_ref: i32, status: i32) {
    let Ok(holder) = HEAP.get_object_field_value(thread_ref, "java/lang/Thread", "holder") else {
        return;
    };
    match holder.first() {
        // A live thread always has its FieldHolder; only write when it's actually present so we
        // don't push a null ref through `set_object_field_value`'s error path on every update.
        Some(&holder_ref) if holder_ref != 0 => {
            let _ = HEAP.set_object_field_value(
                holder_ref,
                "java/lang/Thread$FieldHolder",
                "threadStatus",
                vec![status],
            );
        }
        _ => {
            debug!("set_thread_status: thread {thread_ref} has no holder; skipping status update")
        }
    }
}

/// [`set_thread_status`] for the calling thread (e.g. entering `wait`/`sleep`/`blocked`).
pub(crate) fn set_current_thread_status(status: i32) {
    if let Some(thread_ref) = JavaThread::current_thread() {
        set_thread_status(thread_ref, status);
    }
}

/// Associates a `java.lang.Thread` reference with its OS thread handle so it can be unparked by
/// [`unpark`]. Registered *eagerly by the spawning thread* (from `JoinHandle::thread()`) before the
/// child starts running, so an `unpark` that races ahead of the child's first `park` is never lost.
pub(crate) fn register_parker(thread_ref: i32, handle: std::thread::Thread) {
    OS_THREADS.insert(handle.id(), thread_ref);
    PARKERS.insert(thread_ref, handle);
    STARTED_THREADS.fetch_add(1, Ordering::Relaxed);
    PEAK_THREADS.fetch_max(PARKERS.len(), Ordering::Relaxed);
}

/// Drops the parker mapping for a terminated thread.
pub(crate) fn unregister_parker(thread_ref: i32) {
    if let Some((_, handle)) = PARKERS.remove(&thread_ref) {
        OS_THREADS.remove(&handle.id());
    }
    ACTIVITY.remove(&thread_ref);
}

/// Grants a parking permit to the thread identified by `thread_ref` (waking it if parked), or does
/// nothing if that thread is not currently registered. Backs `Unsafe.unpark` / `LockSupport.unpark`.
pub(crate) fn unpark(thread_ref: i32) {
    if let Some(handle) = PARKERS.get(&thread_ref) {
        handle.unpark();
    }
}

/// Records a non-daemon thread's join handle so the VM waits for it before exiting.
pub(crate) fn register_non_daemon(handle: JoinHandle<()>) {
    NON_DAEMON_THREADS.lock().push(handle);
}

/// Blocks until every non-daemon thread has terminated.
///
/// Called once from the main thread after `main` returns. Loops until the registry drains, so that
/// threads spawned *by* other non-daemon threads while we are joining are awaited too. A thread that
/// terminated by panicking is logged and otherwise ignored (a panic is already contained by the OS
/// thread boundary and cannot poison shared state — locks here are `parking_lot`).
pub(crate) fn join_all_non_daemon() {
    loop {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *NON_DAEMON_THREADS.lock());
        if handles.is_empty() {
            break;
        }
        for handle in handles {
            let _blocked = Blocking::enter(Reason::Blocking);
            if let Err(panic) = handle.join() {
                error!("a non-daemon thread terminated by panicking: {panic:?}");
            }
        }
    }
}
