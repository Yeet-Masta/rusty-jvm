//! Per-object monitors: the runtime behind `synchronized`, `Object.wait/notify/notifyAll`, and
//! `Thread.holdsLock`.
//!
//! Every Java object can act as a monitor. We inflate one lazily on first use and key it by the
//! object's heap reference in [`MONITORS`] (objects never move, so the `i32` key is
//! stable). A monitor is a **reentrant** lock (owner OS-thread + recursion count) plus a condition
//! queue for `wait`/`notify`. One OS thread == one Java platform thread, so the OS `ThreadId`
//! identifies the owning Java thread.
//!
//! `wait`/`notify` use a single `Condvar` shared by the entry set (threads blocked acquiring) and
//! the wait set (threads in `Object.wait`). Each waiter holds a ticket in an ordered wait set, and
//! `notify` marks the oldest ticket, so a notification reaches only a thread that was already
//! waiting when it was issued (a thread that starts waiting later cannot consume it). Every waiter
//! re-checks its own ticket on wake, so a broadcast that also disturbs the entry set is merely
//! inefficient, never incorrect. Spurious wakeups are permitted by the `Object.wait` contract.

use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::{
    set_pending_illegal_monitor_state_exception, set_pending_interrupted_exception,
};
use crate::vm::heap::heap::HEAP;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::safepoint;
use crate::vm::threads::{self, thread_status};
use dashmap::DashMap;
use parking_lot::{Condvar, Mutex};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "monitor_coordinator_tests.rs"]
mod coordinator_tests;

struct MonitorState {
    /// Owning OS thread, or `None` when the monitor is free.
    owner: Option<ThreadId>,
    /// Reentrant acquisition count for `owner`.
    count: u32,
    /// How many Java frames `owner` had when it took the monitor (see
    /// [`JavaThread::root_monitor`]), which locates the locking frame in its stack trace.
    lock_depth: usize,
    /// Threads acquiring logical ownership (including wait-set reacquisition).
    entrants: u32,
    /// Number of threads currently in `Object.wait` on this monitor.
    waiters: u32,
    /// Tickets of threads in `Object.wait` that have not been notified yet, oldest first.
    wait_set: VecDeque<u64>,
    /// Tickets selected by `notify`/`notifyAll` whose threads have not returned yet.
    notified: HashSet<u64>,
    /// Next wait ticket.
    next_ticket: u64,
}

struct ObjectMonitor {
    state: Mutex<MonitorState>,
    cv: Condvar,
}

static MONITORS: LazyLock<DashMap<i32, Arc<ObjectMonitor>>> = LazyLock::new(DashMap::new);

/// The monitor each currently-waiting thread (by `java.lang.Thread` ref) is blocked on, so
/// [`interrupt_waiter`] can wake exactly that thread's `Object.wait`.
static WAIT_TARGETS: LazyLock<DashMap<i32, Arc<ObjectMonitor>>> = LazyLock::new(DashMap::new);

/// Active targets are already snapshot roots. Never wait for monitor state held
/// by a Safe thread reacquiring a lock; leave such a record for a later cycle.
pub(crate) fn prune_idle(mut live: impl FnMut(i32) -> bool) {
    MONITORS.retain(|reference, monitor| {
        if live(*reference) {
            return true;
        }
        match monitor.state.try_lock() {
            Some(state) => state.owner.is_some() || state.entrants != 0 || state.waiters != 0,
            None => true,
        }
    });
}

#[cfg(test)]
pub(crate) fn has_record(reference: i32) -> bool {
    MONITORS.contains_key(&reference)
}

#[cfg(test)]
pub(crate) fn record_count() -> usize {
    MONITORS.len()
}

#[cfg(test)]
pub(crate) fn state_for_test(obj_ref: i32) -> (bool, u32, u32) {
    let monitor = MONITORS
        .get(&obj_ref)
        .map(|entry| Arc::clone(entry.value()));
    let Some(monitor) = monitor else {
        return (false, 0, 0);
    };
    let state = safepoint::lock(&monitor.state);
    (state.owner.is_some(), state.count, state.entrants)
}

/// Atomically reads and clears the calling thread's `Thread.interrupted` flag, returning its prior
/// value (a real `getAndClearInterrupt`). `Thread.interrupt()` sets this Java field before asking the
/// VM to wake the thread, so a blocked `wait`/`sleep` observes it here and throws
/// `InterruptedException`.
///
/// The test-and-clear is a single compare-and-exchange under the field's entry lock — the same lock
/// `interrupt()`'s `PUTFIELD` takes — so an interrupt racing the clear cannot be lost (a separate
/// get-then-set could clear a flag that a concurrent `interrupt()` had just re-raised).
pub(crate) fn take_current_interrupt() -> bool {
    let Some(thread_ref) = JavaThread::current_thread() else {
        return false;
    };
    HEAP.compare_and_exchange_object_field(
        thread_ref,
        "java/lang/Thread",
        "interrupted",
        &[1],
        &[0],
    )
    .map(|(_old, swapped)| swapped)
    .unwrap_or(false)
}

fn monitor_for(obj_ref: i32) -> Arc<ObjectMonitor> {
    MONITORS
        .entry(obj_ref)
        .or_insert_with(|| {
            Arc::new(ObjectMonitor {
                state: Mutex::new(MonitorState {
                    owner: None,
                    count: 0,
                    lock_depth: 0,
                    entrants: 0,
                    waiters: 0,
                    wait_set: VecDeque::new(),
                    notified: HashSet::new(),
                    next_ticket: 0,
                }),
                cv: Condvar::new(),
            })
        })
        .clone()
}

/// Spin/yield attempts a contended `monitorenter` makes before parking.
const SPIN_ROUNDS: u32 = 10;

/// Wakes threads after the monitor became free. With no `Object.wait` waiters, everything asleep on
/// the condvar wants ownership and any one of them can take it, so wake one instead of all (which
/// stampedes every blocked thread for a single handoff). Waiters share the condvar, so with any
/// present wake all, and each re-checks its own condition.
fn wake_after_release(m: &ObjectMonitor, s: &MonitorState) {
    if s.waiters == 0 {
        m.cv.notify_one();
    } else {
        m.cv.notify_all();
    }
}

/// `monitorenter`: acquire the object's monitor, blocking until it is free; reentrant for the
/// current owner.
pub(crate) fn enter(obj_ref: i32) {
    let depth = JavaThread::root_monitor(obj_ref);
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    s.entrants += 1;
    let contended = s.owner.is_some() && s.owner != Some(tid);
    let activity = if contended {
        threads::current_activity()
    } else {
        None
    };
    if contended {
        if let Some(activity) = &activity {
            activity.blocked_count.fetch_add(1, Ordering::Relaxed);
            activity.contended_monitor.store(obj_ref, Ordering::Relaxed);
        }
        threads::set_current_thread_status(thread_status::BLOCKED);
        // Short critical sections are the common case: spin briefly before parking, as HotSpot
        // does, instead of paying a safepoint transition and an OS sleep/wake for every handoff.
        // Bounded, and without holding the state lock, so a safepoint is not held up.
        for round in 0..SPIN_ROUNDS {
            if s.owner.is_none() {
                break;
            }
            drop(s);
            if round < SPIN_ROUNDS / 2 {
                for _ in 0..(32u32 << round) {
                    std::hint::spin_loop();
                }
            } else {
                std::thread::yield_now();
            }
            s = safepoint::lock(&m.state);
        }
    }
    while s.owner.is_some() && s.owner != Some(tid) {
        s = safepoint::wait(&m.cv, &m.state, s);
    }
    s.entrants -= 1;
    if s.owner == Some(tid) {
        s.count += 1;
    } else {
        s.owner = Some(tid);
        s.count = 1;
        s.lock_depth = depth;
    }
    if contended {
        if let Some(activity) = &activity {
            activity.contended_monitor.store(0, Ordering::Relaxed);
        }
        threads::set_current_thread_status(thread_status::RUNNABLE);
    }
}

/// The `java.lang.Thread` owning the monitor of `obj_ref`, if any thread does.
pub(crate) fn owner_thread(obj_ref: i32) -> Option<i32> {
    let monitor = Arc::clone(MONITORS.get(&obj_ref)?.value());
    let owner = safepoint::lock(&monitor.state).owner?;
    threads::thread_of(owner)
}

/// Every monitor the OS thread `owner` holds, as `(object, depth of the locking frame)`.
pub(crate) fn owned_by(owner: ThreadId) -> Vec<(i32, usize)> {
    let monitors: Vec<_> = MONITORS
        .iter()
        .map(|entry| (*entry.key(), Arc::clone(entry.value())))
        .collect();
    monitors
        .into_iter()
        .filter_map(|(obj_ref, monitor)| {
            let state = safepoint::lock(&monitor.state);
            (state.owner == Some(owner)).then_some((obj_ref, state.lock_depth))
        })
        .collect()
}

/// `monitorexit`: release one level of the current thread's ownership. A mismatch (not the owner)
/// is a broken-bytecode situation; we surface it as `IllegalMonitorStateException` like the spec.
pub(crate) fn exit(obj_ref: i32) -> Result<()> {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    if s.owner != Some(tid) {
        drop(s);
        return set_pending_illegal_monitor_state_exception(
            "current thread is not the owner of the monitor",
        );
    }
    s.count -= 1;
    if s.count == 0 {
        s.owner = None;
        wake_after_release(&m, &s);
    }
    drop(s);
    JavaThread::unroot_monitor(obj_ref);
    Ok(())
}

/// Releases the monitor acquired implicitly for a synchronized method. The caller reports
/// `false` as `IllegalMonitorStateException` so an exception already unwinding is not silently
/// replaced by a Rust-side monitor error.
pub(crate) fn exit_synchronized_method(obj_ref: i32) -> bool {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    if s.owner != Some(tid) {
        drop(s);
        JavaThread::unroot_monitor_if_present(obj_ref);
        return false;
    }
    s.count -= 1;
    if s.count == 0 {
        s.owner = None;
        wake_after_release(&m, &s);
    }
    drop(s);
    JavaThread::unroot_monitor_if_present(obj_ref);
    true
}

/// `Object.wait(timeout)`: atomically releases the monitor (all reentrant levels), blocks until
/// notified/timed-out, then re-acquires with the same recursion depth. `timeout_millis == 0` waits
/// indefinitely. Must be called by the monitor's owner.
pub(crate) fn wait(obj_ref: i32, timeout_millis: i64) -> Result<()> {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    if s.owner != Some(tid) {
        drop(s);
        return set_pending_illegal_monitor_state_exception(
            "current thread is not the owner of the monitor",
        );
    }
    // Already interrupted on entry: throw without ever releasing the monitor (per spec).
    if take_current_interrupt() {
        drop(s);
        return set_pending_interrupted_exception();
    }

    // Fully release the monitor, remembering the recursion depth to restore on re-acquire.
    let saved_count = s.count;
    let saved_depth = s.lock_depth;
    let activity = threads::current_activity();
    if let Some(activity) = &activity {
        activity.waited_count.fetch_add(1, Ordering::Relaxed);
        activity.contended_monitor.store(obj_ref, Ordering::Relaxed);
    }
    s.owner = None;
    s.count = 0;
    s.waiters += 1;
    let ticket = s.next_ticket;
    s.next_ticket += 1;
    s.wait_set.push_back(ticket);
    m.cv.notify_all(); // hand the monitor to the entry set

    // Publish which monitor we wait on so interrupt0 can target it. Registered/unregistered under
    // the state lock so it pairs with `interrupt_waiter`'s locked notify (no lost interrupt).
    if let Some(thread_ref) = JavaThread::current_thread() {
        WAIT_TARGETS.insert(thread_ref, Arc::clone(&m));
    }

    threads::set_current_thread_status(if timeout_millis > 0 {
        thread_status::IN_OBJECT_WAIT_TIMED
    } else {
        thread_status::IN_OBJECT_WAIT
    });

    let deadline = (timeout_millis > 0)
        .then(|| Instant::now() + Duration::from_millis(timeout_millis as u64));
    let mut interrupted = false;
    loop {
        if s.notified.remove(&ticket) {
            break;
        }
        if take_current_interrupt() {
            interrupted = true;
            break;
        }
        match deadline {
            None => {
                s = safepoint::wait(&m.cv, &m.state, s);
            }
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    break;
                }
                let (guard, timeout) = safepoint::wait_for(&m.cv, &m.state, s, dl - now);
                s = guard;
                if timeout.timed_out() && !s.notified.contains(&ticket) {
                    break;
                }
            }
        }
    }
    // Leaving by timeout or interrupt: withdraw the ticket. A notification that raced with it is
    // dropped, as `Object.wait` permits.
    s.wait_set.retain(|waiting| *waiting != ticket);
    s.notified.remove(&ticket);
    s.waiters -= 1;
    if let Some(thread_ref) = JavaThread::current_thread() {
        WAIT_TARGETS.remove(&thread_ref);
    }

    // Re-acquire ownership before returning to (or throwing back into) the synchronized region.
    // While contending to re-enter, the thread is BLOCKED, not WAITING (Thread.State semantics) —
    // and since notify() is usually issued while still holding the lock, this window is common.
    if s.owner.is_some() && s.owner != Some(tid) {
        threads::set_current_thread_status(thread_status::BLOCKED);
    }
    s.entrants += 1;
    while s.owner.is_some() && s.owner != Some(tid) {
        s = safepoint::wait(&m.cv, &m.state, s);
    }
    s.entrants -= 1;
    s.owner = Some(tid);
    s.count = saved_count;
    s.lock_depth = saved_depth;
    if let Some(activity) = &activity {
        activity.contended_monitor.store(0, Ordering::Relaxed);
    }
    threads::set_current_thread_status(thread_status::RUNNABLE);

    if interrupted {
        drop(s);
        return set_pending_interrupted_exception();
    }
    Ok(())
}

/// Wakes the given thread if it is currently in `Object.wait`, so it re-checks its interrupt status.
/// Locking the monitor's state around the notify pairs with the waiter's under-lock interrupt check,
/// preventing a lost wakeup when the interrupt lands between the waiter's check and its `wait`.
pub(crate) fn interrupt_waiter(thread_ref: i32) {
    // Clone the Arc out and release the DashMap shard lock *before* locking the monitor state:
    // `wait()` holds the state lock while it insert/removes into WAIT_TARGETS (state -> shard), so
    // acquiring them here in the opposite order (shard -> state) could deadlock.
    let monitor = WAIT_TARGETS
        .get(&thread_ref)
        .map(|entry| entry.value().clone());
    if let Some(m) = monitor {
        let _guard = safepoint::lock(&m.state);
        m.cv.notify_all();
    }
}

/// `Object.notify`: release at most one waiter. Must be called by the monitor's owner.
pub(crate) fn notify(obj_ref: i32) -> Result<()> {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    if s.owner != Some(tid) {
        drop(s);
        return set_pending_illegal_monitor_state_exception(
            "current thread is not the owner of the monitor",
        );
    }
    if let Some(ticket) = s.wait_set.pop_front() {
        s.notified.insert(ticket);
        m.cv.notify_all();
    }
    Ok(())
}

/// `Object.notifyAll`: release every current waiter. Must be called by the monitor's owner.
pub(crate) fn notify_all(obj_ref: i32) -> Result<()> {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    if s.owner != Some(tid) {
        drop(s);
        return set_pending_illegal_monitor_state_exception(
            "current thread is not the owner of the monitor",
        );
    }
    notify_every_waiter(&mut s);
    m.cv.notify_all();
    Ok(())
}

/// Selects every thread currently in the wait set.
fn notify_every_waiter(s: &mut MonitorState) {
    let MonitorState {
        wait_set, notified, ..
    } = s;
    notified.extend(wait_set.drain(..));
}

/// `Thread.holdsLock`: whether the calling thread owns the object's monitor.
pub(crate) fn holds_lock(obj_ref: i32) -> bool {
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let s = safepoint::lock(&m.state);
    s.owner == Some(tid)
}

/// VM-internal wake used when a thread terminates: acquires the object's monitor, wakes every
/// waiter, and releases. This is how a dying thread satisfies `Thread.join`, whose waiters sit in
/// `synchronized(thread) { while (isAlive()) wait(); }` — mirroring HotSpot's `lock.notify_all` in
/// `JavaThread::exit`. Call *after* the liveness flag (`eetop`) has been cleared.
pub(crate) fn wake_all_on_exit(obj_ref: i32) {
    // Deliberately not `enter`/`exit`: the dying thread's status is already TERMINATED, and
    // `enter` would rewrite it to BLOCKED/RUNNABLE on contention. The target is rooted for the
    // whole acquisition because a contended wait here can cross a safepoint.
    JavaThread::root_monitor(obj_ref);
    let m = monitor_for(obj_ref);
    let tid = std::thread::current().id();
    let mut s = safepoint::lock(&m.state);
    s.entrants += 1;
    while s.owner.is_some() && s.owner != Some(tid) {
        s = safepoint::wait(&m.cv, &m.state, s);
    }
    s.entrants -= 1;
    s.owner = Some(tid);
    s.count = 1;
    notify_every_waiter(&mut s);
    m.cv.notify_all();
    // release
    s.count = 0;
    s.owner = None;
    m.cv.notify_all();
    drop(s);
    JavaThread::unroot_monitor(obj_ref);
}
