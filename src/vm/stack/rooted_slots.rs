use crate::vm::gc::budget::{AllocError, AllocResult};
use crate::vm::stack::slot::Slot;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::{Rc, Weak};

type Published = Rc<RefCell<Vec<Slot>>>;

thread_local! {
    static TRANSFERS: RefCell<Vec<Weak<RefCell<Vec<Slot>>>>> = const { RefCell::new(Vec::new()) };
    /// Destinations not in use: emptied, so they root nothing, but still registered and still
    /// owning their storage. Every call and every allocation needs a destination for a moment;
    /// taking one from here costs no allocation and no change to `TRANSFERS`.
    static IDLE: RefCell<Vec<(Vec<Slot>, Published)>> = const { RefCell::new(Vec::new()) };
}

/// Idle destinations kept per thread; beyond this they are released as they are dropped.
const MAX_IDLE: usize = 32;

/// An owned, published destination for an argument, result or exception transfer.
///
/// Unlike a temporary LIFO scope, a transfer can outlive the Rust call that fills it.
/// Create the destination before releasing the source frame, fill it without a
/// safepoint, and keep it until the receiving frame, field or pending slot is set.
/// The Rc makes both the value and its registration thread-bound.
#[derive(Debug)]
pub(crate) struct RootedSlots {
    slots: Vec<Slot>,
    published: Rc<RefCell<Vec<Slot>>>,
}

impl RootedSlots {
    pub(crate) fn new() -> Self {
        let idle = IDLE.try_with(|idle| idle.borrow_mut().pop()).ok().flatten();
        if let Some((slots, published)) = idle {
            return Self { slots, published };
        }
        let published = Rc::new(RefCell::new(Vec::new()));
        TRANSFERS.with(|roots| roots.borrow_mut().push(Rc::downgrade(&published)));
        Self {
            slots: Vec::new(),
            published,
        }
    }

    pub(crate) fn from_slots(slots: Vec<Slot>) -> Self {
        let mut destination = Self::new();
        destination.replace(slots);
        destination
    }

    pub(crate) fn replace(&mut self, slots: Vec<Slot>) {
        self.published.borrow_mut().clone_from(&slots);
        self.slots = slots;
    }

    /// [`Self::replace`] from a slice, into storage this destination already owns.
    pub(crate) fn assign(&mut self, slots: &[Slot]) {
        let mut published = self.published.borrow_mut();
        published.clear();
        published.extend_from_slice(slots);
        self.slots.clear();
        self.slots.extend_from_slice(slots);
    }

    /// Takes `count` slots from `next`, which yields them last to first (as popping an operand
    /// stack does), and holds them in their original order.
    pub(crate) fn assign_popped(&mut self, count: usize, mut next: impl FnMut() -> Slot) {
        self.slots.clear();
        self.slots.extend((0..count).map(|_| next()));
        self.slots.reverse();
        let mut published = self.published.borrow_mut();
        published.clear();
        published.extend_from_slice(&self.slots);
    }

    /// Holds nothing from here on.
    pub(crate) fn clear(&mut self) {
        self.published.borrow_mut().clear();
        self.slots.clear();
    }

    /// Prepare before admission so commit only writes already-owned storage.
    pub(crate) fn prepare_single_ref(&mut self) -> AllocResult<()> {
        if self.slots.is_empty() {
            self.slots
                .try_reserve_exact(1)
                .map_err(|_| AllocError::HostAllocation)?;
        }
        let mut published = self.published.borrow_mut();
        if published.is_empty() {
            published
                .try_reserve_exact(1)
                .map_err(|_| AllocError::HostAllocation)?;
        }
        Ok(())
    }

    pub(crate) fn publish_single_ref(&mut self, reference: i32) {
        let mut published = self.published.borrow_mut();
        assert!(self.slots.capacity() >= 1 && published.capacity() >= 1);
        self.slots.clear();
        published.clear();
        self.slots.push(Slot::Ref(reference));
        published.push(Slot::Ref(reference));
    }

    /// Raw bits are only for a consumer that already knows the type (heap or JNI).
    pub(crate) fn raw(&self) -> Vec<i32> {
        self.slots.iter().map(|slot| slot.value()).collect()
    }

    pub(crate) fn root_refs() -> Vec<i32> {
        TRANSFERS.with(|roots| {
            roots
                .borrow()
                .iter()
                .filter_map(Weak::upgrade)
                .flat_map(|slots| {
                    slots
                        .borrow()
                        .iter()
                        .filter(|slot| slot.is_ref())
                        .map(|slot| slot.value())
                        .collect::<Vec<_>>()
                })
                .collect()
        })
    }
}

impl Deref for RootedSlots {
    type Target = [Slot];

    fn deref(&self) -> &Self::Target {
        &self.slots
    }
}

impl Drop for RootedSlots {
    fn drop(&mut self) {
        self.clear();
        let kept = IDLE.try_with(|idle| {
            let mut idle = idle.borrow_mut();
            if idle.len() >= MAX_IDLE {
                return false;
            }
            idle.push((std::mem::take(&mut self.slots), Rc::clone(&self.published)));
            true
        });
        if kept != Ok(true) {
            let registration = Rc::downgrade(&self.published);
            let _ = TRANSFERS.try_with(|roots| {
                roots
                    .borrow_mut()
                    .retain(|root| !root.ptr_eq(&registration));
            });
        }
    }
}
