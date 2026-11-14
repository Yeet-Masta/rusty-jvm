use crate::vm::error::{Error, Result};
use crate::vm::exception::class_cast::class_in_module_of_loader;
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::ldc_resolution_manager::LdcConstant;
use crate::vm::execution_engine::ldc_resolution_manager::LdcResolutionManager;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::construction::InstancePlan;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::module_helper::Modules;
use crate::vm::method_area::namespaces::{self, DefiningLoader};
use crate::vm::method_area::primitives_helper::PRIMITIVE_TYPE_BY_CODE;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::{JAVA_HOME, SYSTEM_CLASSLOADER_REF};
use indexmap::IndexSet;
use jclassmodel::parse;
use jimage_rs::jimage::JImage;
use jimage_rs::raw_jimage::RawJImage;
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::trace;

static METHOD_AREA: OnceCell<MethodArea> = OnceCell::new();

/// What makes a definition a hidden class (`Lookup.defineHiddenClass`).
pub(crate) struct HiddenDefinition {
    /// Makes the hidden class's name unique.
    pub(crate) id: u32,
    /// The class the defining `Lookup` was created for; the hidden class joins its module.
    pub(crate) lookup_class: Arc<JavaClass>,
    /// `ClassOption.NESTMATE`: the hidden class joins the lookup class's nest.
    pub(crate) nestmate: bool,
}

/// HotSpot's `LinkageError` for a loader defining a name it already has a class for.
fn duplicate_definition(existing: &JavaClass) -> Error {
    let name = existing.external_name();
    let message = match class_in_module_of_loader(existing) {
        Ok((loader, location)) => {
            format!(
                "loader {loader} attempted duplicate class definition for {name}. ({location})"
            )
        }
        Err(_) => format!("attempted duplicate class definition for {name}."),
    };
    java_exception("java/lang/LinkageError", &message)
}

/// Early bootstrap may not have installed the area. Reading roots must not initialize it.
pub(crate) fn global_root_refs() -> Result<Vec<i32>> {
    match METHOD_AREA.get() {
        Some(area) => area.root_refs(),
        None => Ok(Vec::new()),
    }
}

/// [`with_method_area`], or `None` before the method area exists (VM bootstrap has not begun).
pub(crate) fn try_with_method_area<R>(f: impl FnOnce(&MethodArea) -> R) -> Option<R> {
    METHOD_AREA.get().map(f)
}

pub(crate) fn with_method_area<F, R>(f: F) -> R
where
    F: FnOnce(&MethodArea) -> R,
{
    let method_area = METHOD_AREA.get().expect("error getting method area");

    f(method_area)
}

#[derive(Debug)]
pub(crate) struct MethodArea {
    jimage: JImage,
    modules_mapping: HashMap<String, String>,
    modules: Arc<Modules>,
    ldc_resolution_manager: LdcResolutionManager,
    system_thread_id: OnceCell<i32>, // main thread, spawned by VM
    system_thread_group_id: OnceCell<i32>, // root "system" thread group, created by VM
    main_thread_group_id: OnceCell<i32>, // "main" thread group (child of system), holds the main thread
}

impl MethodArea {
    fn root_refs(&self) -> Result<Vec<i32>> {
        let mut roots = self.modules.root_refs()?;
        roots.extend(self.ldc_resolution_manager.cached_refs()?);
        roots.extend(self.system_thread_id.get().copied());
        roots.extend(self.system_thread_group_id.get().copied());
        roots.extend(self.main_thread_group_id.get().copied());
        Ok(roots)
    }

    pub(crate) fn init() -> Result<()> {
        METHOD_AREA
            .set(MethodArea::new()?)
            .map_err(|_| Error::new_execution("MethodArea already initialized"))
    }

    fn new() -> Result<Self> {
        let java_home = JAVA_HOME.get().ok_or_else(|| {
            Error::new_execution("JAVA_HOME not set, cannot initialize MethodArea")
        })?;
        let modules = java_home.join("lib").join("modules");
        let jimage = JImage::open(modules)?;
        let modules_mapping = jimage
            .resource_names_iter()
            .map(|result| result.map_err(Error::from))
            .map(|result| result.map(|r| r.get_full_name()))
            .map(|result| result.map(|(module, name)| (name, module)))
            .collect::<Result<HashMap<_, _>>>()?;

        Ok(Self {
            jimage,
            modules_mapping,
            modules: Arc::new(Modules::new()),
            ldc_resolution_manager: LdcResolutionManager::default(),
            system_thread_id: OnceCell::new(),
            system_thread_group_id: OnceCell::new(),
            main_thread_group_id: OnceCell::new(),
        })
    }

    /// Defines a class from `bytecode` on behalf of `class_loader_ref` (`ClassLoader.defineClass`,
    /// `Lookup.defineClass`/`defineHiddenClass`, JNI `DefineClass`).
    ///
    /// `expected_name` is the internal name the caller asked for, when it gave one; the class
    /// file must record that name. A class of a built-in loader is registered under its plain
    /// name, a class of any other loader under a key private to that loader (see
    /// [`namespaces`]), and a hidden class under a unique name no loader can look up.
    pub(crate) fn define_class(
        &self,
        expected_name: Option<&str>,
        bytecode: &[u8],
        class_loader_ref: i32,
        hidden: Option<HiddenDefinition>,
    ) -> Result<Arc<JavaClass>> {
        let parsed = parse(bytecode)?;
        let recorded = parsed
            .this_class_name()
            .ok_or_else(|| Error::new_constant_pool("Error getting class_name of parsed class"))?;
        if let Some(expected) = expected_name.filter(|expected| *expected != recorded) {
            return Err(java_exception(
                "java/lang/NoClassDefFoundError",
                &format!("{expected} (wrong name: {recorded})"),
            ));
        }
        let serial = namespaces::namespace_of(class_loader_ref)?;
        let defining_loader = (serial != 0).then_some(DefiningLoader {
            serial,
            loader_ref: class_loader_ref,
        });
        let dotted = recorded.replace('/', ".");

        let (internal, external) = match &hidden {
            Some(hidden) => {
                let suffix = format!("0x{:016X}", hidden.id);
                (format!("{recorded}/{suffix}"), format!("{dotted}/{suffix}"))
            }
            None if serial == 0 => {
                if CLASSES.is_loaded(&recorded) {
                    let existing = CLASSES.get(&recorded)?;
                    if class_loader_ref != 0
                        && namespaces::mirror_loader(&existing)? == class_loader_ref
                    {
                        return Err(duplicate_definition(&existing));
                    }
                    // The VM itself may have loaded the class for this name already; the
                    // built-in namespace holds one class per name.
                    return Ok(existing);
                }
                (recorded.clone(), dotted)
            }
            None => {
                if let Some(key) = namespaces::recorded(serial, &recorded) {
                    return Err(duplicate_definition(&*CLASSES.get(&key)?));
                }
                (namespaces::key_for(&recorded, serial), dotted)
            }
        };

        let mut model = parsed.into_model(internal.clone(), external)?;
        if let Some(loader) = defining_loader {
            // Supertypes are resolved through the defining loader, so the hierarchy is stored as
            // registry keys like every other class's.
            let resolve = |name: &str| -> Result<String> {
                Ok(namespaces::load_with_loader(loader.loader_ref, name)?
                    .this_class_name()
                    .clone())
            };
            if let Some(super_class) = model.super_class_name.as_deref() {
                model.super_class_name = Some(resolve(super_class)?);
            }
            model.interfaces = model
                .interfaces
                .iter()
                .map(|interface| resolve(interface))
                .collect::<Result<_>>()?;
        }

        let nest_host = match &hidden {
            Some(hidden) if hidden.nestmate => Some(hidden.lookup_class.nest_host().to_string()),
            Some(_) => Some(internal.clone()),
            None => None,
        };
        let module_ref = match &hidden {
            Some(hidden) => Some(
                HEAP.get_object_field_value(
                    hidden.lookup_class.mirror_clazz_ref()?,
                    "java/lang/Class",
                    "module",
                )?[0],
            ),
            None if serial != 0 => Some(
                Executor::invoke_non_static_method(
                    namespaces::CLASS_LOADER,
                    "getUnnamedModule:()Ljava/lang/Module;",
                    class_loader_ref,
                    &[],
                )?[0]
                    .value(),
            ),
            None => None,
        }
        .filter(|module_ref| *module_ref != 0);

        let mut java_class = JavaClass::from_model(model)?;
        java_class.set_definition(defining_loader, hidden.is_some(), nest_host);
        let (_, _, published) =
            CLASSES.insert_defined(Arc::new(java_class), Some(class_loader_ref), module_ref)?;
        if hidden.is_none() && serial != 0 && !namespaces::record(serial, &recorded, &internal) {
            return Err(duplicate_definition(&published));
        }
        trace!("<META CLASS LOADED> -> {}", published.this_class_name());

        Ok(published)
    }

    pub(crate) fn load_class_file(
        &self,
        fully_qualified_class_name: &str,
    ) -> Result<Arc<JavaClass>> {
        let class_file_path = format!("{fully_qualified_class_name}.class");
        if let Some(module) = self.modules_mapping.get(&class_file_path) {
            let defined_by_java_loader = crate::vm::BOOT_MODULES
                .get()
                .is_some_and(|boot| !boot.contains(module.as_str()))
                && SYSTEM_CLASSLOADER_REF.get().is_some();
            if defined_by_java_loader {
                return Self::load_through_system_loader(fully_qualified_class_name);
            }
            let resource_path = format!("/{module}/{class_file_path}");
            if let Some(res) = self
                .jimage
                .find_resource(&resource_path)
                .map_err(|jimage_error| Error::new_execution(&jimage_error.to_string()))?
            {
                match self.try_parse(&res) {
                    Ok(Some(java_class)) => return Ok(java_class),
                    Ok(None) => {}
                    Err(e) => return Err(e),
                };
            }
        }

        if class_file_path.starts_with("java/") {
            self.try_open_and_parse(&PathBuf::from(&class_file_path))?
                .ok_or_else(|| {
                    Error::new_execution(&format!("error opening class file {class_file_path}"))
                })
        } else {
            Self::load_through_system_loader(fully_qualified_class_name)
        }
    }

    /// `Class.forName(name, false, systemLoader)`: the system loader delegates to the platform
    /// loader for platform modules, so the class ends up defined by the loader that owns it.
    fn load_through_system_loader(fully_qualified_class_name: &str) -> Result<Arc<JavaClass>> {
        let external_name = fully_qualified_class_name.replace('/', ".");
        let name_ref = StringPoolHelper::get_string(&external_name)?;
        let clazz_ref = Executor::invoke_static_method(
            "java/lang/Class",
            "forName:(Ljava/lang/String;ZLjava/lang/ClassLoader;)Ljava/lang/Class;",
            &[
                StackValueKind::Ref(name_ref),
                0.into(),
                StackValueKind::Ref(SYSTEM_CLASSLOADER_REF.get().copied().unwrap_or(0)),
            ],
        )?;
        klass(clazz_ref[0].value())
    }

    fn try_open_and_parse(&self, path: &PathBuf) -> Result<Option<Arc<JavaClass>>> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None), // File not found is not considered as an error
            Err(e) => return Err(e.into()),
        };
        let mut buff = Vec::new();
        file.read_to_end(&mut buff)?;

        self.try_parse(&buff)
    }

    fn try_parse(&self, buff: &[u8]) -> Result<Option<Arc<JavaClass>>> {
        let parsed = parse(buff)?;
        let class_name = parsed
            .this_class_name()
            .ok_or_else(|| Error::new_constant_pool("Error getting class_name of parsed class"))?;

        let (internal, external) = derive_internal_and_external_names(&class_name);
        let model = parsed.into_model(internal, external)?;
        Ok(Some(Arc::new(JavaClass::from_model(model)?)))
    }

    pub fn create_instance_with_default_fields(&self, class_name: &str) -> Result<InstancePlan> {
        let (id, _key, klass) = CLASSES.get_full(class_name)?;
        klass.new_instance(id)
    }

    pub(crate) fn generate_synthetic_classes() -> Vec<Arc<JavaClass>> {
        PRIMITIVE_TYPE_BY_CODE
            .keys()
            .map(|class_name| Self::generate_synthetic_class(class_name))
            .collect()
    }

    fn generate_synthetic_class(class_name: &str) -> Arc<JavaClass> {
        let (internal, external) = derive_internal_and_external_names(class_name);
        Arc::new(JavaClass::synthetic(
            internal,
            external,
            None,
            IndexSet::new(),
        ))
    }

    /// Resolves an `ldc` constant to the slot it occupies on the operand stack.
    pub(crate) fn resolve_ldc(&self, current_class_name: &str, cpoolindex: u16) -> Result<Slot> {
        self.ldc_resolution_manager
            .resolve_ldc(current_class_name, cpoolindex)
            .map(|constant| constant.slot())
    }

    /// Resolves an `ldc` constant keeping its kind, for callers that must box a numeric constant
    /// rather than merely place it in a slot.
    pub(crate) fn resolve_ldc_constant(
        &self,
        current_class_name: &str,
        cpoolindex: u16,
    ) -> Result<LdcConstant> {
        self.ldc_resolution_manager
            .resolve_ldc(current_class_name, cpoolindex)
    }

    pub(crate) fn resolve_ldc2_w(&self, current_class_name: &str, cpoolindex: u16) -> Result<i64> {
        self.ldc_resolution_manager
            .resolve_ldc2_w(current_class_name, cpoolindex)
    }

    pub fn system_thread_id(&self) -> Result<i32> {
        self.system_thread_id
            .get()
            .copied()
            .ok_or_else(|| Error::new_execution("system_thread_id wasn't set"))
    }

    pub fn set_system_thread_id(&self, thread_id: i32) -> Result<()> {
        self.system_thread_id.set(thread_id).map_err(|_| {
            Error::new_execution("system_thread_id was already set, cannot be set again")
        })
    }

    pub fn set_system_thread_group_id(&self, thread_group_id: i32) -> Result<()> {
        self.system_thread_group_id
            .set(thread_group_id)
            .map_err(|_| {
                Error::new_execution("system_thread_group_id was already set, cannot be set again")
            })
    }

    /// The "main" thread group, once bootstrap has created it.
    pub(crate) fn main_thread_group_id(&self) -> Option<i32> {
        self.main_thread_group_id.get().copied()
    }

    pub fn set_main_thread_group_id(&self, thread_group_id: i32) -> Result<()> {
        self.main_thread_group_id.set(thread_group_id).map_err(|_| {
            Error::new_execution("main_thread_group_id was already set, cannot be set again")
        })
    }

    pub fn modules(&self) -> Arc<Modules> {
        Arc::clone(&self.modules)
    }

    pub fn jimage_raw(&self) -> RawJImage<'_> {
        self.jimage.raw()
    }

    pub fn modules_mapping(&self) -> &HashMap<String, String> {
        &self.modules_mapping
    }
}

/// Returns (internal_name, external_name)
///
/// Examples:
///     "I" -> ("I", "int")
///     "my/package/MyClass" -> ("my/package/MyClass", "my.package.MyClass")
fn derive_internal_and_external_names(raw: &str) -> (String, String) {
    if let Some(external_name) = PRIMITIVE_TYPE_BY_CODE.get(raw) {
        // Check if the raw name is a primitive type
        let internal = raw.to_string();
        let external = external_name.to_string();
        (internal, external)
    } else {
        // Just ordinary class name
        let internal = raw.to_string();
        let external = raw.replace('/', ".");
        (internal, external)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_roots_method_area_keeps_primordial_groups_and_injects_before_publication() {
        JAVA_HOME
            .set(PathBuf::from(std::env::var("JAVA_HOME").unwrap()))
            .unwrap();
        let area = MethodArea::new().unwrap();
        area.set_system_thread_id(61).unwrap();
        area.set_system_thread_group_id(62).unwrap();
        area.set_main_thread_group_id(63).unwrap();
        area.modules.registry().insert("java.base".into(), 64);

        let roots = area.root_refs().unwrap();

        assert_eq!(roots, vec![64, 61, 62, 63]);
        for (class, field, descriptor) in [
            ("java/lang/invoke/ResolvedMethodName", "vmtarget", "J"),
            (
                "jdk/internal/reflect/ConstantPool",
                crate::vm::system_native::constant_pool::CONSTANT_POOL_OOP,
                "Ljava/lang/Object;",
            ),
        ] {
            let parsed = area.load_class_file(class).unwrap();
            assert_eq!(
                parsed.instance_field_descriptor(field).unwrap().to_string(),
                descriptor
            );
            assert!(parsed.default_value_instance_fields().contains_key(field));
            assert_eq!(parsed.mirror_ref_if_present(), None);
        }
    }

    #[test]
    fn derive_internal_and_external_names_with_primitive_type() {
        let result = derive_internal_and_external_names("I");
        assert_eq!(result, ("I".to_string(), "int".to_string()));
    }

    #[test]
    fn derive_internal_and_external_names_without_synthetic_class_delim() {
        let result = derive_internal_and_external_names(
            "java/util/concurrent/ConcurrentHashMap$CollectionView",
        );
        assert_eq!(
            result,
            (
                "java/util/concurrent/ConcurrentHashMap$CollectionView".to_string(),
                "java.util.concurrent.ConcurrentHashMap$CollectionView".to_string()
            )
        );
    }

    #[test]
    fn derive_internal_and_external_names_with_arrays() {
        let result = derive_internal_and_external_names("[Ljava/lang/Class;");
        assert_eq!(
            result,
            (
                "[Ljava/lang/Class;".to_string(),
                "[Ljava.lang.Class;".to_string()
            )
        );
    }
}
