use crate::vm::error::{Error, Result};
use crate::vm::gc::budget::{copy_slice, copy_string, AllocError, AllocResult};
#[cfg(test)]
use crate::vm::gc::layout::InstanceFields;
use crate::vm::gc::layout::InstanceLayout;
#[cfg(test)]
use crate::vm::gc::layout::References;
use crate::vm::gc::references::{Reference, REFERENCE};
use crate::vm::heap::charge;
use crate::vm::heap::construction::ArrayLayout;
use crate::vm::heap::java_instance::JavaInstance::{Base, Class};
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use derive_new::new;
use getset::CopyGetters;
use indexmap::IndexMap;
use serde::Serialize;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, OnceLock};

pub type ClassName = String;
pub type FieldNameType = String;

/// An object's state: its class and its field values.
///
/// The values are words in the order of the class's [`InstanceLayout`], one per field and two
/// for a `long` or `double` (high word first). Which word belongs to which field is class
/// metadata, so an object owns a single allocation however deep its class hierarchy is.
///
/// The words are atomics so that the collector can update reference links through a shared
/// borrow. They add no synchronization of their own: readers hold the heap entry's shared lock
/// and writers its exclusive one.
#[derive(Debug)]
pub(crate) struct JavaInstanceBase {
    klass_id: usize,
    slots: Box<[AtomicI32]>,
    layout: Arc<InstanceLayout>,
}

impl JavaInstanceBase {
    /// An instance holding the values of `fields`, which must match `layout`.
    #[cfg(test)]
    pub(crate) fn new(
        klass_id: usize,
        fields: InstanceFields,
        layout: Arc<InstanceLayout>,
    ) -> Result<Self> {
        let image = layout.flatten(&fields)?;
        Self::from_words(klass_id, image.iter().copied(), image.len(), layout)
    }

    /// A new instance with the class's default field values, which the layout must have seen.
    pub(crate) fn with_defaults(klass_id: usize, layout: Arc<InstanceLayout>) -> Result<Self> {
        let image = layout.cached_defaults().ok_or(AllocError::InvalidLayout)?;
        let slots = Self::slots(image.iter().copied(), image.len())?;
        Ok(Self {
            klass_id,
            slots,
            layout,
        })
    }

    fn from_words(
        klass_id: usize,
        words: impl Iterator<Item = i32>,
        count: usize,
        layout: Arc<InstanceLayout>,
    ) -> Result<Self> {
        if count != layout.words() {
            return Err(AllocError::InvalidLayout.into());
        }
        Ok(Self {
            klass_id,
            slots: Self::slots(words, count)?,
            layout,
        })
    }

    fn slots(words: impl Iterator<Item = i32>, count: usize) -> AllocResult<Box<[AtomicI32]>> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(count)
            .map_err(|_| AllocError::HostAllocation)?;
        slots.extend(words.map(AtomicI32::new));
        Ok(slots.into_boxed_slice())
    }

    fn try_clone(&self) -> Result<Self> {
        Self::from_words(
            self.klass_id,
            self.slots.iter().map(|slot| slot.load(Ordering::Relaxed)),
            self.slots.len(),
            Arc::clone(&self.layout),
        )
    }

    fn word(&self, offset: usize) -> Option<i32> {
        Some(self.slots.get(offset)?.load(Ordering::Relaxed))
    }

    /// The words of the field at `offset`, the second one zero unless the field is wide.
    fn read(&self, offset: usize, width: usize) -> Option<[i32; 2]> {
        let first = self.word(offset)?;
        Some(if width == 2 {
            [first, self.word(offset + 1)?]
        } else {
            [first, 0]
        })
    }

    fn write(&self, offset: usize, width: usize, words: [i32; 2]) -> Option<()> {
        self.slots.get(offset)?.store(words[0], Ordering::Relaxed);
        if width == 2 {
            self.slots
                .get(offset + 1)?
                .store(words[1], Ordering::Relaxed);
        }
        Some(())
    }

    /// Where a `getfield`/`putfield` site's field is, from the site's cache when it has been
    /// there before.
    fn position(
        &self,
        class_name: &str,
        field_name: &str,
        position: &OnceLock<(u32, u32)>,
    ) -> Option<(usize, usize)> {
        if let Some(&(offset, width)) = position.get() {
            return Some((offset as usize, width as usize));
        }
        let (offset, width) = self.layout.locate(class_name, field_name)?;
        let _ = position.set((offset as u32, width as u32));
        Some((offset, width))
    }
}

impl Clone for JavaInstanceBase {
    fn clone(&self) -> Self {
        Self {
            klass_id: self.klass_id,
            slots: self
                .slots
                .iter()
                .map(|slot| AtomicI32::new(slot.load(Ordering::Relaxed)))
                .collect(),
            layout: Arc::clone(&self.layout),
        }
    }
}

/// Field values keyed by declaring class and field name, for the diagnostic JSON heap dump.
impl Serialize for JavaInstanceBase {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields: IndexMap<&str, IndexMap<&str, Vec<i32>>> = IndexMap::new();
        let names = self.layout.names();
        for ((class, names), (_, positions)) in names.zip(self.layout.segments()) {
            let values = names
                .zip(positions)
                .map(|(name, (offset, width))| {
                    let words = (offset..offset + width)
                        .filter_map(|at| self.word(at))
                        .collect();
                    (name, words)
                })
                .collect();
            fields.insert(class, values);
        }
        let mut state = serializer.serialize_struct("JavaInstanceBase", 2)?;
        state.serialize_field("klass_id", &self.klass_id)?;
        state.serialize_field("fields", &fields)?;
        state.end()
    }
}

#[derive(Debug, Serialize, Clone, new, CopyGetters)]
pub(crate) struct JavaInstanceClass {
    instance: JavaInstanceBase,
    #[getset(get_copy = "pub")]
    mirror_klass_id: usize,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) enum JavaInstance {
    Base(JavaInstanceBase),
    Class(JavaInstanceClass),
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Array {
    type_name: String,
    data: Vec<u8>,
    /// Registry id of the last class whose instance an `aastore` type check accepted into this
    /// array. Element types never change, so such instances are accepted again without the check.
    store_klass: Option<usize>,
}

impl Array {
    #[cfg(test)]
    pub(crate) fn references(&self) -> Result<References> {
        let mut references = References::default();
        self.visit_references(|reference| {
            references.strong.push(reference);
            Ok(())
        })?;
        Ok(references)
    }

    pub(crate) fn visit_references(&self, mut visit: impl FnMut(i32) -> Result<()>) -> Result<()> {
        // Descriptors were planned before publication. Do not parse into an owned
        // TypeDescriptor while holding a heap shard (object descriptors allocate).
        let component = self.type_name.trim_start_matches('[');
        let dimensions = self.type_name.len() - component.len();
        let valid_component = matches!(component, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z")
            || (component.starts_with('L') && component.ends_with(';') && component.len() > 2);
        if dimensions == 0 || dimensions > 255 || !valid_component {
            return Err(AllocError::InvalidLayout.into());
        }
        // [I and [[I have identical element widths, but only the latter holds refs.
        let references = self.type_name.starts_with("[[") || self.type_name.starts_with("[L");
        if !references {
            return Ok(());
        }
        let chunks = self.data.chunks_exact(4);
        if !chunks.remainder().is_empty() {
            return Err(AllocError::InvalidLayout.into());
        }
        for chunk in chunks {
            let reference = i32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if reference != 0 {
                visit(reference)?;
            }
        }
        Ok(())
    }

    pub fn new(type_name: &str, len: i32) -> AllocResult<Self> {
        Self::from_layout(type_name, ArrayLayout::new(type_name, len)?)
    }

    pub(super) fn from_layout(type_name: &str, layout: ArrayLayout) -> AllocResult<Self> {
        let mut data = Vec::new();
        data.try_reserve_exact(layout.bytes)
            .map_err(|_| AllocError::HostAllocation)?;
        data.resize(layout.bytes, 0);
        Ok(Self {
            type_name: copy_string(type_name)?,
            data,
            store_klass: None,
        })
    }

    pub(crate) fn try_clone(&self) -> AllocResult<Self> {
        Ok(Self {
            type_name: copy_string(&self.type_name)?,
            data: copy_slice(&self.data)?,
            store_klass: self.store_klass,
        })
    }

    fn bytes_size(type_name: &str) -> usize {
        ArrayLayout::width(type_name)
    }

    /// Wide values are pairs of JVM slots, high word then low word, just like set_value.
    pub fn new_with_values(type_name: &str, array: &[i32]) -> AllocResult<Self> {
        let size = Self::bytes_size(type_name);
        let mut result =
            Self::from_layout(type_name, ArrayLayout::with_values(type_name, array.len())?)?;
        if size == 8 {
            for (dst, words) in result.data.chunks_exact_mut(8).zip(array.chunks_exact(2)) {
                let (first, second) = if cfg!(target_endian = "big") {
                    (words[0], words[1])
                } else {
                    (words[1], words[0])
                };
                dst[..4].copy_from_slice(&first.to_ne_bytes());
                dst[4..].copy_from_slice(&second.to_ne_bytes());
            }
        } else {
            for (dst, value) in result.data.chunks_exact_mut(size).zip(array) {
                let bytes = value.to_ne_bytes();
                let src = if cfg!(target_endian = "big") {
                    &bytes[4 - size..]
                } else {
                    &bytes[..size]
                };
                dst.copy_from_slice(src);
            }
        }
        Ok(result)
    }

    /// Bytes per element, from the second character of a one-dimensional primitive array's name.
    /// `count` elements from `start` as raw bytes, or `None` past the end.
    pub(crate) fn element_bytes(&self, start: usize, count: usize) -> Option<&[u8]> {
        let width = self.element_width();
        self.data
            .get(start.checked_mul(width)?..start.checked_add(count)?.checked_mul(width)?)
    }

    /// Overwrites elements from `start` with raw `bytes` taken from an array of the same element
    /// width, or returns `None`, having written nothing, past the end.
    pub(crate) fn write_element_bytes(&mut self, start: usize, bytes: &[u8]) -> Option<()> {
        let from = start.checked_mul(self.element_width())?;
        self.data
            .get_mut(from..from.checked_add(bytes.len())?)?
            .copy_from_slice(bytes);
        Some(())
    }

    /// Moves `count` elements from `from` to `to` within this array, as `memmove` does.
    pub(crate) fn copy_elements_within(
        &mut self,
        from: usize,
        to: usize,
        count: usize,
    ) -> Option<()> {
        let width = self.element_width();
        let bytes = count.checked_mul(width)?;
        let (source, target) = (from.checked_mul(width)?, to.checked_mul(width)?);
        if source.checked_add(bytes)? > self.data.len()
            || target.checked_add(bytes)? > self.data.len()
        {
            return None;
        }
        self.data.copy_within(source..source + bytes, target);
        Some(())
    }

    fn element_width(&self) -> usize {
        match self.type_name.as_bytes() {
            [_, b'B' | b'Z'] => 1,
            [_, b'C' | b'S'] => 2,
            [_, b'J' | b'D'] => 8,
            _ => 4,
        }
    }

    /// The element at `index` as JVM words (a wide value is high word, then low word), or the
    /// array's length when `index` is outside it.
    pub(crate) fn load(&self, index: i32) -> std::result::Result<[i32; 2], i32> {
        let width = self.element_width();
        let length = self.data.len() / width;
        if index < 0 || index as usize >= length {
            return Err(length as i32);
        }
        let at = index as usize * width;
        let source = &self.data[at..at + width];
        Ok(match width {
            1 => [source[0] as i8 as i32, 0],
            2 => {
                let bytes = [source[0], source[1]];
                if self.type_name.as_bytes()[1] == b'C' {
                    [u16::from_ne_bytes(bytes) as i32, 0]
                } else {
                    [i16::from_ne_bytes(bytes) as i32, 0]
                }
            }
            4 => [
                i32::from_ne_bytes([source[0], source[1], source[2], source[3]]),
                0,
            ],
            _ => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(source);
                let value = i64::from_ne_bytes(bytes);
                [(value >> 32) as i32, value as i32]
            }
        })
    }

    /// Stores JVM words (as [`Self::load`] returns them) at `index`, or returns the array's
    /// length when `index` is outside it.
    /// Whether an `aastore` of an instance of class `value_klass` (`None` for `null`) needs no type
    /// check: `null`, anything into an `Object[]`, or the class last accepted.
    #[inline]
    pub(crate) fn admits_without_check(&self, value_klass: Option<usize>) -> bool {
        value_klass.is_none()
            || self.store_klass == value_klass
            || self.type_name == "[Ljava/lang/Object;"
    }

    /// Records that the type check accepted an instance of `klass_id`.
    pub(crate) fn note_accepted(&mut self, klass_id: usize) {
        self.store_klass = Some(klass_id);
    }

    pub(crate) fn store(&mut self, index: i32, words: [i32; 2]) -> std::result::Result<(), i32> {
        let width = self.element_width();
        let length = self.data.len() / width;
        if index < 0 || index as usize >= length {
            return Err(length as i32);
        }
        let at = index as usize * width;
        let target = &mut self.data[at..at + width];
        if width == 8 {
            let value = (i64::from(words[0]) << 32) | i64::from(words[1] as u32);
            target.copy_from_slice(&value.to_ne_bytes());
        } else {
            let bytes = words[0].to_ne_bytes();
            target.copy_from_slice(if cfg!(target_endian = "big") {
                &bytes[4 - width..]
            } else {
                &bytes[..width]
            });
        }
        Ok(())
    }

    pub fn get_value(&self, index: i32) -> Result<Vec<i32>> {
        let size = Self::bytes_size(&self.type_name);
        let offset = index as usize * size;

        self.get_value_by_raw_offset(offset, size)
    }

    pub fn get_value_by_raw_offset(&self, offset: usize, size: usize) -> Result<Vec<i32>> {
        let src = self.data.get(offset..offset + size)
            .ok_or_else(|| Error::new_execution(&format!(
                "get_value_by_raw_offset: offset out of bounds: offset={offset}, size={size}, data_len={}",
                self.data.len()
            )))?;

        match size {
            1 => Ok(vec![src[0] as i8 as i32]),
            2 => {
                let bytes: [u8; 2] = src.try_into()?;
                let value = if self.type_name == "[C" {
                    u16::from_ne_bytes(bytes) as i32
                } else {
                    i16::from_ne_bytes(bytes) as i32
                };
                Ok(vec![value])
            }
            4 => {
                let bytes: [u8; 4] = src.try_into()?;
                Ok(vec![i32::from_ne_bytes(bytes)])
            }
            8 => {
                let bytes: [u8; 8] = src.try_into()?;
                let (high, low) = bytes.split_at(4);
                let high = i32::from_ne_bytes(high.try_into()?);
                let low = i32::from_ne_bytes(low.try_into()?);
                if cfg!(target_endian = "big") {
                    Ok(vec![high, low])
                } else {
                    Ok(vec![low, high])
                }
            }
            _ => Err(Error::new_execution(&format!("Invalid size: {size}"))),
        }
    }

    pub fn set_value(&mut self, index: i32, value: Vec<i32>) -> Result<()> {
        let size = Self::bytes_size(&self.type_name);
        let offset = index as usize * size;

        self.set_array_value_by_raw_offset(offset, value, size)
    }

    pub fn set_array_value_by_raw_offset(
        &mut self,
        offset: usize,
        value: Vec<i32>,
        size: usize,
    ) -> Result<()> {
        let data_len = self.data.len();
        match size {
            1..=4 => {
                let bytes = value[0].to_ne_bytes();
                let slice = if cfg!(target_endian = "big") {
                    &bytes[4 - size..]
                } else {
                    &bytes[..size]
                };
                self.data
                    .get_mut(offset..offset + size)
                    .ok_or_else(|| Error::new_execution(&format!(
                        "set_array_value_by_raw_offset: offset out of bounds (offset={offset}, size={size}, data_len={data_len})"
                    )))?
                    .copy_from_slice(slice);
            }
            8 => {
                let mut buf = [0u8; 8];
                if cfg!(target_endian = "big") {
                    buf[..4].copy_from_slice(&value[0].to_ne_bytes());
                    buf[4..].copy_from_slice(&value[1].to_ne_bytes());
                } else {
                    buf[..4].copy_from_slice(&value[1].to_ne_bytes());
                    buf[4..].copy_from_slice(&value[0].to_ne_bytes());
                }

                self.data
                    .get_mut(offset..offset + 8)
                    .ok_or_else(|| Error::new_execution(&format!(
                        "set_array_value_by_raw_offset: offset out of bounds (offset={offset}, size=8, data_len={data_len})"
                    )))?
                    .copy_from_slice(&buf);
            }
            _ => return Err(Error::new_execution(&format!("Invalid size: {size}"))),
        }

        Ok(())
    }

    pub fn get_entire_value(&self) -> Vec<Vec<i32>> {
        let size = Self::bytes_size(&self.type_name);
        let mut result = Vec::new();
        for i in 0..self.data.len() / size {
            let value = self.get_value(i as i32).unwrap();
            result.push(value);
        }
        result
    }

    pub fn get_length(&self) -> i32 {
        let size = Self::bytes_size(&self.type_name);
        let length = self.data.len() / size;
        length as i32
    }

    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    pub fn raw_data(&self) -> &Vec<u8> {
        &self.data
    }

    pub fn raw_data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.data
    }
}

#[derive(Debug, Serialize, Clone)]
pub(crate) enum HeapValue {
    Object(JavaInstance),
    Arr(Array),
}

impl HeapValue {
    pub(super) fn planned_charge(&self) -> AllocResult<u64> {
        match self {
            Self::Object(instance) => charge::instance(instance.get_instance().slots.len()),
            Self::Arr(array) => charge::array(array.type_name.len(), array.data.len()),
        }
    }

    pub(super) fn charge(&self) -> AllocResult<u64> {
        match self {
            Self::Object(instance) => charge::instance(instance.get_instance().slots.len()),
            Self::Arr(array) => charge::array(array.type_name.capacity(), array.data.capacity()),
        }
    }

    pub(super) fn try_clone(&self) -> Result<Self> {
        Ok(match self {
            Self::Arr(array) => Self::Arr(array.try_clone()?),
            Self::Object(instance) => {
                let base = instance.get_instance().try_clone()?;
                Self::Object(match instance {
                    Base(_) => Base(base),
                    Class(mirror) => Class(JavaInstanceClass::new(base, mirror.mirror_klass_id)),
                })
            }
        })
    }
}

impl JavaInstance {
    pub(crate) fn visit_references(
        &self,
        object: i32,
        visit: impl FnMut(i32) -> Result<()>,
    ) -> Result<Option<Reference>> {
        let instance = self.get_instance();
        let referent = instance.layout.visit_references(&instance.slots, visit)?;
        referent
            .map(|referent| {
                Ok(Reference {
                    object,
                    kind: instance.layout.reference_kind(),
                    referent,
                    discovered: self.reference_field("discovered")?,
                    next: self.reference_field("next")?,
                })
            })
            .transpose()
    }

    /// A `java.lang.ref.Reference` field of this object: its word offset.
    fn reference_slot(&self, name: &str) -> Result<&AtomicI32> {
        let instance = self.get_instance();
        match instance.layout.locate(REFERENCE, name) {
            Some((offset, 1)) => Ok(instance
                .slots
                .get(offset)
                .ok_or(AllocError::InvalidLayout)?),
            _ => Err(AllocError::InvalidLayout.into()),
        }
    }

    pub(crate) fn reference_field(&self, name: &str) -> Result<i32> {
        Ok(self.reference_slot(name)?.load(Ordering::Relaxed))
    }

    pub(crate) fn check_reference_field(&self, name: &str) -> Result<()> {
        self.reference_slot(name).map(|_| ())
    }

    pub(crate) fn set_reference_field(&self, name: &str, value: i32) -> Result<()> {
        self.reference_slot(name)?.store(value, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn references(&self) -> Result<References> {
        let instance = self.get_instance();
        instance.layout.references(&instance.slots)
    }

    pub fn set_field_value(
        &mut self,
        class_name: &str,
        field_name_type: &str,
        value: Vec<i32>,
    ) -> Result<()> {
        let instance = self.get_instance();
        let (offset, width) = instance
            .layout
            .locate(class_name, field_name_type)
            .ok_or_else(|| {
                Error::new_execution(&format!(
                    "error setting value for instance field {class_name}.{field_name_type}"
                ))
            })?;
        if value.len() != width {
            return Err(AllocError::InvalidLayout.into());
        }
        let words = [value[0], value.get(1).copied().unwrap_or(0)];
        instance
            .write(offset, width, words)
            .ok_or_else(|| AllocError::InvalidLayout.into())
    }

    pub fn get_field_value(&self, class_name: &str, field_name_type: &str) -> Result<Vec<i32>> {
        let instance = self.get_instance();
        let words = instance
            .layout
            .locate(class_name, field_name_type)
            .and_then(|(offset, width)| Some((instance.read(offset, width)?, width)));
        match words {
            Some((words, width)) => Ok(words[..width].to_vec()),
            None => Err(Error::new_execution(&format!(
                "error getting instance field value {class_name}.{field_name_type}"
            ))),
        }
    }

    /// Reads the field a `getfield` site names, as JVM words.
    pub(crate) fn read_at(
        &self,
        class_name: &str,
        field_name: &str,
        position: &OnceLock<(u32, u32)>,
    ) -> Option<[i32; 2]> {
        let instance = self.get_instance();
        let (offset, width) = instance.position(class_name, field_name, position)?;
        instance.read(offset, width)
    }

    /// Writes the field a `putfield` site names. The caller holds the entry exclusively.
    pub(crate) fn write_at(
        &self,
        class_name: &str,
        field_name: &str,
        position: &OnceLock<(u32, u32)>,
        words: [i32; 2],
    ) -> Option<()> {
        let instance = self.get_instance();
        let (offset, width) = instance.position(class_name, field_name, position)?;
        instance.write(offset, width, words)
    }

    /// The words at a field position a `getfield` site has already found.
    pub(crate) fn read_slots(&self, offset: usize, width: usize) -> Option<[i32; 2]> {
        self.get_instance().read(offset, width)
    }

    /// Writes the words at a field position a `putfield` site has already found. The caller holds
    /// the entry exclusively.
    pub(crate) fn write_slots(&self, offset: usize, width: usize, words: [i32; 2]) -> Option<()> {
        self.get_instance().write(offset, width, words)
    }

    fn get_instance(&self) -> &JavaInstanceBase {
        match self {
            Base(instance) => instance,
            Class(class_instance) => &class_instance.instance,
        }
    }

    /// The object's field values grouped by declaring class, root class first.
    pub(crate) fn field_groups(&self) -> Vec<(&str, Vec<[i32; 2]>)> {
        let instance = self.get_instance();
        instance
            .layout
            .segments()
            .map(|(class, fields)| {
                let values = fields
                    .filter_map(|(offset, width)| instance.read(offset, width))
                    .collect();
                (class, values)
            })
            .collect()
    }

    pub fn klass(&self) -> Result<Arc<JavaClass>> {
        let instance = self.get_instance();
        CLASSES.get_by_id(instance.klass_id)
    }

    /// Id of the object's class in the class registry, without resolving the class.
    pub(crate) fn klass_id(&self) -> usize {
        self.get_instance().klass_id
    }

    pub fn instance_name(&self) -> Result<String> {
        self.klass().map(|klass| klass.this_class_name().clone()) // fixme!!! cloning string
    }
}
