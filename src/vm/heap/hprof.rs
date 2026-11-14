//! Heap dumps in the HPROF binary format (`JAVA PROFILE 1.0.2`), the format HotSpot writes for
//! `HotSpotDiagnosticMXBean.dumpHeap` and that heap analysers read.
//!
//! The dump is taken with every other thread stopped, through the same rendezvous a collection
//! uses, so it is a consistent picture of the heap. Records follow HotSpot's `heapDumper.cpp`:
//! a class is identified by its mirror, mirrors themselves are described by their `CLASS DUMP`
//! record rather than dumped as instances, and class names are written in internal form.

use crate::vm::error::{Error, Result};
use crate::vm::gc::collector::{self, Cause};
use crate::vm::gc::roots;
use crate::vm::heap::heap::HEAP;
use crate::vm::heap::java_instance::{HeapValue, JavaInstance};
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::namespaces::binary_name;
use crate::vm::method_area::primitives_helper::PRIMITIVE_TYPE_BY_CODE;
use crate::vm::safepoint::{self, COORDINATOR};
use crate::vm::threads;
use jdescriptor::TypeDescriptor;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const ID_SIZE: u32 = 8;
/// Heap sub-records are grouped into segments of about this size.
const SEGMENT_BYTES: usize = 1 << 20;
/// The one (empty) stack trace every record refers to.
const TRACE_SERIAL: u32 = 1;

// Top-level record tags.
const TAG_UTF8: u8 = 0x01;
const TAG_LOAD_CLASS: u8 = 0x02;
const TAG_TRACE: u8 = 0x05;
const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;
const TAG_HEAP_DUMP_END: u8 = 0x2C;

// Heap dump sub-record tags.
const ROOT_UNKNOWN: u8 = 0xFF;
const ROOT_STICKY_CLASS: u8 = 0x05;
const ROOT_THREAD_OBJECT: u8 = 0x08;
const CLASS_DUMP: u8 = 0x20;
const INSTANCE_DUMP: u8 = 0x21;
const OBJECT_ARRAY_DUMP: u8 = 0x22;
const PRIMITIVE_ARRAY_DUMP: u8 = 0x23;

/// HPROF basic types.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BasicType {
    Object = 2,
    Boolean = 4,
    Char = 5,
    Float = 6,
    Double = 7,
    Byte = 8,
    Short = 9,
    Int = 10,
    Long = 11,
}

impl BasicType {
    fn of(descriptor: &TypeDescriptor) -> Self {
        match descriptor {
            TypeDescriptor::Boolean => Self::Boolean,
            TypeDescriptor::Char => Self::Char,
            TypeDescriptor::Float => Self::Float,
            TypeDescriptor::Double => Self::Double,
            TypeDescriptor::Byte => Self::Byte,
            TypeDescriptor::Short => Self::Short,
            TypeDescriptor::Integer => Self::Int,
            TypeDescriptor::Long => Self::Long,
            TypeDescriptor::Object(_) | TypeDescriptor::Array(_, _) | TypeDescriptor::Void => {
                Self::Object
            }
        }
    }

    fn of_array_element(code: &str) -> Option<Self> {
        Some(match code {
            "Z" => Self::Boolean,
            "C" => Self::Char,
            "F" => Self::Float,
            "D" => Self::Double,
            "B" => Self::Byte,
            "S" => Self::Short,
            "I" => Self::Int,
            "J" => Self::Long,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::Object => ID_SIZE as usize,
            Self::Boolean | Self::Byte => 1,
            Self::Char | Self::Short => 2,
            Self::Float | Self::Int => 4,
            Self::Double | Self::Long => 8,
        }
    }

    /// Appends a field value held as JVM slots (a wide value is high word, then low word).
    fn write_slots(self, slots: &[i32], out: &mut Vec<u8>) {
        let first = slots.first().copied().unwrap_or(0);
        match self {
            Self::Object => out.extend(object_id(first).to_be_bytes()),
            Self::Boolean | Self::Byte => out.push(first as u8),
            Self::Char | Self::Short => out.extend((first as u16).to_be_bytes()),
            Self::Float | Self::Int => out.extend(first.to_be_bytes()),
            Self::Double | Self::Long => {
                out.extend(first.to_be_bytes());
                out.extend(slots.get(1).copied().unwrap_or(0).to_be_bytes());
            }
        }
    }
}

/// Heap references are positive `i32`s; null is 0.
fn object_id(reference: i32) -> u64 {
    reference as u32 as u64
}

/// Names get identifiers above every possible object identifier.
const FIRST_NAME_ID: u64 = 1 << 32;

#[derive(Default)]
struct Names {
    ids: HashMap<String, u64>,
    ordered: Vec<String>,
}

impl Names {
    fn id(&mut self, name: &str) -> u64 {
        if let Some(id) = self.ids.get(name) {
            return *id;
        }
        let id = FIRST_NAME_ID + self.ordered.len() as u64;
        self.ids.insert(name.to_string(), id);
        self.ordered.push(name.to_string());
        id
    }
}

struct FieldDescription {
    name: u64,
    basic_type: BasicType,
}

/// Everything a `CLASS DUMP` record says about a class.
struct ClassDescription {
    id: u64,
    name: u64,
    super_id: u64,
    loader_id: u64,
    protection_domain_id: u64,
    /// `(name, type, value)` of each static field.
    statics: Vec<(u64, BasicType, Vec<u8>)>,
    /// Instance fields this class declares, in storage order.
    fields: Vec<FieldDescription>,
    /// Bytes an instance occupies in an `INSTANCE DUMP`: this class's fields and its ancestors'.
    instance_size: u32,
    sticky: bool,
}

fn invalid(message: &str) -> Error {
    Error::new_execution(&format!("heap dump: {message}"))
}

fn mirror_field(mirror_ref: i32, name: &str) -> u64 {
    HEAP.get_object_field_value(mirror_ref, "java/lang/Class", name)
        .ok()
        .and_then(|value| value.first().copied())
        .map_or(0, object_id)
}

fn describe_class(
    class: &JavaClass,
    mirror_ref: i32,
    names: &mut Names,
) -> Result<ClassDescription> {
    let super_id = class
        .parent()
        .as_ref()
        .and_then(|parent| CLASSES.get_loaded(parent))
        .and_then(|parent| parent.mirror_ref_if_present())
        .map_or(0, object_id);
    let mut statics = Vec::new();
    for (name, value) in class.static_field_values() {
        let basic_type = class
            .field_info(name)
            .map(|info| BasicType::of(info.type_descriptor()))
            .ok_or_else(|| invalid("static field without metadata"))?;
        let slots = value
            .try_raw_value()
            .ok_or_else(|| invalid("a static field is locked"))?;
        let mut bytes = Vec::with_capacity(basic_type.size());
        basic_type.write_slots(&slots, &mut bytes);
        statics.push((names.id(name), basic_type, bytes));
    }
    let fields: Vec<_> = class
        .declared_instance_fields()
        .map(|(name, descriptor)| FieldDescription {
            name: names.id(name),
            basic_type: descriptor.map_or(BasicType::Object, BasicType::of),
        })
        .collect();
    let loader_id = mirror_field(mirror_ref, "classLoader");
    Ok(ClassDescription {
        id: object_id(mirror_ref),
        name: names.id(&binary_name(class.this_class_name())),
        super_id,
        loader_id,
        protection_domain_id: mirror_field(mirror_ref, "protectionDomain"),
        statics,
        fields,
        instance_size: 0, // filled in once every class is described
        sticky: loader_id == 0,
    })
}

struct Dump<W: Write> {
    out: W,
    segment: Vec<u8>,
}

impl<W: Write> Dump<W> {
    fn record(&mut self, tag: u8, body: &[u8]) -> io::Result<()> {
        self.out.write_all(&[tag])?;
        self.out.write_all(&0u32.to_be_bytes())?; // microseconds since the header's timestamp
        self.out.write_all(&(body.len() as u32).to_be_bytes())?;
        self.out.write_all(body)
    }

    /// Ends a heap sub-record, starting a new segment when the current one is full.
    fn end_sub_record(&mut self) -> io::Result<()> {
        if self.segment.len() >= SEGMENT_BYTES {
            self.flush_segment()?;
        }
        Ok(())
    }

    fn flush_segment(&mut self) -> io::Result<()> {
        if self.segment.is_empty() {
            return Ok(());
        }
        let segment = std::mem::take(&mut self.segment);
        self.record(TAG_HEAP_DUMP_SEGMENT, &segment)?;
        self.segment = segment;
        self.segment.clear();
        Ok(())
    }
}

/// Writes a heap dump to `path`, which must not exist yet. With `live`, unreachable objects are
/// collected first, so only live ones are dumped.
///
/// The outer `Result` is a VM failure; the inner one is the file's, for the caller to report as
/// an `IOException`.
pub(crate) fn dump(path: &Path, live: bool) -> Result<io::Result<()>> {
    let file = match File::create_new(path) {
        Ok(file) => file,
        Err(error) => return Ok(Err(error)),
    };
    if live {
        collector::collect(Cause::Explicit);
    }
    let owner = safepoint::current().ok_or_else(|| invalid("the thread is not attached"))?;
    let mut attempts = 0;
    let operation = loop {
        // An array's class is only created when something asks for it, and creating one
        // allocates, so that has to happen while the world is still running.
        for name in unloaded_array_classes() {
            CLASSES.get(&name)?;
        }
        // `None` means a collection finished while this thread was getting everyone stopped.
        let stopped =
            COORDINATOR.request_collection_for(owner, JavaThread::snapshot_roots(), false);
        let Some(operation) = stopped else {
            continue;
        };
        // Another thread may have made an array of a new type in between.
        if unloaded_array_classes().is_empty() {
            break operation;
        }
        drop(operation);
        attempts += 1;
        if attempts == 16 {
            return Err(invalid("array classes keep appearing"));
        }
    };
    let root_refs = roots::snapshot_retrying(&operation)?;
    let mut dump = Dump {
        out: BufWriter::with_capacity(1 << 16, file),
        segment: Vec::with_capacity(SEGMENT_BYTES + (1 << 16)),
    };
    let written = write(&mut dump, &root_refs);
    // Dropped without `complete()`: nothing was collected, the world simply resumes.
    drop(operation);
    match written {
        Ok(result) => Ok(result.and_then(|()| dump.out.flush())),
        Err(error) => Err(error),
    }
}

/// Names of the array types that have instances but no class with a mirror yet.
fn unloaded_array_classes() -> HashSet<String> {
    let mut missing = HashSet::new();
    for entry in HEAP.data.iter() {
        if let HeapValue::Arr(array) = entry.value() {
            let name = array.type_name();
            let object_array = BasicType::of_array_element(&name[1..]).is_none();
            let loaded = || {
                CLASSES
                    .get_loaded(name)
                    .is_some_and(|class| class.mirror_ref_if_present().is_some())
            };
            if object_array && !missing.contains(name) && !loaded() {
                missing.insert(name.to_string());
            }
        }
    }
    missing
}

fn write<W: Write>(dump: &mut Dump<W>, root_refs: &[i32]) -> Result<io::Result<()>> {
    // Pass 1: describe every class, which also collects every name the dump uses.
    let mut names = Names::default();
    let mut classes: Vec<ClassDescription> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut class_keys: Vec<Arc<JavaClass>> = Vec::new();
    for class in CLASSES.snapshot() {
        let Some(mirror_ref) = class.mirror_ref_if_present() else {
            continue;
        };
        if PRIMITIVE_TYPE_BY_CODE.contains_key(class.this_class_name().as_str()) {
            continue; // primitive types have a mirror but are not classes
        }
        by_key.insert(class.this_class_name().clone(), classes.len());
        classes.push(describe_class(&class, mirror_ref, &mut names)?);
        class_keys.push(class);
    }
    for index in 0..classes.len() {
        let mut size = 0usize;
        let mut current = Some(index);
        let mut hops = 0;
        while let Some(at) = current {
            size += classes[at]
                .fields
                .iter()
                .map(|field| field.basic_type.size())
                .sum::<usize>();
            current = class_keys[at]
                .parent()
                .as_ref()
                .and_then(|parent| by_key.get(parent).copied());
            hops += 1;
            if hops > 1024 {
                return Err(invalid("class hierarchy does not terminate"));
            }
        }
        classes[index].instance_size = size as u32;
    }
    let field_types: HashMap<&str, Vec<BasicType>> = by_key
        .iter()
        .map(|(key, &index)| {
            (
                key.as_str(),
                classes[index]
                    .fields
                    .iter()
                    .map(|field| field.basic_type)
                    .collect(),
            )
        })
        .collect();

    macro_rules! io {
        ($operation:expr) => {
            if let Err(error) = $operation {
                return Ok(Err(error));
            }
        };
    }

    // Header and the records that must precede the heap dump.
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    io!(dump.out.write_all(b"JAVA PROFILE 1.0.2\0"));
    io!(dump.out.write_all(&ID_SIZE.to_be_bytes()));
    io!(dump.out.write_all(&millis.to_be_bytes()));
    let mut body = Vec::new();
    for (index, name) in names.ordered.iter().enumerate() {
        body.clear();
        body.extend((FIRST_NAME_ID + index as u64).to_be_bytes());
        body.extend(name.as_bytes());
        io!(dump.record(TAG_UTF8, &body));
    }
    for (index, class) in classes.iter().enumerate() {
        body.clear();
        body.extend((index as u32 + 1).to_be_bytes()); // class serial number
        body.extend(class.id.to_be_bytes());
        body.extend(TRACE_SERIAL.to_be_bytes());
        body.extend(class.name.to_be_bytes());
        io!(dump.record(TAG_LOAD_CLASS, &body));
    }
    body.clear();
    body.extend(TRACE_SERIAL.to_be_bytes());
    body.extend(0u32.to_be_bytes()); // thread serial: none
    body.extend(0u32.to_be_bytes()); // no frames
    io!(dump.record(TAG_TRACE, &body));

    // Pass 2: the heap dump proper. Classes first, as HotSpot orders it.
    for class in &classes {
        let out = &mut dump.segment;
        out.push(CLASS_DUMP);
        out.extend(class.id.to_be_bytes());
        out.extend(TRACE_SERIAL.to_be_bytes());
        out.extend(class.super_id.to_be_bytes());
        out.extend(class.loader_id.to_be_bytes());
        out.extend(0u64.to_be_bytes()); // signers
        out.extend(class.protection_domain_id.to_be_bytes());
        out.extend(0u64.to_be_bytes()); // reserved
        out.extend(0u64.to_be_bytes()); // reserved
        out.extend(class.instance_size.to_be_bytes());
        out.extend(0u16.to_be_bytes()); // constant pool entries
        out.extend((class.statics.len() as u16).to_be_bytes());
        for (name, basic_type, value) in &class.statics {
            out.extend(name.to_be_bytes());
            out.push(*basic_type as u8);
            out.extend(value);
        }
        out.extend((class.fields.len() as u16).to_be_bytes());
        for field in &class.fields {
            out.extend(field.name.to_be_bytes());
            out.push(field.basic_type as u8);
        }
        io!(dump.end_sub_record());
    }

    let mut class_of_id: HashMap<usize, Option<u64>> = HashMap::new();
    let mut failure = None;
    for entry in HEAP.data.iter() {
        let reference = *entry.key();
        let written = match entry.value() {
            HeapValue::Object(JavaInstance::Class(mirror)) => {
                // A mirror is its class's `CLASS DUMP`; only a primitive type's is an instance.
                let primitive = CLASSES
                    .get_by_id(mirror.mirror_klass_id())
                    .is_ok_and(|class| {
                        PRIMITIVE_TYPE_BY_CODE.contains_key(class.this_class_name().as_str())
                    });
                if primitive {
                    write_instance(
                        entry.value(),
                        reference,
                        &mut class_of_id,
                        &field_types,
                        dump,
                    )
                } else {
                    Ok(())
                }
            }
            HeapValue::Object(_) => write_instance(
                entry.value(),
                reference,
                &mut class_of_id,
                &field_types,
                dump,
            ),
            HeapValue::Arr(array) => write_array(
                array.type_name(),
                array.raw_data(),
                reference,
                &by_key,
                &classes,
                dump,
            ),
        };
        match written.map(|()| dump.end_sub_record()) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Ok(Err(error)),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }

    // Roots: thread objects, system classes, and everything else the collector treats as one.
    let mut reported: HashSet<i32> = HashSet::new();
    let mut thread_refs = threads::parker_refs();
    thread_refs.sort_unstable();
    for (index, thread_ref) in thread_refs.iter().enumerate() {
        reported.insert(*thread_ref);
        let out = &mut dump.segment;
        out.push(ROOT_THREAD_OBJECT);
        out.extend(object_id(*thread_ref).to_be_bytes());
        out.extend((index as u32 + 1).to_be_bytes()); // thread serial number
        out.extend(TRACE_SERIAL.to_be_bytes());
    }
    for class in classes.iter().filter(|class| class.sticky) {
        reported.insert(class.id as i32);
        dump.segment.push(ROOT_STICKY_CLASS);
        dump.segment.extend(class.id.to_be_bytes());
    }
    for &root in root_refs {
        if root != 0 && reported.insert(root) {
            dump.segment.push(ROOT_UNKNOWN);
            dump.segment.extend(object_id(root).to_be_bytes());
        }
        io!(dump.end_sub_record());
    }
    io!(dump.flush_segment());
    io!(dump.record(TAG_HEAP_DUMP_END, &[]));
    Ok(Ok(()))
}

fn write_instance<W: Write>(
    value: &HeapValue,
    reference: i32,
    class_of_id: &mut HashMap<usize, Option<u64>>,
    field_types: &HashMap<&str, Vec<BasicType>>,
    dump: &mut Dump<W>,
) -> Result<()> {
    let HeapValue::Object(instance) = value else {
        return Ok(());
    };
    let class_id = *class_of_id.entry(instance.klass_id()).or_insert_with(|| {
        CLASSES
            .get_by_id(instance.klass_id())
            .ok()
            .and_then(|class| class.mirror_ref_if_present())
            .map(object_id)
    });
    let class_id = class_id.ok_or_else(|| invalid("an instance's class has no mirror"))?;

    let out = &mut dump.segment;
    out.push(INSTANCE_DUMP);
    out.extend(object_id(reference).to_be_bytes());
    out.extend(TRACE_SERIAL.to_be_bytes());
    out.extend(class_id.to_be_bytes());
    let length_at = out.len();
    out.extend(0u32.to_be_bytes());
    // Field values: the object's own class first, then each superclass in turn.
    for (class_key, fields) in instance.field_groups().iter().rev() {
        let types = field_types
            .get(class_key.as_str())
            .ok_or_else(|| invalid("an instance has fields of an undumped class"))?;
        if types.len() != fields.len() {
            return Err(invalid("an instance disagrees with its class's fields"));
        }
        for (field, basic_type) in fields.values().zip(types) {
            let slots = field
                .try_raw_value()
                .ok_or_else(|| invalid("an instance field is locked"))?;
            basic_type.write_slots(&slots, out);
        }
    }
    let length = (out.len() - length_at - 4) as u32;
    out[length_at..length_at + 4].copy_from_slice(&length.to_be_bytes());
    Ok(())
}

fn write_array<W: Write>(
    type_name: &str,
    data: &[u8],
    reference: i32,
    by_key: &HashMap<String, usize>,
    classes: &[ClassDescription],
    dump: &mut Dump<W>,
) -> Result<()> {
    let out = &mut dump.segment;
    let element = &type_name[1..];
    match BasicType::of_array_element(element) {
        Some(basic_type) => {
            let width = basic_type.size();
            out.push(PRIMITIVE_ARRAY_DUMP);
            out.extend(object_id(reference).to_be_bytes());
            out.extend(TRACE_SERIAL.to_be_bytes());
            out.extend(((data.len() / width) as u32).to_be_bytes());
            out.push(basic_type as u8);
            if width == 1 || cfg!(target_endian = "big") {
                out.extend(data);
            } else {
                // Elements are stored in native byte order; HPROF is big-endian.
                for element in data.chunks_exact(width) {
                    out.extend(element.iter().rev());
                }
            }
        }
        None => {
            let class_id = by_key
                .get(type_name)
                .map(|&index| classes[index].id)
                .ok_or_else(|| invalid(&format!("the array class {type_name} is not loaded")))?;
            out.push(OBJECT_ARRAY_DUMP);
            out.extend(object_id(reference).to_be_bytes());
            out.extend(TRACE_SERIAL.to_be_bytes());
            out.extend(((data.len() / 4) as u32).to_be_bytes());
            out.extend(class_id.to_be_bytes());
            for element in data.chunks_exact(4) {
                let element = i32::from_ne_bytes([element[0], element[1], element[2], element[3]]);
                out.extend(object_id(element).to_be_bytes());
            }
        }
    }
    Ok(())
}
