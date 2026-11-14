//! Precise, nonmoving mark/sweep under the coordinator's exclusive Operation.
use super::budget::AllocError;
use super::pending::{PendingList, PENDING};
use super::references::{Kind, Reference};
use super::{roots, trigger};
use crate::vm::error::Result;
use crate::vm::heap::heap::{Heap, HEAP};
use crate::vm::jni::{handles, java_thread::JavaThread};
use crate::vm::safepoint::{self, Operation, COORDINATOR};
use std::collections::HashSet;
use std::mem::size_of;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause {
    Explicit,
    Threshold,
    Stress,
    Pressure,
}

#[derive(Debug)]
pub(crate) struct Stats {
    pub(crate) before: usize,
    pub(crate) live: usize,
    pub(crate) reclaimed: u64,
    pub(crate) discovered: usize,
    pub(crate) scratch_bytes: usize,
}

struct Trace {
    marked: HashSet<i32>,
    work: Vec<i32>,
    references: Vec<Reference>,
    limit: usize,
}

impl Trace {
    fn new(limit: usize) -> Result<Self> {
        let mut trace = Self {
            marked: HashSet::new(),
            work: Vec::new(),
            references: Vec::new(),
            limit,
        };
        trace
            .marked
            .try_reserve(limit)
            .map_err(|_| AllocError::HostAllocation)?;
        trace
            .work
            .try_reserve_exact(limit)
            .map_err(|_| AllocError::HostAllocation)?;
        trace
            .references
            .try_reserve_exact(limit)
            .map_err(|_| AllocError::HostAllocation)?;
        Ok(trace)
    }

    fn mark(&mut self, reference: i32) -> Result<()> {
        if reference == 0 || self.marked.contains(&reference) {
            return Ok(());
        }
        // Even corrupted edges cannot grow scratch past preflight. Each valid
        // object enters the worklist once, irrespective of edge count or key age.
        if reference < 0 || self.marked.len() == self.limit {
            return Err(AllocError::InvalidLayout.into());
        }
        self.marked.insert(reference);
        self.work.push(reference);
        Ok(())
    }

    fn close(&mut self, heap: &Heap, pressure: bool) -> Result<()> {
        while let Some(object) = self.work.pop() {
            let reference = heap.trace_edges(object, |edge| self.mark(edge))?;
            if let Some(reference) = reference {
                if reference.referent != 0 && !heap.contains(reference.referent) {
                    return Err(AllocError::InvalidLayout.into());
                }
                match reference.kind {
                    Kind::Soft if !pressure => self.mark(reference.referent)?,
                    // Finalization is disabled. Never discover a FinalReference,
                    // or free a referent that Java could still read through one.
                    Kind::Final | Kind::Other => self.mark(reference.referent)?,
                    Kind::Soft | Kind::Weak | Kind::Phantom => {}
                }
                self.references.push(reference);
            }
        }
        // Retained soft edges join this same worklist, including newly reached
        // soft references, so closure reaches a fixed point without recursion.
        Ok(())
    }

    fn scratch_bytes(&self) -> usize {
        // HashSet exposes element capacity, not bucket bytes. Report a conservative
        // bucket/control-byte bound; tests also measure actual allocator requests.
        let hash_bytes = self
            .marked
            .capacity()
            .saturating_mul(2)
            .saturating_add(16)
            .saturating_mul(size_of::<i32>() + 1);
        hash_bytes
            + self.work.capacity() * size_of::<i32>()
            + self.references.capacity() * size_of::<Reference>()
    }
}

/// One attempt, with skipped-cycle policy for ANY snapshot/preflight/trace failure.
/// Dropping an incomplete Operation opens the coordinator without reporting a
/// successful epoch. No retry loop can spin on a lock held by a Safe thread.
/// Completed collections and the wall time they took, for `GarbageCollectorMXBean`.
pub(crate) static COLLECTION_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static COLLECTION_NANOS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn collect(cause: Cause) -> Option<Stats> {
    let started = std::time::Instant::now();
    let owner = safepoint::current()?;
    let operation = COORDINATOR.request_collection_for(
        owner,
        JavaThread::snapshot_roots(),
        cause == Cause::Pressure,
    )?;
    let cause = if operation.pressure() {
        Cause::Pressure
    } else {
        cause
    };
    let result = roots::snapshot_retrying(&operation)
        .and_then(|snapshot| collect_snapshot(&HEAP, &snapshot, &PENDING, cause, &operation));
    match result {
        Ok(stats) => {
            trigger::completed(stats.live);
            tracing::info!(target: "gc",
                "[gc] {:?}: {} -> {} objects, reclaimed={} bytes, pending={}, scratch<={} bytes",
                cause, stats.before, stats.live, stats.reclaimed, stats.discovered, stats.scratch_bytes);
            operation.complete();
            COLLECTION_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            COLLECTION_NANOS.fetch_add(
                started.elapsed().as_nanos() as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            Some(stats)
        }
        Err(error) => {
            tracing::info!(target: "gc", "[gc] {:?}: skipped: {}", cause, error);
            None
        }
    }
}

/// Unit seams use the same traversal and real heap, still requiring an Operation.
/// The snapshot is the ONLY source of roots; never consult live mutator state here.
pub(crate) fn collect_snapshot(
    heap: &Heap,
    snapshot: &[i32],
    pending: &PendingList,
    cause: Cause,
    _operation: &Operation,
) -> Result<Stats> {
    let entries = heap.collection_entries()?;
    let mut trace = Trace::new(entries.len())?;
    let mut pending_head = pending.for_collection()?;
    for &root in snapshot {
        trace.mark(root)?;
    }
    trace.close(heap, cause == Cause::Pressure)?;
    // Marking has checked every live ID under its heap guard. Check the remaining
    // entries before any mutation, rather than acquiring every live shard twice.
    for &(reference, _) in &entries {
        if !trace.marked.contains(&reference) && !heap.contains(reference) {
            return Err(AllocError::InvalidLayout.into());
        }
    }

    trace.references.retain(|reference| {
        reference.active()
            && !trace.marked.contains(&reference.referent)
            && matches!(reference.kind, Kind::Soft | Kind::Weak | Kind::Phantom)
    });
    // The trace read both links of every discovered reference; check that both can be written.
    for reference in &trace.references {
        heap.check_reference_links(reference.object)?;
    }

    // All roots, strong edges, referents, links and accounting have now been validated, and
    // the collector fails closed from here on: nothing below can be skipped half-done.
    // Nothing below allocates scratch or takes admission while a heap guard lives.
    // Weak JNI identities survive; only their target is cleared.
    handles::clear_weak_references(|reference| trace.marked.contains(&reference));
    for reference in &trace.references {
        validated(
            heap.set_reference_link(reference.object, "referent", 0),
            "clearing a referent",
        );
        validated(
            heap.set_reference_link(reference.object, "discovered", *pending_head),
            "publishing a pending reference",
        );
        *pending_head = reference.object;
    }
    let mut reclaimed = 0;
    for &(reference, _) in &entries {
        if !trace.marked.contains(&reference) {
            reclaimed += validated(
                heap.remove_recorded(reference).map_err(Into::into),
                "sweeping",
            );
        }
    }
    crate::vm::monitor::prune_idle(|reference| trace.marked.contains(&reference));
    if !trace.references.is_empty() {
        pending.notify();
    }
    Ok(Stats {
        before: entries.len(),
        live: trace.marked.len(),
        reclaimed,
        discovered: trace.references.len(),
        scratch_bytes: trace.scratch_bytes() + entries.capacity() * size_of::<(i32, u64)>(),
    })
}

/// Unwraps a mutation whose preconditions the collector has already validated.
///
/// Failing here means the heap changed under a stopped world, a VM bug. Resuming the mutators over
/// a partially processed heap (some referents cleared, some objects freed) could only spread the
/// corruption, so the VM stops instead of reporting a skipped cycle.
fn validated<T>(result: Result<T>, step: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "rusty-jvm: fatal: garbage collection invariant violated while {step}: {error}"
            );
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests;
