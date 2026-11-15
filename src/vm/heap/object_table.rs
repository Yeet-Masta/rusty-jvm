//! The heap's objects, indexed directly by reference.
//!
//! Heap references are handed out in sequence, so a reference is already a dense index. A table
//! of fixed-size chunks finds an object with two loads and takes a lock on that object alone,
//! where a hash map hashes the key, locks a whole shard and probes a bucket, and scatters
//! neighbouring objects across memory.
//!
//! Each slot has its own reader-writer lock, finer than any shard, so access patterns that were
//! deadlock-free under a sharded map stay so. A chunk is allocated when the first reference in it
//! is issued. Once every reference in a chunk has been issued and every object in it removed, the
//! chunk is retired; it is freed at the start of the next collection, when no thread can still be
//! looking into it.

use crate::vm::gc::budget::{AllocError, AllocResult};
use parking_lot::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, Mutex, RwLock, RwLockReadGuard,
    RwLockWriteGuard,
};
use serde::ser::SerializeMap;
use serde::Serialize;
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const CHUNK_BITS: u32 = 12;
/// Slots per chunk.
const CHUNK: usize = 1 << CHUNK_BITS;
/// Chunks enough for every positive `i32`.
const CHUNKS: usize = 1 << (31 - CHUNK_BITS);

struct Chunk<V> {
    slots: Box<[RwLock<Option<V>>]>,
    /// What each present object was charged against the heap budget, written and read under
    /// its slot's lock.
    charges: Box<[AtomicU64]>,
    /// One bit per slot, set while it holds an object, so a walk over the chunk visits only the
    /// objects present without taking each empty slot's lock.
    present: Box<[AtomicU64]>,
    /// Objects present.
    live: AtomicU32,
    /// Objects ever inserted. A chunk whose every slot has been filled once and emptied again
    /// can never be used again, since references are not reused.
    filled: AtomicU32,
}

impl<V> Chunk<V> {
    fn new() -> Box<Self> {
        Box::new(Self {
            slots: (0..CHUNK).map(|_| RwLock::new(None)).collect(),
            charges: (0..CHUNK).map(|_| AtomicU64::new(0)).collect(),
            present: (0..CHUNK / 64).map(|_| AtomicU64::new(0)).collect(),
            live: AtomicU32::new(0),
            filled: AtomicU32::new(0),
        })
    }
}

pub(crate) struct ObjectTable<V> {
    chunks: Box<[AtomicPtr<Chunk<V>>]>,
    /// One past the highest chunk index ever allocated, bounding iteration.
    high: AtomicUsize,
    /// Chunks emptied for good, waiting for [`Self::free_retired`].
    retired: Mutex<Vec<Box<Chunk<V>>>>,
    /// Retired chunks no thread can still be looking into, kept to be handed out again: every
    /// reference is new, so an allocating program needs a chunk every `CHUNK` objects, and a
    /// recycled one is already built and in memory.
    spare: Mutex<Vec<Box<Chunk<V>>>>,
    /// The table owns `V`s behind raw pointers; this ties its auto traits to theirs.
    _owns: PhantomData<Box<Chunk<V>>>,
}

/// A shared borrow of one object, holding its slot's read lock.
pub(crate) struct Entry<'a, V> {
    key: i32,
    guard: MappedRwLockReadGuard<'a, V>,
}

/// An exclusive borrow of one object, holding its slot's write lock.
pub(crate) struct EntryMut<'a, V> {
    guard: MappedRwLockWriteGuard<'a, V>,
}

impl<'a, V> Entry<'a, V> {
    pub(crate) fn key(&self) -> &i32 {
        &self.key
    }

    pub(crate) fn value(&self) -> &V {
        &self.guard
    }

    /// Narrows the borrow to a part of the object, keeping the lock.
    pub(crate) fn map<T>(entry: Self, f: impl FnOnce(&V) -> &T) -> MappedRwLockReadGuard<'a, T> {
        MappedRwLockReadGuard::map(entry.guard, f)
    }
}

impl<'a, V> EntryMut<'a, V> {
    pub(crate) fn value(&self) -> &V {
        &self.guard
    }

    pub(crate) fn value_mut(&mut self) -> &mut V {
        &mut self.guard
    }

    /// Narrows the borrow to a part of the object, keeping the lock.
    pub(crate) fn map<T>(
        entry: Self,
        f: impl FnOnce(&mut V) -> &mut T,
    ) -> MappedRwLockWriteGuard<'a, T> {
        MappedRwLockWriteGuard::map(entry.guard, f)
    }
}

impl<V> Deref for Entry<'_, V> {
    type Target = V;
    fn deref(&self) -> &V {
        &self.guard
    }
}

impl<V> Deref for EntryMut<'_, V> {
    type Target = V;
    fn deref(&self) -> &V {
        &self.guard
    }
}

impl<V> DerefMut for EntryMut<'_, V> {
    fn deref_mut(&mut self) -> &mut V {
        &mut self.guard
    }
}

impl<V> Default for ObjectTable<V> {
    fn default() -> Self {
        Self {
            chunks: (0..CHUNKS)
                .map(|_| AtomicPtr::new(ptr::null_mut()))
                .collect(),
            high: AtomicUsize::new(0),
            retired: Mutex::new(Vec::new()),
            spare: Mutex::new(Vec::new()),
            _owns: PhantomData,
        }
    }
}

impl<V> ObjectTable<V> {
    /// The chunk and slot index of `key`, or `None` for a key no object can have.
    #[inline]
    fn locate(key: i32) -> Option<(usize, usize)> {
        let index = usize::try_from(key).ok().filter(|index| *index > 0)?;
        Some((index >> CHUNK_BITS, index & (CHUNK - 1)))
    }

    #[inline]
    fn chunk(&self, index: usize) -> Option<&Chunk<V>> {
        let chunk = self.chunks[index].load(Ordering::Acquire);
        // SAFETY: a published chunk stays allocated until `free_retired`, which runs only when no
        // thread can hold a borrow of the table's contents.
        unsafe { chunk.as_ref() }
    }

    /// The chunk at `index`, allocating it if needed.
    fn chunk_or_insert(&self, index: usize) -> &Chunk<V> {
        if let Some(chunk) = self.chunk(index) {
            return chunk;
        }
        let fresh = Box::into_raw(self.spare.lock().pop().unwrap_or_else(Chunk::new));
        let chunk = match self.chunks[index].compare_exchange(
            ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                self.high.fetch_max(index + 1, Ordering::AcqRel);
                fresh
            }
            Err(existing) => {
                // SAFETY: `fresh` was never published.
                self.spare.lock().push(unsafe { Box::from_raw(fresh) });
                existing
            }
        };
        // SAFETY: published above or by the winning thread, and freed only by `free_retired`.
        unsafe { &*chunk }
    }

    #[inline]
    pub(crate) fn get(&self, key: i32) -> Option<Entry<'_, V>> {
        let (chunk, slot) = Self::locate(key)?;
        let guard = self.chunk(chunk)?.slots[slot].read();
        RwLockReadGuard::try_map(guard, Option::as_ref)
            .ok()
            .map(|guard| Entry { key, guard })
    }

    #[inline]
    pub(crate) fn get_mut(&self, key: i32) -> Option<EntryMut<'_, V>> {
        let (chunk, slot) = Self::locate(key)?;
        let guard = self.chunk(chunk)?.slots[slot].write();
        RwLockWriteGuard::try_map(guard, Option::as_mut)
            .ok()
            .map(|guard| EntryMut { guard })
    }

    /// Whether an object is present, from the chunk's presence bits rather than the slot's lock.
    /// The bit is set once the value is stored and cleared as it is taken, both under the slot's
    /// write lock, so a reader racing an insert or removal of the same key sees either outcome.
    pub(crate) fn contains_key(&self, key: i32) -> bool {
        let Some((chunk_index, slot)) = Self::locate(key) else {
            return false;
        };
        self.chunk(chunk_index).is_some_and(|chunk| {
            chunk.present[slot / 64].load(Ordering::Acquire) & (1 << (slot % 64)) != 0
        })
    }

    /// Inserts under a key that must be positive and free, recording the object's charge.
    pub(crate) fn insert_checked(&self, key: i32, value: V, charge: u64) -> AllocResult<()> {
        let (chunk_index, slot) = Self::locate(key).ok_or(AllocError::IdentityExhausted)?;
        let chunk = self.chunk_or_insert(chunk_index);
        let mut guard = chunk.slots[slot].write();
        if guard.is_some() {
            return Err(AllocError::IdentityExhausted);
        }
        *guard = Some(value);
        chunk.charges[slot].store(charge, Ordering::Relaxed);
        chunk.present[slot / 64].fetch_or(1 << (slot % 64), Ordering::Release);
        chunk.live.fetch_add(1, Ordering::Relaxed);
        chunk.filled.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Objects present. Exact only while no thread inserts or removes.
    pub(crate) fn len(&self) -> usize {
        let high = self.high.load(Ordering::Acquire);
        (0..high)
            .filter_map(|index| self.chunk(index))
            .map(|chunk| chunk.live.load(Ordering::Relaxed) as usize)
            .sum()
    }

    /// The charge recorded for a present object.
    pub(crate) fn charge(&self, key: i32) -> Option<u64> {
        let (chunk_index, slot) = Self::locate(key)?;
        let chunk = self.chunk(chunk_index)?;
        let guard = chunk.slots[slot].read();
        guard
            .as_ref()
            .map(|_| chunk.charges[slot].load(Ordering::Relaxed))
    }

    /// Every present object's key and charge, in reference order. Exact only while no thread
    /// inserts or removes, as during a collection.
    pub(crate) fn charges(&self) -> impl Iterator<Item = (i32, u64)> + '_ {
        let high = self.high.load(Ordering::Acquire);
        (0..high)
            .filter_map(move |index| self.chunk(index).map(|chunk| (index, chunk)))
            .flat_map(|(index, chunk)| {
                chunk
                    .present
                    .iter()
                    .enumerate()
                    .flat_map(move |(word, bits)| {
                        let mut bits = bits.load(Ordering::Acquire);
                        std::iter::from_fn(move || {
                            if bits == 0 {
                                return None;
                            }
                            let slot = word * 64 + bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            let key = i32::try_from((index << CHUNK_BITS) | slot).ok()?;
                            Some((key, chunk.charges[slot].load(Ordering::Relaxed)))
                        })
                    })
            })
    }

    /// [`Self::remove`] of an object recorded with `charge`, or `None`, removing nothing, when it is
    /// absent or recorded with another charge. The same callers only: the charge is read before
    /// the slot is locked, which is exact only while nothing inserts.
    pub(crate) fn remove_charged(&self, key: i32, charge: u64) -> Option<V> {
        let (chunk_index, slot) = Self::locate(key)?;
        if self.chunk(chunk_index)?.charges[slot].load(Ordering::Relaxed) != charge {
            return None;
        }
        self.remove(key).map(|(value, _)| value)
    }

    /// Removes an object, returning it and its charge. Call only while no other thread can borrow
    /// from the table, as the collector's sweep does: emptying a chunk for good retires it.
    pub(crate) fn remove(&self, key: i32) -> Option<(V, u64)> {
        let (chunk_index, slot) = Self::locate(key)?;
        let chunk = self.chunk(chunk_index)?;
        let (value, charge) = {
            let mut guard = chunk.slots[slot].write();
            let value = guard.take()?;
            chunk.present[slot / 64].fetch_and(!(1 << (slot % 64)), Ordering::Release);
            (value, chunk.charges[slot].load(Ordering::Relaxed))
        };
        let live = chunk.live.fetch_sub(1, Ordering::Relaxed) - 1;
        if live == 0 && chunk.filled.load(Ordering::Relaxed) as usize >= CHUNK {
            let unpublished = self.chunks[chunk_index].swap(ptr::null_mut(), Ordering::AcqRel);
            if !unpublished.is_null() {
                // SAFETY: unpublished just now, so this is the only owner; it is freed later.
                self.retired
                    .lock()
                    .push(unsafe { Box::from_raw(unpublished) });
            }
        }
        Some((value, charge))
    }

    /// Frees retired chunks. Call only while no other thread can borrow from the table and none
    /// has since the chunks were retired.
    pub(crate) fn free_retired(&self) {
        /// The most spare chunks kept; the rest are freed.
        const SPARE: usize = 16;
        let retired = std::mem::take(&mut *self.retired.lock());
        let mut spare = self.spare.lock();
        for chunk in retired {
            if spare.len() >= SPARE {
                break;
            }
            // Retired only once every slot was filled and emptied again: nothing is present, so
            // it is as new once its fill count starts over.
            chunk.filled.store(0, Ordering::Relaxed);
            spare.push(chunk);
        }
    }

    /// Every object, in reference order. Weakly consistent: objects added or removed meanwhile
    /// may or may not be seen. Each yielded entry holds its object's read lock.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Entry<'_, V>> {
        let high = self.high.load(Ordering::Acquire);
        (0..high)
            .filter_map(move |index| self.chunk(index).map(|chunk| (index, chunk)))
            .flat_map(|(index, chunk)| {
                chunk
                    .slots
                    .iter()
                    .enumerate()
                    .filter_map(move |(slot, lock)| {
                        let guard = RwLockReadGuard::try_map(lock.read(), Option::as_ref).ok()?;
                        let key = i32::try_from((index << CHUNK_BITS) | slot).ok()?;
                        Some(Entry { key, guard })
                    })
            })
    }
}

impl<V> Drop for ObjectTable<V> {
    fn drop(&mut self) {
        for chunk in self.chunks.iter() {
            let chunk = chunk.swap(ptr::null_mut(), Ordering::AcqRel);
            if !chunk.is_null() {
                // SAFETY: the table is being dropped, so nothing borrows from it.
                drop(unsafe { Box::from_raw(chunk) });
            }
        }
    }
}

impl<V> Debug for ObjectTable<V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectTable")
            .field("chunks", &self.high.load(Ordering::Relaxed))
            .finish()
    }
}

impl<V: Serialize> Serialize for ObjectTable<V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entries = self.iter().collect::<Vec<_>>();
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for entry in entries {
            map.serialize_entry(entry.key(), entry.value())?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_insert_get_and_remove() {
        let table = ObjectTable::default();
        table.insert_checked(1, "one", 10).unwrap();
        table.insert_checked(5000, "far", 20).unwrap();
        assert_eq!(*table.get(1).unwrap(), "one");
        assert_eq!(*table.get(5000).unwrap().key(), 5000);
        assert!(table.get(2).is_none());
        *table.get_mut(1).unwrap() = "uno";
        assert_eq!(table.charge(5000), Some(20));
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.charges().collect::<Vec<_>>(),
            vec![(1, 10), (5000, 20)]
        );
        assert_eq!(table.remove(1), Some(("uno", 10)));
        assert!(!table.contains_key(1));
        assert_eq!(table.charges().collect::<Vec<_>>(), vec![(5000, 20)]);
        assert_eq!(table.remove(1), None);
    }

    #[test]
    fn should_reject_occupied_and_nonpositive_keys() {
        let table = ObjectTable::default();
        table.insert_checked(3, 3, 0).unwrap();
        for key in [3, 0, -1] {
            assert!(matches!(
                table.insert_checked(key, 4, 0),
                Err(AllocError::IdentityExhausted)
            ));
        }
        assert!(table.get(0).is_none() && table.get(i32::MIN).is_none());
        table.insert_checked(i32::MAX, 7, 0).unwrap();
        assert_eq!(*table.get(i32::MAX).unwrap(), 7);
    }

    #[test]
    fn should_iterate_in_reference_order() {
        let table = ObjectTable::default();
        for key in [9000, 2, 7] {
            table.insert_checked(key, key * 10, 0).unwrap();
        }
        let seen: Vec<_> = table.iter().map(|entry| (*entry.key(), *entry)).collect();
        assert_eq!(seen, vec![(2, 20), (7, 70), (9000, 90000)]);
    }

    #[test]
    fn should_retire_a_chunk_emptied_for_good() {
        let table = ObjectTable::default();
        let first = CHUNK as i32;
        for key in first..first * 2 {
            table.insert_checked(key, key, 0).unwrap();
        }
        for key in first..first * 2 - 1 {
            table.remove(key);
        }
        assert!(table.retired.lock().is_empty());
        table.remove(first * 2 - 1);
        assert_eq!(table.retired.lock().len(), 1);
        assert!(table.get(first).is_none());
        table.free_retired();
        assert!(table.retired.lock().is_empty());
        // A key in a freed chunk can be inserted again; the chunk is allocated afresh.
        table.insert_checked(first, 1, 0).unwrap();
        assert_eq!(*table.get(first).unwrap(), 1);
    }
}
