use crate::vm::jni::handles::{
    self, HandleError, HandleRecord, HandleResult, HandleTable, ReferenceKind,
};
use crate::vm::jni::java_thread::JavaThread;
use jni_sys::jobject;
use std::marker::PhantomData;
use std::rc::Rc;
use std::thread;

/// HotSpot's default `MaxJNILocalCapacity`: the largest capacity `EnsureLocalCapacity` and
/// `PushLocalFrame` accept. Like HotSpot's, a frame still grows past it on demand: it bounds
/// requests, not the number of local references native code may create.
pub(crate) const MAX_LOCAL_CAPACITY: usize = 65_536;

struct LocalFrame {
    id: usize,
    native: bool,
    /// Each local reference's token, with the heap reference it denotes. A local reference's
    /// target never changes, so the frames themselves are the thread's local roots: no root
    /// snapshot needs the global handle table for them.
    locals: Vec<(usize, i32)>,
}

#[derive(Default)]
pub(crate) struct LocalFrames {
    frames: Vec<LocalFrame>,
    next_frame: usize,
}

impl LocalFrames {
    fn reserve(frame: &mut LocalFrame, count: usize, table: &mut HandleTable) -> HandleResult<()> {
        table.reserve(count)?;
        frame
            .locals
            .try_reserve(count)
            .map_err(|_| HandleError::OutOfMemory)
    }

    /// Reserves room for `count` more local references in the current frame; the caller has
    /// checked `count` against [`MAX_LOCAL_CAPACITY`].
    pub(super) fn ensure_capacity(&mut self, count: usize) -> HandleResult<()> {
        let frame = self.frames.last_mut().ok_or(HandleError::MissingFrame)?;
        Self::reserve(frame, count, &mut handles::table())
    }

    pub(super) fn push(&mut self, capacity: usize, native: bool) -> HandleResult<usize> {
        // All checks and storage reservations precede publication of a new frame.
        let id = self
            .next_frame
            .checked_add(1)
            .ok_or(HandleError::OutOfMemory)?;
        let mut frame = LocalFrame {
            id,
            native,
            locals: Vec::new(),
        };
        Self::reserve(&mut frame, capacity, &mut handles::table())?;
        self.frames
            .try_reserve(1)
            .map_err(|_| HandleError::OutOfMemory)?;
        self.frames.push(frame);
        self.next_frame = id;
        Ok(id)
    }

    fn insert(
        frame: &mut LocalFrame,
        target: i32,
        table: &mut HandleTable,
    ) -> HandleResult<usize> {
        Self::reserve(frame, 1, table)?;
        let token = table.insert(HandleRecord {
            kind: ReferenceKind::Local,
            target,
            owner_thread: Some(thread::current().id()),
            frame: Some(frame.id),
        })?;
        frame.locals.push((token, target));
        Ok(token)
    }

    pub(super) fn new_local(
        &mut self,
        target: i32,
        table: &mut HandleTable,
    ) -> HandleResult<usize> {
        Self::insert(
            self.frames.last_mut().ok_or(HandleError::MissingFrame)?,
            target,
            table,
        )
    }

    pub(super) fn delete_local(&mut self, id: usize, token: usize) {
        if let Some(frame) = self.frames.iter_mut().find(|frame| frame.id == id) {
            frame.locals.retain(|&(entry, _)| entry != token);
        }
    }

    pub(super) fn pop(&mut self, object: jobject) -> HandleResult<usize> {
        let child = self.frames.last().ok_or(HandleError::MissingFrame)?;
        if child.native || self.frames.len() < 2 {
            return Err(HandleError::MissingFrame);
        }
        let mut table = handles::table();
        let target = table.resolve(object as usize)?;
        let parent = self.frames.len() - 2;
        // Destination first. A failure leaves the child and all its records intact.
        let result = if target == 0 {
            0
        } else {
            Self::insert(&mut self.frames[parent], target, &mut table)?
        };
        if let Some(child) = self.frames.pop() {
            for (token, _) in child.locals {
                table.records.remove(&token);
            }
        }
        Ok(result)
    }

    pub(super) fn native_frame(&self) -> HandleResult<usize> {
        self.frames
            .iter()
            .rev()
            .find(|frame| frame.native)
            .map(|frame| frame.id)
            .ok_or(HandleError::MissingFrame)
    }

    /// The targets of every local reference of this thread: its JNI local roots.
    pub(crate) fn local_targets(&self) -> impl Iterator<Item = i32> + '_ {
        self.frames
            .iter()
            .flat_map(|frame| frame.locals.iter().map(|&(_, target)| target))
    }

    fn discard_from(&mut self, id: usize) {
        let mut table = handles::table();
        while self.frames.last().is_some_and(|frame| frame.id >= id) {
            if let Some(frame) = self.frames.pop() {
                for (token, _) in frame.locals {
                    table.records.remove(&token);
                }
            }
        }
    }
}

impl Drop for LocalFrames {
    fn drop(&mut self) {
        self.discard_from(0);
    }
}

/// Owns the base frame and any unbalanced PushLocalFrame calls in this native invocation.
pub(crate) struct NativeFrame {
    id: usize,
    _thread_bound: PhantomData<Rc<()>>,
}

impl NativeFrame {
    pub(crate) fn enter() -> Option<Self> {
        let result = JavaThread::with_jni_frames(|frames| frames.push(16, true));
        match result {
            Ok(id) => Some(Self {
                id,
                _thread_bound: PhantomData,
            }),
            Err(error) => {
                error.set_pending();
                None
            }
        }
    }
}

impl Drop for NativeFrame {
    fn drop(&mut self) {
        JavaThread::with_jni_frames(|frames| frames.discard_from(self.id));
        JavaThread::with_jni_leases(|leases| leases.retain(|lease| lease.frame < self.id));
    }
}
