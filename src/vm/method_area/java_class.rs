use crate::vm::error::{Error, Result};
use crate::vm::execution_engine::invoke_dynamic_runner::InvokeDynamicRunner;
use crate::vm::gc::layout::InstanceLayout;
use crate::vm::heap::java_instance::{ClassName, FieldNameType};
use crate::vm::helper::is_reference;
use crate::vm::method_area::field::{FieldInfo, FieldValue};
use crate::vm::method_area::java_method::{CodeContext, JavaMethod};
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::lookup;
use crate::vm::method_area::namespaces::{self, DefiningLoader};
use crate::vm::stack::stack_frame::ExceptionTable;
use getset::{CopyGetters, Getters};
use indexmap::{IndexMap, IndexSet};
use jclassmodel::attributes::Attributes;
use jclassmodel::constant_pool::{ConstantPool, ConstantPoolLookup};
use jclassmodel::modifiers::{ClassModifier, NestedClassModifier};
use jclassmodel::{ClassModel, EnclosingMethodInfo, FieldModel, MethodModel};
use jdescriptor::TypeDescriptor;
use once_cell::sync::OnceCell;
use parking_lot::{Condvar, Mutex};
use std::sync::{Arc, OnceLock};
use std::thread::ThreadId;

pub const STATIC_FIELDS_START: i64 = i32::MAX as i64;

/// Instance-field offsets are the field's slot index scaled to a 4-byte-aligned byte offset
/// (`index * 4`). Each field therefore owns its own aligned word, so the JDK's sub-word Unsafe ops
/// (`compareAndSetBoolean`/`Byte`/`Short`/`Char`, which word-align the offset via `offset & ~3` and
/// mask in a byte) operate on the intended field instead of aliasing a neighbour. Must divide the
/// offset back out in [`JavaClass::get_field_name_by_offset`].
const FIELD_OFFSET_SCALE: i64 = 4;

type FullyQualifiedFieldName = String; // format: com/example/models/Person.name

#[path = "java_class_gc.rs"]
mod gc;

/// A class's fields, split the three ways [`JavaClass`] stores them: metadata for every field,
/// values for the static ones, and the template instances are created from.
struct BuiltFields {
    info: IndexMap<String, Arc<FieldInfo>>,
    static_values: IndexMap<String, Arc<FieldValue>>,
    static_references: Box<[String]>,
    instance_template: IndexMap<String, FieldValue>,
}

#[derive(Debug, Getters, CopyGetters)]
pub(crate) struct JavaClass {
    methods: IndexMap<String, Arc<JavaMethod>>,
    fields_info: IndexMap<String, Arc<FieldInfo>>,
    static_fields: IndexMap<String, Arc<FieldValue>>,
    static_references: Box<[String]>,
    instance_fields_template: IndexMap<String, FieldValue>,
    #[get = "pub"]
    constant_pool: ConstantPool,
    #[get = "pub"]
    attributes: Attributes,
    #[get = "pub"]
    this_class_name: String,
    #[get = "pub"]
    external_name: String,
    #[get = "pub"]
    parent: Option<String>,
    #[get = "pub"]
    interfaces: IndexSet<String>,
    #[get_copy = "pub"]
    class_modifiers: ClassModifier,
    /// Modifiers from this class's own `InnerClasses` record, present only when it is nested.
    /// The only place `private`, `protected` and `static` are recorded for a nested class; the
    /// `ClassFile`'s own `access_flags` structurally cannot express them.
    #[get_copy = "pub"]
    nested_class_modifiers: Option<NestedClassModifier>,

    #[get = "pub"]
    static_fields_init_state: InitState,

    instance_fields_hierarchy:
        OnceCell<Arc<IndexMap<ClassName, IndexMap<FieldNameType, FieldValue>>>>,
    instance_layout: OnceCell<Arc<InstanceLayout>>,
    fields_offset_mapping: OnceCell<IndexSet<FullyQualifiedFieldName>>,
    vtable: OnceCell<IndexMap<String, Arc<JavaMethod>>>,
    #[get = "pub"]
    declaring_class: Option<String>,
    #[get = "pub"]
    annotations_raw: Option<Vec<u8>>,
    #[get = "pub"]
    enclosing_method: Option<EnclosingMethodInfo>,
    #[get = "pub"]
    source_file: Option<String>,
    #[get = "pub"]
    invoke_dynamic_runner: InvokeDynamicRunner,
    mirror_clazz_ref: OnceLock<i32>,
    /// `(major, minor)` class file version; `(0, 0)` for synthetic (primitive and array) classes.
    #[get_copy = "pub"]
    class_file_version: (u16, u16),
    /// Symbolic references already resolved from this class's constant pool, keyed by kind and
    /// index. Invoke and field opcodes hit this on every execution, so resolution (and descriptor
    /// parsing) happens once per call site instead of once per bytecode.
    /// Indexed by constant-pool index. Reads are a single atomic load: invokes from every thread
    /// hit this, so it must not take a lock or touch a shared reference count.
    member_refs: Box<[OnceLock<(MemberRefKind, MemberRef)>]>,
    /// The loader that defined this class, when it is not one of the built-in loaders; `None`
    /// for the shared built-in namespace, where symbolic names are registry keys as they stand.
    #[get_copy = "pub(crate)"]
    defining_loader: Option<DefiningLoader>,
    /// Defined through `Lookup.defineHiddenClass` (which includes every lambda proxy).
    #[get_copy = "pub(crate)"]
    hidden: bool,
    /// Registry key of the nest host, once determined.
    nest_host: OnceLock<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MemberRefKind {
    Method,
    InterfaceMethod,
    Field,
}

/// A resolved `CONSTANT_{Methodref,InterfaceMethodref,Fieldref}` entry.
#[derive(Debug)]
pub(crate) struct MemberRef {
    pub(crate) class_name: String,
    pub(crate) name: String,
    pub(crate) descriptor: String,
    /// `name:descriptor`, the key used by method lookup.
    pub(crate) full_signature: String,
    method_descriptor: OnceLock<jdescriptor::MethodDescriptor>,
    /// Inline caches for invoke sites using this reference. Resolution is deterministic for a
    /// given (receiver) class, so each is filled once and then only read (a single atomic load).
    /// `invokestatic`: the resolved class and method.
    pub(crate) static_target: OnceLock<(Arc<JavaClass>, Arc<JavaMethod>)>,
    /// `invokespecial`: the resolved method.
    pub(crate) special_target: OnceLock<Arc<JavaMethod>>,
    /// `invokevirtual`/`invokeinterface`: the first receiver class seen and its implementation
    /// (monomorphic). Other receiver classes take the full lookup.
    pub(crate) virtual_target: OnceLock<(usize, Arc<JavaMethod>)>,
}

impl MemberRef {
    /// The parsed method descriptor, parsed on first use.
    pub(crate) fn method_descriptor(&self) -> Result<&jdescriptor::MethodDescriptor> {
        if let Some(parsed) = self.method_descriptor.get() {
            return Ok(parsed);
        }
        let parsed = self.descriptor.parse()?;
        Ok(self.method_descriptor.get_or_init(|| parsed))
    }
}

#[derive(Debug, Default)]
pub struct InitState {
    state: Mutex<InnerState>,
    cv: Condvar,
}

impl InitState {
    /// Returns true only to the elected initializer. Ownership is logical; no
    /// mutex guard survives this call or spans the initializer's Java execution.
    pub(crate) fn begin(&self) -> std::result::Result<bool, i32> {
        let owner = std::thread::current().id();
        let mut state = crate::vm::safepoint::lock(&self.state);
        loop {
            match *state {
                InnerState::Uninitialized => {
                    *state = InnerState::Initializing { owner };
                    return Ok(true);
                }
                InnerState::Initializing { owner: current } if owner == current => {
                    return Ok(false)
                }
                InnerState::Initializing { .. } => {
                    state = crate::vm::safepoint::wait(&self.cv, &self.state, state);
                }
                InnerState::Initialized => return Ok(false),
                InnerState::Failed { cause_root } => return Err(cause_root),
            }
        }
    }

    pub(crate) fn finish(&self, cause: Option<i32>) {
        let mut state = crate::vm::safepoint::lock(&self.state);
        assert_eq!(
            *state,
            InnerState::Initializing {
                owner: std::thread::current().id()
            }
        );
        *state = match cause {
            Some(cause_root) => InnerState::Failed { cause_root },
            None => InnerState::Initialized,
        };
        self.cv.notify_all();
    }

    /// The failed cause is owned by permanent class metadata and is a global GC
    /// root. Enumeration uses this value without class loading or Java execution.
    pub(crate) fn get_inner_state(&self) -> InnerState {
        *crate::vm::safepoint::lock(&self.state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum InnerState {
    #[default]
    Uninitialized,
    Initializing {
        owner: ThreadId,
    },
    Initialized,
    Failed {
        cause_root: i32,
    },
}

impl JavaClass {
    /// Resolves a member reference from this class's constant pool, caching the result.
    pub(crate) fn member_ref(&self, kind: MemberRefKind, index: u16) -> Result<&MemberRef> {
        let mismatch = || {
            Error::new_constant_pool(&format!(
                "Error getting {kind:?} info by index {index} in {}",
                self.this_class_name()
            ))
        };
        let slot = self
            .member_refs
            .get(usize::from(index))
            .ok_or_else(mismatch)?;
        if let Some((cached_kind, member)) = slot.get() {
            return if *cached_kind == kind {
                Ok(member)
            } else {
                Err(mismatch())
            };
        }
        let pool = self.constant_pool();
        let resolved = match kind {
            MemberRefKind::Method => pool.get_full_method_info(index),
            MemberRefKind::InterfaceMethod => pool.get_full_interfacemethodref_info(index),
            MemberRefKind::Field => pool.get_full_field_info(index),
        };
        let (class_name, name, descriptor) = resolved.ok_or_else(mismatch)?;
        let class_name = self.resolve_class_name(&class_name)?.into_owned();
        let member = MemberRef {
            full_signature: format!("{name}:{descriptor}"),
            class_name,
            name,
            descriptor,
            method_descriptor: OnceLock::new(),
            static_target: OnceLock::new(),
            special_target: OnceLock::new(),
            virtual_target: OnceLock::new(),
        };
        // Racing resolvers produce identical values; keep whichever was stored first.
        let (_, member) = slot.get_or_init(|| (kind, member));
        Ok(member)
    }

    /// Builds a class from its resolved class-file view, adding the runtime state (field values,
    /// initialization state, vtable and offset caches) that the class file itself cannot describe.
    pub fn from_model(model: ClassModel) -> Result<Self> {
        let ClassModel {
            version,
            nested_class_modifiers,
            name,
            external_name,
            super_class_name,
            interfaces,
            modifiers,
            methods,
            fields,
            declaring_class,
            annotations_raw,
            enclosing_method,
            source_file,
            constant_pool,
            attributes,
        } = model;

        let built_fields = Self::build_fields(fields, &name)?;

        let mut class = Self::assemble(
            Self::build_methods(methods, &name),
            built_fields.info,
            built_fields.static_values,
            built_fields.static_references,
            built_fields.instance_template,
            constant_pool,
            attributes,
            name,
            external_name,
            super_class_name,
            interfaces.into_iter().collect(),
            modifiers,
            nested_class_modifiers,
            declaring_class,
            annotations_raw,
            enclosing_method,
            source_file,
        );
        class.class_file_version = (version.major, version.minor);
        class.add_vm_fields()?;
        Ok(class)
    }

    /// Builds a class the VM invents rather than loads: primitives (`int`, `void`, …) and array
    /// classes. They have no class file, so they carry no methods, fields, constant pool or
    /// attributes.
    pub fn synthetic(
        this_class_name: String,
        external_name: String,
        parent: Option<String>,
        interfaces: IndexSet<String>,
    ) -> Self {
        Self::assemble(
            IndexMap::new(),
            IndexMap::new(),
            IndexMap::new(),
            Box::new([]),
            IndexMap::new(),
            ConstantPool::empty(),
            Attributes::empty(),
            this_class_name,
            external_name,
            parent,
            interfaces,
            ClassModifier::Public | ClassModifier::Final | ClassModifier::Abstract,
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn build_methods(
        methods: Vec<MethodModel>,
        class_name: &str,
    ) -> IndexMap<String, Arc<JavaMethod>> {
        let mut method_by_signature = IndexMap::new();
        // One shared name for every method of the class, so the interpreter can tell that
        // consecutive frames belong to the same class by pointer identity alone.
        let shared_class_name = Arc::new(class_name.to_string());

        for mut method in methods {
            // Polymorphic-signature natives have no fixed descriptor, so they are keyed by bare name.
            let key = if method.is_polymorphic_signature() {
                method.name.clone()
            } else {
                method.name_signature.clone()
            };

            // Read before `code` is moved out of `method` below.
            let is_native = method.is_native();
            let parameters = method.has_parameters_attribute.then(|| {
                method
                    .parameters
                    .iter()
                    .map(|parameter| (parameter.name.clone(), parameter.modifiers.bits()))
                    .collect::<Vec<_>>()
            });
            let generic_signature = method.generic_signature.take();

            let code_context = method.code.map(|code| {
                CodeContext::new(
                    code.max_stack,
                    code.max_locals,
                    Arc::new(code.bytecode),
                    Arc::new(code.line_numbers),
                    Arc::new(ExceptionTable::new(code.exception_table)),
                    Arc::new(code.local_variable_table),
                )
            });

            method_by_signature.insert(
                key,
                Arc::new(
                    JavaMethod::new(
                        method.descriptor,
                        class_name,
                        &method.name_signature,
                        code_context,
                        is_native,
                        method.exceptions,
                        method.modifiers.bits() as i32,
                        &method.name,
                        method.annotation_default_raw,
                        method.annotations_raw,
                        method.runtime_visible_annotations,
                        method_by_signature.len() as i32, // God, forgive me, this is a hack to get the method index
                    )
                    .with_reflection_metadata(generic_signature, parameters)
                    .with_class_name(Arc::clone(&shared_class_name)),
                ),
            );
        }

        method_by_signature
    }

    fn build_fields(fields: Vec<FieldModel>, class_name: &str) -> Result<BuiltFields> {
        let mut info = IndexMap::new();
        let mut instance_template = IndexMap::new();
        let mut static_values = IndexMap::new();
        let mut static_references = Vec::new();
        for field in fields {
            // This storage is keyed by bare name. Do not publish an incomplete
            // layout when a class file declares same-name/different-type fields.
            if info.contains_key(&field.name) {
                return Err(Error::new_execution(&format!(
                    "Cannot represent duplicate field name {class_name}.{}",
                    field.name
                )));
            }
            let is_static = field.is_static();
            info.insert(
                field.name.clone(),
                Arc::new(
                    FieldInfo::new(
                        field.descriptor.clone(),
                        field.modifiers.bits(),
                        class_name.to_string(),
                        field.name.clone(),
                    )
                    .with_reflection_metadata(field.signature, field.annotations_raw),
                ),
            );
            if is_static {
                if is_reference(&field.descriptor) {
                    static_references.push(field.name.clone());
                }
                static_values.insert(field.name, Arc::new(FieldValue::new(field.descriptor)?));
            } else {
                instance_template.insert(field.name, FieldValue::new(field.descriptor)?);
            }
        }

        Ok(BuiltFields {
            info,
            static_values,
            static_references: static_references.into_boxed_slice(),
            instance_template,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        methods: IndexMap<String, Arc<JavaMethod>>,
        fields_info: IndexMap<String, Arc<FieldInfo>>,
        static_fields: IndexMap<String, Arc<FieldValue>>,
        static_references: Box<[String]>,
        instance_fields_template: IndexMap<String, FieldValue>,
        constant_pool: ConstantPool,
        attributes: Attributes,
        this_class_name: String,
        external_name: String,
        parent: Option<String>,
        interfaces: IndexSet<String>,
        class_modifiers: ClassModifier,
        nested_class_modifiers: Option<NestedClassModifier>,
        declaring_class: Option<String>,
        annotations_raw: Option<Vec<u8>>,
        enclosing_method: Option<EnclosingMethodInfo>,
        source_file: Option<String>,
    ) -> Self {
        let member_ref_slots = constant_pool.len();
        Self {
            methods,
            fields_info,
            static_fields,
            static_references,
            instance_fields_template,
            constant_pool,
            attributes,
            this_class_name,
            external_name,
            parent,
            interfaces,
            class_modifiers,
            nested_class_modifiers,
            static_fields_init_state: InitState::default(),
            instance_fields_hierarchy: OnceCell::new(),
            instance_layout: OnceCell::new(),
            fields_offset_mapping: OnceCell::new(),
            vtable: OnceCell::new(),
            declaring_class,
            annotations_raw,
            enclosing_method,
            source_file,
            invoke_dynamic_runner: InvokeDynamicRunner::default(),
            mirror_clazz_ref: OnceLock::default(),
            class_file_version: (0, 0),
            member_refs: (0..member_ref_slots).map(|_| OnceLock::new()).collect(),
            defining_loader: None,
            hidden: false,
            nest_host: OnceLock::new(),
        }
    }

    /// Records who defined the class. Only before publication, while it is exclusively owned.
    pub(crate) fn set_definition(
        &mut self,
        defining_loader: Option<DefiningLoader>,
        hidden: bool,
        nest_host: Option<String>,
    ) {
        self.defining_loader = defining_loader;
        self.hidden = hidden;
        if let Some(nest_host) = nest_host {
            self.nest_host = OnceLock::from(nest_host);
        }
    }

    /// The name class files use for this class: the registry key without its loader serial.
    pub(crate) fn binary_name(&self) -> &str {
        match self.defining_loader {
            Some(_) if !self.hidden => self
                .this_class_name
                .split(namespaces::LOADER_KEY_DELIM)
                .next()
                .unwrap_or(&self.this_class_name),
            _ => &self.this_class_name,
        }
    }

    /// The registry key of the class a symbolic `name` denotes to this class, i.e. as resolved
    /// through its defining loader. See [`namespaces::resolve`].
    pub(crate) fn resolve_class_name<'a>(
        &self,
        name: &'a str,
    ) -> Result<std::borrow::Cow<'a, str>> {
        namespaces::resolve(self, name)
    }

    /// The nest host's registry key (JVMS §5.4.4): the class named by `NestHost` when it loads,
    /// shares this class's package and lists this class among its `NestMembers`; otherwise the
    /// class itself. A hidden nestmate has its host fixed when it is defined.
    pub(crate) fn nest_host(&self) -> &str {
        if let Some(host) = self.nest_host.get() {
            return host;
        }
        let host = self
            .declared_nest_host()
            .unwrap_or_else(|| self.this_class_name.clone());
        self.nest_host.get_or_init(|| host)
    }

    fn declared_nest_host(&self) -> Option<String> {
        let declared = self
            .attributes
            .get_nest_host(&self.constant_pool)
            .ok()
            .flatten()?;
        let key = self.resolve_class_name(&declared).ok()?;
        let host = CLASSES.get(&key).ok()?;
        // Same runtime package: the same package name and the same defining loader.
        let package = |name: &str| name.rfind('/').map(|end| name[..end].to_string());
        if package(host.binary_name()) != package(self.binary_name())
            || namespaces::mirror_loader(&host).ok()? != namespaces::mirror_loader(self).ok()?
        {
            return None;
        }
        let members = host
            .attributes
            .get_nest_members(&host.constant_pool)
            .ok()
            .flatten()?;
        members
            .iter()
            .any(|member| member == self.binary_name())
            .then(|| host.this_class_name.clone())
    }

    pub fn is_interface(&self) -> bool {
        self.class_modifiers.contains(ClassModifier::Interface)
    }

    /// Modifiers as `Class.getModifiers()` reports them, which are `java.lang.reflect.Modifier`
    /// bits rather than the class file's `access_flags`.
    ///
    /// A nested class records its real `private`, `protected` and `static` only in the
    /// `InnerClasses` entry describing it, so those flags *replace* the class file's own when
    /// present, matching HotSpot's `InstanceKlass::compute_modifier_flags`. ACC_SUPER and
    /// ACC_MODULE are then stripped, since neither is a `Modifier` bit.
    pub fn reflection_modifiers(&self) -> u16 {
        /// `JVM_ACC_WRITTEN_FLAGS`: everything below ACC_MODULE (0x8000).
        const WRITTEN_FLAGS: u16 = 0x7FFF;

        let bits = self
            .nested_class_modifiers
            .map(|modifiers| modifiers.bits())
            .unwrap_or_else(|| self.class_modifiers.bits());

        bits & !ClassModifier::Super.bits() & WRITTEN_FLAGS
    }

    pub fn static_field(&self, field_name: &str) -> Option<Arc<FieldValue>> {
        self.static_fields.get(field_name).map(Arc::clone)
    }

    /// Every static field with its current storage, in declaration order.
    pub(crate) fn static_field_values(&self) -> impl Iterator<Item = (&String, &Arc<FieldValue>)> {
        self.static_fields.iter()
    }

    /// The instance fields this class itself declares, in the order an instance stores them.
    pub(crate) fn declared_instance_fields(
        &self,
    ) -> impl Iterator<Item = (&String, Option<&TypeDescriptor>)> {
        self.instance_fields_template.keys().map(|name| {
            let descriptor = self
                .fields_info
                .get(name)
                .map(|info| info.type_descriptor());
            (name, descriptor)
        })
    }

    pub fn field_info(&self, field_name: &str) -> Option<&Arc<FieldInfo>> {
        self.fields_info.get(field_name)
    }

    pub fn instance_field_descriptor(
        &self,
        instance_field_name_type: &str,
    ) -> Option<&TypeDescriptor> {
        if !self
            .instance_fields_template
            .contains_key(instance_field_name_type)
        {
            return None;
        }
        let field_info = self.fields_info.get(instance_field_name_type)?;

        Some(field_info.type_descriptor())
    }

    pub fn default_value_instance_fields(&self) -> IndexMap<FieldNameType, FieldValue> {
        self.instance_fields_template.clone()
    }

    pub fn get_field_offset(&self, fully_qualified_field_name: &str) -> Result<i64> {
        self.find_field_offset(fully_qualified_field_name)?
            .ok_or_else(|| {
                Error::new_execution(&format!(
                    "Failed to get offset by name {fully_qualified_field_name}"
                ))
            })
    }

    /// Like [`Self::get_field_offset`], but yields `None` when there is no such instance field.
    pub fn find_field_offset(&self, fully_qualified_field_name: &str) -> Result<Option<i64>> {
        let offset = self
            .fields_offset_mapping()?
            .get_index_of(fully_qualified_field_name);
        Ok(offset.map(|offset| offset as i64 * FIELD_OFFSET_SCALE))
    }

    pub fn get_static_field_offset(&self, field_name: &str) -> Result<i64> {
        let offset = self.static_fields.get_index_of(field_name).ok_or_else(|| {
            Error::new_execution(&format!(
                "Failed to get static field offset by name {field_name}"
            ))
        })?;
        let real_offset = STATIC_FIELDS_START + offset as i64;
        Ok(real_offset)
    }

    pub fn get_static_field_info_by_offset(&self, offset: i64) -> Result<&Arc<FieldInfo>> {
        let field = self
            .static_fields
            .get_index((offset - STATIC_FIELDS_START) as usize)
            .and_then(|(name, _)| self.fields_info.get(name));
        field.ok_or_else(|| Error::new_execution(&format!("Invalid static field offset {offset}")))
    }

    pub fn get_static_field_by_offset(&self, offset: i64) -> Result<Arc<FieldValue>> {
        let real_offset = offset - STATIC_FIELDS_START; // adjust offset to match the index in static_fields
        let (_field_name, field_value) = self
            .static_fields
            .get_index(real_offset as usize)
            .ok_or_else(|| {
                Error::new_execution(&format!("Failed to get static field by offset {offset}"))
            })?;
        Ok(Arc::clone(field_value))
    }

    /// The declared type of the static field at `offset`.
    ///
    /// [`Self::get_static_field_by_offset`] yields only the value, which is raw `i32` chunks with
    /// nothing to say whether they are a reference. Callers that put such a value on an operand
    /// stack need this to tag it.
    pub fn get_static_field_descriptor_by_offset(&self, offset: i64) -> Result<&TypeDescriptor> {
        let real_offset = offset - STATIC_FIELDS_START;
        let (field_name, _) = self
            .static_fields
            .get_index(real_offset as usize)
            .ok_or_else(|| {
                Error::new_execution(&format!("Failed to get static field by offset {offset}"))
            })?;

        self.fields_info
            .get(field_name)
            .map(|field_info| field_info.type_descriptor())
            .ok_or_else(|| {
                Error::new_execution(&format!(
                    "Failed to get field info for static field {}.{field_name}",
                    self.this_class_name
                ))
            })
    }

    pub fn get_field_name_by_offset(&self, offset: i64) -> Result<(String, String)> {
        // `offset` is a 4-byte-scaled slot index (see [`FIELD_OFFSET_SCALE`]); a sub-word Unsafe op
        // may pass a word-aligned offset (`offset & ~3`), which still divides back to the right slot.
        let result = self
            .fields_offset_mapping()?
            .get_index((offset / FIELD_OFFSET_SCALE) as usize)
            .ok_or_else(|| {
                Error::new_execution(&format!(
                    "Failed to get field name by offset {offset} from fields_offset_mapping"
                ))
            })?;

        let mut parts = result.split('.');
        let class_name = parts.next().ok_or_else(|| {
            Error::new_execution(&format!(
                "Failed to get class name by offset {offset} after splitting"
            ))
        })?;
        let field_name = parts.next().ok_or_else(|| {
            Error::new_execution(&format!(
                "Failed to get field name by offset {offset} after splitting"
            ))
        })?;

        Ok((class_name.to_string(), field_name.to_string()))
    }

    pub fn get_method_full(&self, full_signature: &str) -> Option<(usize, Arc<JavaMethod>)> {
        self.methods
            .get_full(full_signature)
            .map(|(index, _key, method)| (index, Arc::clone(method)))
            .or_else(|| {
                // we have not found the method by full signature, let's treat it as polymorphic signature method and try to find it by method name only
                self.methods
                    .get_full(full_signature.split(':').next()?)
                    .map(|(index, _key, method)| (index, Arc::clone(method)))
            })
    }

    pub fn try_get_method(&self, full_signature: &str) -> Option<Arc<JavaMethod>> {
        self.get_method_full(full_signature)
            .map(|(_index, method)| Arc::clone(&method))
    }

    pub fn get_method(&self, full_signature: &str) -> Result<Arc<JavaMethod>> {
        self.get_method_full(full_signature)
            .map(|(_index, method)| Arc::clone(&method))
            .ok_or_else(|| {
                Error::new_execution(&format!(
                    "Method {full_signature} not found in {}",
                    self.this_class_name
                ))
            })
    }

    pub fn get_method_by_index(&self, method_index: i64) -> Result<Arc<JavaMethod>> {
        self.methods
            .get_index(method_index as usize)
            .map(|(_key, method)| Arc::clone(method))
            .ok_or_else(|| {
                Error::new_execution(&format!("Failed to get method by index {method_index}"))
            })
    }

    pub fn get_fields_info(&self) -> Vec<Arc<FieldInfo>> {
        self.fields_info
            .values()
            .map(Arc::clone)
            .collect::<Vec<_>>()
    }

    pub fn get_methods(&self) -> impl Iterator<Item = (&String, &Arc<JavaMethod>)> {
        self.methods.iter()
    }

    pub fn instance_fields_hierarchy(
        &self,
    ) -> Result<&Arc<IndexMap<ClassName, IndexMap<FieldNameType, FieldValue>>>> {
        if self.instance_fields_hierarchy.get().is_none() {
            let mut instance_fields_hierarchy = IndexMap::new();
            lookup::lookup_and_fill_instance_fields_hierarchy(
                &self.this_class_name,
                &mut instance_fields_hierarchy,
            )?;
            // Construction may load classes. Only publication occurs under the cell lock.
            let _ = self
                .instance_fields_hierarchy
                .set(Arc::new(instance_fields_hierarchy));
        }
        Ok(self
            .instance_fields_hierarchy
            .get()
            .expect("published field hierarchy"))
    }

    fn fields_offset_mapping(&self) -> Result<&IndexSet<FullyQualifiedFieldName>> {
        if self.fields_offset_mapping.get().is_none() {
            let mut fields_offset_mapping = IndexSet::new();
            let hierarchy = self.instance_fields_hierarchy()?;

            hierarchy.iter().for_each(|(class_name, fields)| {
                fields.iter().for_each(|(field_name, _)| {
                    fields_offset_mapping.insert(format!("{class_name}.{field_name}"));
                });
            });

            let _ = self.fields_offset_mapping.set(fields_offset_mapping);
        }
        Ok(self
            .fields_offset_mapping
            .get()
            .expect("published field offsets"))
    }

    pub fn vtable(&self) -> Result<&IndexMap<String, Arc<JavaMethod>>> {
        if self.vtable.get().is_none() {
            let vtable = lookup::build_vtable(self.this_class_name())?;
            let _ = self.vtable.set(vtable);
        }
        Ok(self.vtable.get().expect("published vtable"))
    }

    pub fn inject_mirror_clazz_ref(&self, mirror_class_ref: i32) -> Result<()> {
        self.mirror_clazz_ref.set(mirror_class_ref).map_err(|e| {
            Error::new_execution(&format!(
                "{}: attempt to re-initialize mirror_clazz_ref {} -> {}",
                self.this_class_name, e, mirror_class_ref
            ))
        })
    }

    pub fn mirror_clazz_ref(&self) -> Result<i32> {
        self.mirror_clazz_ref.get().copied().ok_or_else(|| {
            Error::new_execution(&format!(
                "{}: mirror_clazz_ref is not initialized",
                self.this_class_name
            ))
        })
    }
}
