use super::charge;
use super::java_instance::{JavaInstance, JavaInstanceBase, JavaInstanceClass};
use crate::vm::error::Result;
use crate::vm::gc::budget::{AllocError, AllocResult};
use crate::vm::gc::layout::{InstanceFields, InstanceLayout};
use std::sync::Arc;

/// Shared class metadata only: nothing is allocated for the object until it is built.
pub(crate) struct InstancePlan {
    klass_id: usize,
    layout: Arc<InstanceLayout>,
    mirror: Option<usize>,
}

impl InstancePlan {
    /// `fields` are the class's default field values. They are checked against `layout` and
    /// flattened the first time the layout is used; after that only the layout is consulted.
    pub(crate) fn new(
        klass_id: usize,
        fields: &InstanceFields,
        layout: Arc<InstanceLayout>,
    ) -> Result<Self> {
        layout.defaults(fields)?;
        Ok(Self {
            klass_id,
            layout,
            mirror: None,
        })
    }

    pub(crate) fn mirror(mut self, klass_id: usize) -> Self {
        self.mirror = Some(klass_id);
        self
    }

    pub(super) fn charge(&self) -> AllocResult<u64> {
        charge::instance(self.layout.words())
    }

    pub(super) fn build(self) -> Result<JavaInstance> {
        let base = JavaInstanceBase::with_defaults(self.klass_id, self.layout)?;
        Ok(match self.mirror {
            Some(id) => JavaInstance::Class(JavaInstanceClass::new(base, id)),
            None => JavaInstance::Base(base),
        })
    }
}

/// A single array's implementation limit is independent of -Xmx. This prevents
/// impossible lengths from reaching the host allocator even in accounting-only mode.
pub(crate) const MAX_ARRAY_PAYLOAD: usize = i32::MAX as usize - 8;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ArrayLayout {
    pub(super) bytes: usize,
}

impl ArrayLayout {
    pub(crate) fn new(descriptor: &str, length: i32) -> AllocResult<Self> {
        let length = usize::try_from(length).map_err(|_| AllocError::NegativeLength(length))?;
        let bytes = length
            .checked_mul(Self::width(descriptor))
            .ok_or(AllocError::Arithmetic)?;
        if bytes > MAX_ARRAY_PAYLOAD || bytes > isize::MAX as usize {
            return Err(AllocError::Capacity);
        }
        Ok(Self { bytes })
    }

    pub(crate) fn with_values(descriptor: &str, slots: usize) -> AllocResult<Self> {
        let width = Self::width(descriptor);
        let length = if width == 8 {
            if slots % 2 != 0 {
                return Err(AllocError::InvalidLayout);
            }
            slots / 2
        } else {
            slots
        };
        Self::new(
            descriptor,
            i32::try_from(length).map_err(|_| AllocError::Capacity)?,
        )
    }

    pub(crate) fn width(descriptor: &str) -> usize {
        match descriptor {
            "[B" | "[Z" => 1,
            "[C" | "[S" => 2,
            "[J" | "[D" => 8,
            _ => 4,
        }
    }

    pub(super) fn charge(self, descriptor: &str) -> AllocResult<u64> {
        charge::array(descriptor.len(), self.bytes)
    }
}
