//! Fixed, descriptor-derived schemas. Neither the schema nor its keys have a mutable API.
use crate::vm::error::{Error, Result};
use crate::vm::gc::budget::AllocError;
use crate::vm::gc::references::{self, Kind};
use crate::vm::heap::java_instance::{ClassName, FieldNameType};
use crate::vm::helper::{get_length, is_reference};
use crate::vm::method_area::field::{FieldInfo, FieldValue};
use indexmap::IndexMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

/// A class's instance fields with their default values, grouped by declaring class, root class
/// first. This is class metadata: an instance stores only the values, as words in this order.
pub(crate) type InstanceFields = IndexMap<ClassName, IndexMap<FieldNameType, FieldValue>>;

enum FieldKind {
    Value,
    Strong,
    Referent,
}

#[derive(Debug)]
struct FieldLayout {
    name: String,
    width: usize,
    /// Index of the field's first word in an instance.
    offset: usize,
}

#[derive(Debug)]
struct SegmentLayout {
    declaring_class: String,
    fields: Box<[FieldLayout]>,
}

/// Shared by instances, including clones. Private boxed slices, no interior mutability
/// beyond a write-once cache, and no mutable accessors make publication irreversible.
#[derive(Debug)]
pub(crate) struct InstanceLayout {
    segments: Box<[SegmentLayout]>,
    reference_kind: Kind,
    /// Words an instance stores: one per field, two for a `long` or `double`.
    words: usize,
    /// Word offsets of the strong reference fields, in declaration order.
    strong: Box<[u32]>,
    /// Word offset of `java.lang.ref.Reference.referent`, when the class has it.
    referent: Option<u32>,
    /// The words of a new instance, taken once from the class's default field values.
    defaults: OnceLock<Box<[i32]>>,
}

/// The collector must process the distinguished referent separately from strong closure.
///
/// An owned edge set for tests; the collector visits edges in place instead.
#[cfg(test)]
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct References {
    pub(crate) strong: Vec<i32>,
    pub(crate) referent: Option<i32>,
}

impl InstanceLayout {
    /// Called before constructing an instance, never by the scanner. Supplying metadata
    /// explicitly also lets tests exercise the real schema without loading Java classes.
    pub(crate) fn new(
        segments: impl IntoIterator<Item = (String, Vec<Arc<FieldInfo>>)>,
    ) -> Result<Self> {
        let mut layout = Vec::new();
        let mut reference_kind = Kind::Other;
        let mut words = 0usize;
        let mut strong = Vec::new();
        let mut referent = None;
        for (declaring_class, fields) in segments {
            match declaring_class.as_str() {
                references::REFERENCE => references::validate_fields(&fields)?,
                "java/lang/ref/SoftReference" => reference_kind = Kind::Soft,
                "java/lang/ref/WeakReference" => reference_kind = Kind::Weak,
                "java/lang/ref/PhantomReference" => reference_kind = Kind::Phantom,
                "java/lang/ref/FinalReference" => reference_kind = Kind::Final,
                _ => {}
            }
            let mut slots = Vec::new();
            for field in fields {
                if field.declaring_class() != declaring_class {
                    return Err(Error::new_execution(
                        "Layout field has the wrong declaring class",
                    ));
                }
                let descriptor = field.type_descriptor();
                let kind = if is_reference(descriptor) {
                    if declaring_class == "java/lang/ref/Reference" && field.name() == "referent" {
                        FieldKind::Referent
                    } else {
                        FieldKind::Strong
                    }
                } else {
                    FieldKind::Value
                };
                let width = usize::try_from(get_length(descriptor)?)?;
                let offset = u32::try_from(words).map_err(|_| AllocError::Capacity)?;
                match kind {
                    FieldKind::Value => {}
                    FieldKind::Strong => strong.push(offset),
                    FieldKind::Referent => referent = Some(offset),
                }
                slots.push(FieldLayout {
                    name: field.name().to_string(),
                    width,
                    offset: words,
                });
                words = words.checked_add(width).ok_or(AllocError::Arithmetic)?;
            }
            layout.push(SegmentLayout {
                declaring_class,
                fields: slots.into_boxed_slice(),
            });
        }
        Ok(Self {
            segments: layout.into_boxed_slice(),
            reference_kind,
            words,
            strong: strong.into_boxed_slice(),
            referent,
            defaults: OnceLock::new(),
        })
    }

    /// Reject incomplete or mismatched schemas before a heap entry can be published.
    pub(crate) fn validate(&self, fields: &InstanceFields) -> Result<()> {
        if self.segments.len() != fields.len() {
            return Err(Error::new_execution(
                "Instance layout has missing class segments",
            ));
        }
        for (segment, (class, values)) in self.segments.iter().zip(fields) {
            if segment.declaring_class != *class || segment.fields.len() != values.len() {
                return Err(Error::new_execution(
                    "Instance layout has mismatched fields",
                ));
            }
            for (slot, (name, value)) in segment.fields.iter().zip(values) {
                if slot.name != *name || value.storage_capacity(true)? != slot.width {
                    return Err(Error::new_execution(
                        "Instance layout has a mismatched field slot",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Words an instance of this layout stores.
    pub(crate) fn words(&self) -> usize {
        self.words
    }

    /// The values of `fields`, which must match this layout, as an instance stores them.
    pub(crate) fn flatten(&self, fields: &InstanceFields) -> Result<Box<[i32]>> {
        self.validate(fields)?;
        let mut image = Vec::new();
        image
            .try_reserve_exact(self.words)
            .map_err(|_| AllocError::HostAllocation)?;
        for values in fields.values() {
            for value in values.values() {
                image.extend(value.raw_value()?);
            }
        }
        if image.len() != self.words {
            return Err(AllocError::InvalidLayout.into());
        }
        Ok(image.into_boxed_slice())
    }

    /// The words of a new instance: the class's default values, flattened on first use.
    pub(crate) fn defaults(&self, fields: &InstanceFields) -> Result<&[i32]> {
        if let Some(image) = self.defaults.get() {
            return Ok(image);
        }
        let image = self.flatten(fields)?;
        Ok(self.defaults.get_or_init(|| image))
    }

    /// [`Self::defaults`] once it has been computed.
    pub(crate) fn cached_defaults(&self) -> Option<&[i32]> {
        self.defaults.get().map(|image| &**image)
    }

    /// Where the field `field_name` is as seen from `class_name`: declared by that class or,
    /// failing that, by its nearest ancestor that has one. Returns the offset of its first word
    /// and its width in words.
    pub(crate) fn locate(&self, class_name: &str, field_name: &str) -> Option<(usize, usize)> {
        let start = self
            .segments
            .iter()
            .position(|segment| segment.declaring_class == class_name)?;
        self.segments[..=start].iter().rev().find_map(|segment| {
            segment
                .fields
                .iter()
                .find(|field| field.name == field_name)
                .map(|field| (field.offset, field.width))
        })
    }

    /// Each declaring class, root first, with the `(offset, width)` of each of its fields.
    pub(crate) fn segments(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&str, impl Iterator<Item = (usize, usize)> + '_)> + '_
    {
        self.segments.iter().map(|segment| {
            (
                segment.declaring_class.as_str(),
                segment
                    .fields
                    .iter()
                    .map(|field| (field.offset, field.width)),
            )
        })
    }

    /// The field names of each declaring class, in layout order. For diagnostics.
    pub(crate) fn names(&self) -> impl Iterator<Item = (&str, impl Iterator<Item = &str> + '_)> {
        self.segments.iter().map(|segment| {
            (
                segment.declaring_class.as_str(),
                segment.fields.iter().map(|field| field.name.as_str()),
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn references(&self, slots: &[AtomicI32]) -> Result<References> {
        let mut references = References::default();
        references.referent = self.visit_references(slots, |reference| {
            references.strong.push(reference);
            Ok(())
        })?;
        Ok(references)
    }

    pub(crate) fn reference_kind(&self) -> Kind {
        self.reference_kind
    }

    /// The collector supplies pre-reserved storage. The visitor only copies/deduplicates
    /// IDs, and must not acquire another heap guard or reserve managed storage.
    pub(crate) fn visit_references(
        &self,
        slots: &[AtomicI32],
        mut visit: impl FnMut(i32) -> Result<()>,
    ) -> Result<Option<i32>> {
        if slots.len() != self.words {
            return Err(AllocError::InvalidLayout.into());
        }
        // The offsets were fixed with the schema; no class or field name is consulted, so an
        // inherited field cannot be confused with one that shadows its bare name.
        for &offset in self.strong.iter() {
            let reference = slots[offset as usize].load(Ordering::Relaxed);
            if reference != 0 {
                visit(reference)?;
            }
        }
        Ok(self
            .referent
            .map(|offset| slots[offset as usize].load(Ordering::Relaxed)))
    }
}

#[cfg(test)]
mod tests;
