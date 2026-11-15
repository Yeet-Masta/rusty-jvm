use super::charge;
use super::heap::Heap;
use super::java_instance::HeapValue;
use crate::vm::gc::budget::{AllocError, AllocResult, Budget};
use crate::vm::gc::{collector, trigger};
use crate::vm::stack::rooted_slots::RootedSlots;

#[derive(Debug)]
pub(super) struct Admission {
    pub(super) budget: Budget,
    pub(super) next: Option<i32>,
}

impl Admission {
    pub(super) fn new(max: u64, enforced: bool) -> Self {
        Self {
            budget: Budget::new(max, enforced),
            next: Some(1),
        }
    }
}

/// Owns only a numeric debit, never a lock or a coordinator ticket. Payload building
/// must not poll, call Java, or park between reserve and commit/cancel.
pub(crate) struct Reservation<'a> {
    heap: &'a Heap,
    charge: u64,
    active: bool,
}

impl Heap {
    /// Publishes `value` under the next key against a reserved `charge`, with `admission` held.
    /// A failure publishes nothing and leaves the reservation in place, for its owner to cancel.
    fn publish(
        &self,
        admission: &mut Admission,
        charge: u64,
        value: HeapValue,
        destination: &mut RootedSlots,
    ) -> AllocResult<i32> {
        let recorded = value.charge()?;
        let key = admission
            .next
            .filter(|key| *key > 0)
            .ok_or(AllocError::IdentityExhausted)?;
        // Validate all accounting before insertion. Holding the admission mutex serializes the
        // key/insert/root triple.
        if recorded > charge {
            return Err(AllocError::InvalidLayout);
        }
        let used = admission
            .budget
            .used
            .checked_add(recorded)
            .ok_or(AllocError::Arithmetic)?;
        let reserved = admission
            .budget
            .reserved
            .checked_sub(charge)
            .ok_or(AllocError::Arithmetic)?;
        let total = used.checked_add(reserved).ok_or(AllocError::Arithmetic)?;
        if admission.budget.enforced && total > admission.budget.max {
            return Err(AllocError::Capacity);
        }
        let tag = match &value {
            HeapValue::Object(instance) => super::heap::klass_tag(instance.klass_id()),
            HeapValue::Arr(_) => super::heap::ARRAY_TAG,
        };
        // Rejects an occupied key before anything is published.
        self.data.insert_tagged(key, value, recorded, tag)?;
        admission.budget.commit(charge, recorded)?;
        admission.next = key.checked_add(1);
        destination.publish_single_ref(key);
        trigger::allocated();
        Ok(key)
    }

    /// Reserves `charge`, builds the value and publishes it in one admission critical section
    /// instead of two. The value is built only once its charge is reserved, so a refused
    /// allocation builds nothing. At capacity, the reservation is retried after a collection,
    /// outside the lock, as [`Self::reserve`] does.
    ///
    /// `build` must not poll, call Java, park or allocate on this heap.
    pub(crate) fn allocate_with(
        &self,
        charge: u64,
        build: impl FnOnce() -> crate::vm::error::Result<HeapValue>,
        destination: &mut RootedSlots,
    ) -> crate::vm::error::Result<i32> {
        trigger::before_allocation();
        let mut admission = self.admission.lock();
        let mut reserved = self.try_reserve_locked(&mut admission, charge);
        if matches!(reserved, Err(AllocError::Capacity)) && trigger::enabled() {
            drop(admission);
            collector::collect(collector::Cause::Pressure);
            admission = self.admission.lock();
            reserved = self.try_reserve_locked(&mut admission, charge);
        }
        reserved?;
        // The debit is reserved from here on: every failure cancels it before returning.
        let published = build().and_then(|value| {
            Ok(self.publish(&mut admission, charge, value, destination)?)
        });
        if published.is_err() {
            admission
                .budget
                .cancel(charge)
                .expect("a live reservation owns its exact reserved debit");
        }
        published
    }

    fn try_reserve_locked(&self, admission: &mut Admission, charge: u64) -> AllocResult<()> {
        let key = admission
            .next
            .filter(|key| *key > 0)
            .ok_or(AllocError::IdentityExhausted)?;
        if self.data.contains_key(key) {
            return Err(AllocError::IdentityExhausted);
        }
        admission.budget.reserve(charge)
    }

    pub(crate) fn reserve(&self, charge: u64) -> AllocResult<Reservation<'_>> {
        trigger::before_allocation();
        match self.try_reserve(charge) {
            Err(AllocError::Capacity) if trigger::enabled() => {
                // try_reserve has dropped admission and created no reservation or
                // staged payload. Collect/join once, then compete atomically again.
                collector::collect(collector::Cause::Pressure);
                self.try_reserve(charge)
            }
            result => result,
        }
    }

    fn try_reserve(&self, charge: u64) -> AllocResult<Reservation<'_>> {
        let mut admission = self.admission.lock();
        self.try_reserve_locked(&mut admission, charge)?;
        Ok(Reservation {
            heap: self,
            charge,
            active: true,
        })
    }

    /// Removes every entry of `entries` (reference and recorded charge) that `live` rejects, under
    /// one admission lock, and returns the bytes refunded. The collector calls this only while its
    /// quiescent Operation is retained. Stops at the first entry whose object or charge does not
    /// match the table, having removed the ones before it.
    pub(crate) fn sweep(
        &self,
        entries: &[(i32, u64)],
        mut live: impl FnMut(i32) -> bool,
    ) -> AllocResult<u64> {
        let mut admission = self.admission.lock();
        let mut reclaimed = 0u64;
        for &(reference, recorded) in entries {
            if live(reference) {
                continue;
            }
            // One slot lock per freed object: the charge check and the removal together.
            if self.data.remove_charged(reference, recorded).is_none() {
                return Err(AllocError::InvalidLayout);
            }
            admission.budget.reclaim(recorded)?;
            reclaimed = reclaimed
                .checked_add(recorded)
                .ok_or(AllocError::Arithmetic)?;
        }
        Ok(reclaimed)
    }

    /// The collector calls this only while its quiescent Operation is retained.
    /// Never derive a refund from a newly computed layout.
    pub(crate) fn remove_recorded(&self, reference: i32) -> AllocResult<u64> {
        let mut admission = self.admission.lock();
        let charge = self
            .data
            .charge(reference)
            .ok_or(AllocError::InvalidLayout)?;
        admission.budget.reclaim(charge)?;
        self.data.remove(reference);
        Ok(charge)
    }

    pub(crate) fn finish_emergency_bootstrap(&self) -> AllocResult<()> {
        let admission = self.admission.lock();
        if admission.budget.reserved != 0 {
            return Err(AllocError::InvalidLayout);
        }
        let total = admission
            .budget
            .used
            .checked_add(admission.budget.reserved)
            .ok_or(AllocError::Arithmetic)?;
        if total > admission.budget.max {
            Err(AllocError::Capacity)
        } else {
            // Remain enforced. Pressure collection is enabled only after the
            // emergency throwable has been published as a permanent root.
            Ok(())
        }
    }
}

impl Reservation<'_> {
    pub(super) fn commit_intern(
        mut self,
        key: String,
        reference: i32,
    ) -> AllocResult<Option<i32>> {
        use dashmap::mapref::entry::Entry;
        let mut admission = self.heap.admission.lock();
        match self.heap.ref_by_stringvalue.entry(key) {
            Entry::Occupied(mut entry) => {
                // Keep the existing key allocation. Drop refunds this unused reservation.
                Ok(Some(entry.insert(reference)))
            }
            Entry::Vacant(entry) => {
                let recorded = charge::intern(entry.key().capacity())?;
                admission.budget.commit(self.charge, recorded)?;
                entry.insert(reference);
                self.active = false;
                trigger::allocated();
                Ok(None)
            }
        }
    }

    pub(super) fn commit(
        mut self,
        value: HeapValue,
        destination: &mut RootedSlots,
    ) -> AllocResult<i32> {
        let mut admission = self.heap.admission.lock();
        let key = self
            .heap
            .publish(&mut admission, self.charge, value, destination)?;
        self.active = false;
        Ok(key)
    }

    #[cfg(test)]
    pub(crate) fn cancel(mut self) -> AllocResult<()> {
        self.heap.admission.lock().budget.cancel(self.charge)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.heap
                .admission
                .lock()
                .budget
                .cancel(self.charge)
                .expect("a live reservation owns its exact reserved debit");
        }
    }
}
