//! `ThreadMXBean` thread snapshots and deadlock detection: the `sun.management.ThreadImpl`
//! natives HotSpot serves from `ThreadService` (`ThreadSnapshot`, `DeadlockCycle`).
//!
//! HotSpot takes these snapshots at a safepoint. Here each thread is sampled on its own: its
//! status, what it is blocked on and its stack are read one after the other, so a thread that
//! moves on in between can be reported in a slightly mixed state. Blocked and waiting threads,
//! which is what these APIs are for, do not move.

use crate::vm::error::Result;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::vec_to_i64;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::instance_checker::InstanceChecker;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::monitor;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_frames_util::StackElement;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::management::live_threads;
use crate::vm::system_native::thread::{stack_trace_elements, thread_frames};
use crate::vm::threads::{self, thread_status};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

const THREAD: &str = "java/lang/Thread";
const THREAD_INFO: &str = "java/lang/management/ThreadInfo";
const OWNABLE_SYNCHRONIZER: &str = "java/util/concurrent/locks/AbstractOwnableSynchronizer";

fn thread_id(thread_ref: i32) -> Result<i64> {
    Ok(vec_to_i64(
        &HEAP.get_object_field_value(thread_ref, THREAD, "tid")?,
    ))
}

fn thread_status_of(thread_ref: i32) -> Result<i32> {
    let holder = HEAP.get_object_field_value(thread_ref, THREAD, "holder")?[0];
    if holder == 0 {
        return Ok(thread_status::NEW);
    }
    Ok(HEAP.get_object_field_value(holder, "java/lang/Thread$FieldHolder", "threadStatus")?[0])
}

/// Live platform threads by `Thread.threadId()`.
fn threads_by_id() -> Result<HashMap<i64, i32>> {
    live_threads()
        .into_iter()
        .map(|thread_ref| Ok((thread_id(thread_ref)?, thread_ref)))
        .collect()
}

fn is_ownable_synchronizer(object_ref: i32) -> Result<bool> {
    InstanceChecker::checkcast(&HEAP.get_instance_name(object_ref)?, OWNABLE_SYNCHRONIZER)
}

fn synchronizer_owner(synchronizer_ref: i32) -> Result<i32> {
    Ok(HEAP.get_object_field_value(
        synchronizer_ref,
        OWNABLE_SYNCHRONIZER,
        "exclusiveOwnerThread",
    )?[0])
}

/// What a thread is blocked on, as HotSpot's `ThreadSnapshot` works it out: the status to
/// report, the object, and the thread owning that object (0 for none).
///
/// A thread entering or waiting on a monitor is blocked on that monitor's object; a parked
/// thread on its park blocker, which has an owner when it is an ownable synchronizer.
struct Blocker {
    status: i32,
    object: i32,
    owner: i32,
}

fn blocker_of(thread_ref: i32) -> Result<Blocker> {
    let status = thread_status_of(thread_ref)?;
    let unblocked = |status| Blocker {
        status,
        object: 0,
        owner: 0,
    };
    if status & (thread_status::BLOCKED_BIT | thread_status::IN_OBJECT_WAIT_BIT) != 0 {
        let object = threads::activity(thread_ref)
            .contended_monitor
            .load(Ordering::Relaxed);
        let owner = if object == 0 {
            None
        } else {
            monitor::owner_thread(object)
        };
        let entering = status & thread_status::BLOCKED_BIT != 0;
        // The monitor was handed over between the two reads: the thread is no longer blocked.
        if object == 0 || (entering && owner.is_none()) {
            return Ok(unblocked(thread_status::RUNNABLE));
        }
        return Ok(Blocker {
            status,
            object,
            owner: owner.unwrap_or(0),
        });
    }
    if status & thread_status::PARKED_BIT != 0 {
        let object = HEAP.get_object_field_value(thread_ref, THREAD, "parkBlocker")?[0];
        let owner = if object != 0 && is_ownable_synchronizer(object)? {
            synchronizer_owner(object)?
        } else {
            0
        };
        return Ok(Blocker {
            status,
            object,
            owner,
        });
    }
    Ok(unblocked(status))
}

/// Every ownable synchronizer (`ReentrantLock` and friends) in the heap by owning thread.
fn locked_synchronizers() -> Result<HashMap<i32, Vec<i32>>> {
    let mut ownable_classes: HashMap<usize, bool> = HashMap::new();
    let candidates = HEAP.instances_where(|klass_id| {
        *ownable_classes.entry(klass_id).or_insert_with(|| {
            CLASSES
                .get_by_id(klass_id)
                .and_then(|class| {
                    InstanceChecker::checkcast(class.this_class_name(), OWNABLE_SYNCHRONIZER)
                })
                .unwrap_or(false)
        })
    });
    let mut by_owner: HashMap<i32, Vec<i32>> = HashMap::new();
    for synchronizer in candidates {
        let owner = synchronizer_owner(synchronizer)?;
        if owner != 0 {
            by_owner.entry(owner).or_default().push(synchronizer);
        }
    }
    Ok(by_owner)
}

/// What a `ThreadInfo` should say beyond the basics.
struct Detail<'a> {
    /// Frames to report: negative for all, 0 for none.
    max_depth: i32,
    locked_monitors: bool,
    /// `Some` when locked synchronizers were asked for.
    synchronizers: Option<&'a HashMap<i32, Vec<i32>>>,
}

/// Builds the `ThreadInfo` of a live thread through the constructors the JDK keeps for the VM.
fn thread_info(thread_ref: i32, detail: &Detail) -> Result<i32> {
    let mut roots = JavaThread::hold_temp_roots(vec![Slot::Ref(thread_ref)]);
    let blocker = blocker_of(thread_ref)?;
    roots.add_root(Slot::Ref(blocker.object));
    roots.add_root(Slot::Ref(blocker.owner));
    let activity = threads::activity(thread_ref);

    let mut dump_roots = RootedSlots::new();
    let all_frames: Vec<StackElement> = if detail.max_depth != 0 || detail.locked_monitors {
        thread_frames(thread_ref, &mut dump_roots)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    let reported = match detail.max_depth {
        depth if depth < 0 => all_frames.len(),
        depth => all_frames.len().min(depth as usize),
    };
    let stack_trace_ref = if detail.max_depth == 0 {
        0 // null: `ThreadInfo` substitutes its empty trace
    } else {
        stack_trace_elements(&all_frames[..reported])?
    };
    roots.add_root(Slot::Ref(stack_trace_ref));

    let mut args = vec![
        StackValueKind::Ref(thread_ref),
        StackValueKind::I32(blocker.status),
        StackValueKind::Ref(blocker.object),
        StackValueKind::Ref(blocker.owner),
        StackValueKind::I64(activity.blocked_count.load(Ordering::Relaxed)),
        StackValueKind::I64(-1), // blocked time: contention monitoring is not supported
        StackValueKind::I64(activity.waited_count.load(Ordering::Relaxed)),
        StackValueKind::I64(-1),
        StackValueKind::Ref(stack_trace_ref),
    ];
    let basic = "<init>:(Ljava/lang/Thread;ILjava/lang/Object;Ljava/lang/Thread;JJJJ[Ljava/lang/StackTraceElement;)V";
    if !detail.locked_monitors && detail.synchronizers.is_none() {
        return Ok(Executor::invoke_args_constructor(THREAD_INFO, basic, &args, None)?[0].value());
    }

    // A monitor is reported with the index of its locking frame in the trace; one taken in a
    // frame beyond the reported depth is left out, as it is by HotSpot.
    let mut monitors: Vec<(i32, i32)> = Vec::new();
    if detail.locked_monitors {
        if let Some(os_thread) = threads::os_thread_of(thread_ref) {
            for (object, lock_depth) in monitor::owned_by(os_thread) {
                let frame = all_frames.len() as i64 - lock_depth as i64;
                if lock_depth == 0 || frame < 0 {
                    monitors.push((object, -1)); // taken outside any Java frame (JNI)
                } else if (frame as usize) < reported {
                    monitors.push((object, frame as i32));
                }
            }
        }
        monitors.sort_by_key(|(_, frame)| *frame);
    }
    for (object, _) in &monitors {
        roots.add_root(Slot::Ref(*object));
    }
    let synchronizers = detail
        .synchronizers
        .and_then(|by_owner| by_owner.get(&thread_ref))
        .cloned()
        .unwrap_or_default();
    for synchronizer in &synchronizers {
        roots.add_root(Slot::Ref(*synchronizer));
    }

    let objects = |refs: &[i32], roots: &mut crate::vm::jni::java_thread::TempRootsGuard| {
        let array_ref =
            HEAP.create_array_with_values("[Ljava/lang/Object;", refs, &mut RootedSlots::new())?;
        roots.add_root(Slot::Ref(array_ref));
        Ok::<_, crate::vm::error::Error>(array_ref)
    };
    let monitor_refs: Vec<i32> = monitors.iter().map(|(object, _)| *object).collect();
    let monitor_depths: Vec<i32> = monitors.iter().map(|(_, frame)| *frame).collect();
    let monitors_ref = objects(&monitor_refs, &mut roots)?;
    let depths_ref =
        HEAP.create_array_with_values("[I", &monitor_depths, &mut RootedSlots::new())?;
    roots.add_root(Slot::Ref(depths_ref));
    let synchronizers_ref = objects(&synchronizers, &mut roots)?;

    args.extend([
        StackValueKind::Ref(monitors_ref),
        StackValueKind::Ref(depths_ref),
        StackValueKind::Ref(synchronizers_ref),
    ]);
    let detailed = "<init>:(Ljava/lang/Thread;ILjava/lang/Object;Ljava/lang/Thread;JJJJ[Ljava/lang/StackTraceElement;[Ljava/lang/Object;[I[Ljava/lang/Object;)V";
    Ok(Executor::invoke_args_constructor(THREAD_INFO, detailed, &args, None)?[0].value())
}

fn requested_ids(ids_ref: i32) -> Result<Vec<i64>> {
    (0..HEAP.get_array_len(ids_ref)?)
        .map(|index| Ok(vec_to_i64(&HEAP.get_array_value(ids_ref, index)?)))
        .collect()
}

/// `ThreadImpl.getThreadInfo1([JI[Ljava/lang/management/ThreadInfo;)V`
///
/// Fills `result[i]` for `ids[i]`, leaving null for an id that is not a live thread.
pub(crate) fn get_thread_info1(ids_ref: i32, max_depth: i32, result_ref: i32) -> Result<()> {
    let live = threads_by_id()?;
    let detail = Detail {
        max_depth,
        locked_monitors: false,
        synchronizers: None,
    };
    for (index, id) in requested_ids(ids_ref)?.into_iter().enumerate() {
        let info_ref = match live.get(&id) {
            Some(&thread_ref) => thread_info(thread_ref, &detail)?,
            None => 0,
        };
        HEAP.set_array_value(result_ref, index as i32, vec![info_ref])?;
    }
    Ok(())
}

/// `ThreadImpl.dumpThreads0([JZZI)[Ljava/lang/management/ThreadInfo;`
///
/// `ThreadInfo`s with lock details for the given ids (null entries for ids that are not live
/// threads), or for every live thread when `ids` is null.
pub(crate) fn dump_threads0(
    ids_ref: i32,
    locked_monitors: bool,
    locked_synchronizers: bool,
    max_depth: i32,
) -> Result<i32> {
    let targets: Vec<Option<i32>> = if ids_ref == 0 {
        let mut all = live_threads();
        all.sort_unstable();
        all.into_iter().map(Some).collect()
    } else {
        let live = threads_by_id()?;
        requested_ids(ids_ref)?
            .into_iter()
            .map(|id| live.get(&id).copied())
            .collect()
    };
    let mut roots = JavaThread::hold_temp_roots(
        targets
            .iter()
            .flatten()
            .map(|thread_ref| Slot::Ref(*thread_ref))
            .collect(),
    );
    let synchronizers = if locked_synchronizers {
        let by_owner = locked_synchronizers_rooted(&mut roots)?;
        Some(by_owner)
    } else {
        None
    };
    let detail = Detail {
        max_depth,
        locked_monitors,
        synchronizers: synchronizers.as_ref(),
    };

    let mut destination = RootedSlots::new();
    let result_ref = HEAP.create_array(
        "[Ljava/lang/management/ThreadInfo;",
        targets.len() as i32,
        &mut destination,
    )?;
    for (index, target) in targets.iter().enumerate() {
        if let Some(thread_ref) = target {
            let info_ref = thread_info(*thread_ref, &detail)?;
            HEAP.set_array_value(result_ref, index as i32, vec![info_ref])?;
        }
    }
    Ok(result_ref)
}

fn locked_synchronizers_rooted(
    roots: &mut crate::vm::jni::java_thread::TempRootsGuard,
) -> Result<HashMap<i32, Vec<i32>>> {
    let by_owner = locked_synchronizers()?;
    for synchronizer in by_owner.values().flatten() {
        roots.add_root(Slot::Ref(*synchronizer));
    }
    Ok(by_owner)
}

/// The thread `thread_ref` cannot proceed without: the owner of the monitor it is entering or,
/// when `synchronizers` count, of the ownable synchronizer it is parked on.
fn waits_for(thread_ref: i32, synchronizers: bool) -> Result<Option<i32>> {
    let status = thread_status_of(thread_ref)?;
    if status & thread_status::BLOCKED_BIT != 0 {
        let object = threads::activity(thread_ref)
            .contended_monitor
            .load(Ordering::Relaxed);
        return Ok(if object == 0 {
            None
        } else {
            monitor::owner_thread(object)
        });
    }
    if synchronizers && status & thread_status::PARKED_BIT != 0 {
        let blocker = HEAP.get_object_field_value(thread_ref, THREAD, "parkBlocker")?[0];
        if blocker != 0 && is_ownable_synchronizer(blocker)? {
            return Ok(Some(synchronizer_owner(blocker)?).filter(|owner| *owner != 0));
        }
    }
    Ok(None)
}

/// HotSpot's `ThreadService::find_deadlocks_at_safepoint`: follows each thread's wait-for chain
/// and reports the threads of every chain that runs into itself, or null when there is none.
fn find_deadlocks(synchronizers: bool) -> Result<i32> {
    let mut all = live_threads();
    all.sort_unstable();
    let _roots =
        JavaThread::hold_temp_roots(all.iter().map(|thread| Slot::Ref(*thread)).collect());

    // Depth-first numbers, as in HotSpot: a chain that reaches a thread numbered during this
    // very walk has found a cycle; one that reaches an earlier walk's thread has not.
    let mut numbers: HashMap<i32, usize> = HashMap::new();
    let mut next_number = 0;
    let mut deadlocked: Vec<i32> = Vec::new();
    for &start in &all {
        if numbers.contains_key(&start) {
            continue;
        }
        let walk_start = next_number;
        let mut chain = vec![start];
        numbers.insert(start, next_number);
        next_number += 1;
        let mut current = start;
        while let Some(next) = waits_for(current, synchronizers)? {
            match numbers.get(&next) {
                None => {
                    numbers.insert(next, next_number);
                    next_number += 1;
                    chain.push(next);
                    current = next;
                }
                // A thread waiting for itself is not a deadlock; neither is a known dead end.
                Some(&number) if number < walk_start || next == current => break,
                Some(_) => {
                    deadlocked.extend(&chain);
                    break;
                }
            }
        }
    }
    if deadlocked.is_empty() {
        return Ok(0);
    }
    HEAP.create_array_with_values("[Ljava/lang/Thread;", &deadlocked, &mut RootedSlots::new())
}

/// `ThreadImpl.findDeadlockedThreads0()[Ljava/lang/Thread;`: cycles through monitors and
/// ownable synchronizers.
pub(crate) fn find_deadlocked_threads0() -> Result<i32> {
    find_deadlocks(true)
}

/// `ThreadImpl.findMonitorDeadlockedThreads0()[Ljava/lang/Thread;`: cycles through monitors only.
pub(crate) fn find_monitor_deadlocked_threads0() -> Result<i32> {
    find_deadlocks(false)
}
