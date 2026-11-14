use crate::vm::assertions::java_assertions;
use crate::vm::error::{Error, Result};
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::exception::pending_helpers::set_pending_class_not_found_exception;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::static_init::StaticInit;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{clazz_ref, klass};
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::instance_checker::InstanceChecker;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::namespaces;
use crate::vm::method_area::primitives_helper::{PRIMITIVE_CODE_BY_TYPE, PRIMITIVE_TYPE_BY_CODE};
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::system_native::constant_pool::CONSTANT_POOL_OOP;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use jclassmodel::modifiers::ClassModifier;

const PUBLIC: u16 = 0x00000001;

/// `java.lang.Class.getSuperclass()Ljava/lang/Class;`
pub(crate) fn get_superclass(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let parent = if !klass.is_interface() {
        klass.parent().clone()
    } else {
        None
    };

    parent
        .map(|parent_name| clazz_ref(&parent_name))
        .unwrap_or(Ok(0))
}

/// `java.lang.Class.getPrimitiveClass(Ljava/lang/String;)Ljava/lang/Class;`
pub(crate) fn get_primitive_class(string_ref: i32) -> Result<i32> {
    let string_content = get_utf8_string_by_ref(string_ref)?;
    let mapped_class_name = map_primitive_class(&string_content)?;

    clazz_ref(mapped_class_name)
}
fn map_primitive_class(primitive_type: &str) -> Result<&str> {
    let matched = PRIMITIVE_CODE_BY_TYPE.get(primitive_type).ok_or_else(|| {
        Error::new_execution(&format!("Unsupported primitive type: {primitive_type}"))
    })?;

    Ok(matched)
}

/// `java.lang.Class.desiredAssertionStatus0(Ljava/lang/Class)Z`
///
/// Whether the `-ea`/`-da`/`-esa`/`-dsa` options enable assertions for the class, which counts as
/// a system class when the bootstrap class loader loaded it. Never for primitive and array classes.
pub(crate) fn desired_assertion_status0(class_ref: i32) -> Result<bool> {
    let klass = klass(class_ref)?;
    let name = klass.binary_name();
    if name.starts_with('[') || PRIMITIVE_TYPE_BY_CODE.contains_key(name) {
        return Ok(false);
    }
    let class_loader_ref =
        HEAP.get_object_field_value(class_ref, "java/lang/Class", "classLoader")?[0];
    Ok(java_assertions().enabled(name, class_loader_ref == 0))
}

/// `java.lang.Class.initClassName()Ljava/lang/String;`
///
/// Returns the class name for the given class reference.
/// Updates Class.name field in the heap.
pub(crate) fn init_class_name(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let class_name = klass.external_name();
    let string_ref = StringPoolHelper::get_string(class_name)?;

    HEAP.set_object_field_value(class_ref, "java/lang/Class", "name", vec![string_ref])?;

    Ok(string_ref)
}

/// `java.lang.Class.forName0(Ljava/lang/String;ZLjava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/Class;`
/// (JDK 25)
pub(crate) fn for_name0_with_caller(
    name_ref: i32,
    initialize: bool,
    loader_ref: i32,
    _caller_ref: i32,
) -> Result<i32> {
    for_name0(name_ref, initialize, loader_ref)
}

/// `java.lang.Class.forName0(Ljava/lang/String;ZLjava/lang/ClassLoader;)Ljava/lang/Class;` (JDK 26+)
pub(crate) fn for_name0(name_ref: i32, initialize: bool, loader_ref: i32) -> Result<i32> {
    let name = get_utf8_string_by_ref(name_ref)?;
    // A binary name never contains '/': that spelling belongs to hidden classes, which no
    // loader can find by name.
    if name.contains('/') {
        set_pending_class_not_found_exception(&name)?;
        return Ok(0);
    }
    let internal_name = name.replace('.', "/");
    // The class as `loader_ref` resolves the name (the bootstrap loader for null), arrays included.
    let loaded = match namespaces::load_with_loader(loader_ref, &internal_name) {
        Ok(loaded) => loaded,
        Err(error) => {
            // A failure of the loader other than "not found" (say a LinkageError) is the caller's
            // to see; everything else is reported as HotSpot does, by name.
            if let Some(throwable_ref) = error.throwable_ref() {
                let not_found = HEAP.get_instance_name(throwable_ref).and_then(|thrown| {
                    InstanceChecker::checkcast(&thrown, "java/lang/ClassNotFoundException")
                });
                if !not_found.unwrap_or(true) {
                    return Err(error);
                }
            }
            // fixme: samples.reflection.trivial.forname.NonExisting prints stacktrace here
            // https://github.com/hextriclosan/rusty-jvm/issues/676
            // HotSpot's bootstrap loader reports the name in its internal form.
            let reported = if loader_ref == 0 {
                &internal_name
            } else {
                &name
            };
            set_pending_class_not_found_exception(reported)?;
            return Ok(0);
        }
    };
    let reflection_ref = RootedSlots::from_slots(vec![Slot::Ref(loaded.mirror_clazz_ref()?)]);

    if initialize {
        StaticInit::initialize_java_class(&loaded)?;
    }

    Ok(reflection_ref[0].value())
}

/// `java.lang.Class.registerNatives()V`
pub(crate) fn register_natives() -> Result<()> {
    // todo implement me
    Ok(())
}

/// `java.lang.Class.getInterfaces0()[Ljava/lang/Class;`
pub(crate) fn get_interfaces0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let interfaces = klass.interfaces();

    let interface_refs = interfaces
        .iter()
        .map(|interface| clazz_ref(interface))
        .collect::<Result<Vec<i32>>>()?;

    HEAP.create_array_with_values(
        "[Ljava/lang/Class;",
        &interface_refs,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getPermittedSubclasses0()[Ljava/lang/Class;`
pub(crate) fn get_permitted_subclasses0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let Some(subclasses) = klass
        .attributes()
        .get_permitted_subclasses(klass.constant_pool())?
    else {
        // not sealed
        return Ok(0);
    };

    // As in HotSpot, a permitted subclass that fails to load is left out rather than failing the
    // call, and so is one that names an array class
    let subclass_refs = subclasses
        .iter()
        .filter(|subclass| !subclass.starts_with('['))
        .filter_map(|subclass| resolved_clazz_ref(&klass, subclass).ok())
        .collect::<Vec<_>>();

    HEAP.create_array_with_values(
        "[Ljava/lang/Class;",
        &subclass_refs,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getDeclaringClass0()Ljava/lang/Class;`
pub(crate) fn get_declaring_class0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let Some(declaring_class) = klass.declaring_class() else {
        return Ok(0);
    };
    let outer = CLASSES.get(&klass.resolve_class_name(declaring_class)?)?;
    // HotSpot's `Reflection::check_for_inner_class`: the outer class must list this very class
    // as a member, which fails when the two come from different loaders.
    if klass.defining_loader().is_some() || outer.defining_loader().is_some() {
        let lists_inner = outer
            .attributes()
            .get_declared_classes(outer.constant_pool(), outer.this_class_name())
            .iter()
            .any(|inner| {
                outer
                    .resolve_class_name(inner)
                    .is_ok_and(|key| key == klass.this_class_name().as_str())
            });
        if !lists_inner {
            return Err(java_exception(
                "java/lang/IncompatibleClassChangeError",
                &format!(
                    "{} and {} disagree on InnerClasses attribute",
                    outer.external_name(),
                    klass.external_name()
                ),
            ));
        }
    }
    outer.mirror_clazz_ref()
}

/// `java.lang.Class.getDeclaredFields0(Z)[Ljava/lang/reflect/Field;`
pub(crate) fn get_declared_fields0(class_ref: i32, public_only: bool) -> Result<i32> {
    let klass = klass(class_ref)?;

    let fields_info = klass.get_fields_info();
    let fields_info_ref = fields_info
        .iter()
        .filter_map(|field_info| {
            if public_only && (field_info.flags() & PUBLIC) == 0 {
                return None; // Skip non-public fields
            }

            Some(field_info.reflection_ref())
        })
        .collect::<Result<Vec<_>>>()?;

    HEAP.create_array_with_values(
        "[Ljava/lang/reflect/Field;",
        &fields_info_ref,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getDeclaredMethods0(Z)[Ljava/lang/reflect/Method;`
pub(crate) fn get_declared_methods0(class_ref: i32, public_only: bool) -> Result<i32> {
    let klass = klass(class_ref)?;

    let method_refs = klass
        .get_methods()
        .filter_map(|(_, java_method)| {
            if (java_method.name() == "<init>") || (java_method.name() == "<clinit>") {
                return None; // Skip constructors and static initializers
            }

            if public_only && (java_method.access_flags() as u16 & PUBLIC) == 0 {
                return None; // Skip non-public methods
            }

            Some(java_method.reflection_ref())
        })
        .collect::<Result<Vec<_>>>()?;

    HEAP.create_array_with_values(
        "[Ljava/lang/reflect/Method;",
        &method_refs,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getDeclaredConstructors0(Z)[Ljava/lang/reflect/Constructor;`
pub(crate) fn get_declared_constructors0(class_ref: i32, public_only: bool) -> Result<i32> {
    let klass = klass(class_ref)?;
    let method_refs = klass
        .get_methods()
        .filter(|(_, java_method)| java_method.name() == "<init>")
        .filter(|(_, java_method)| {
            !public_only || (java_method.access_flags() as u16 & PUBLIC) != 0
        })
        .map(|(_, java_method)| java_method.reflection_ref())
        .collect::<Result<Vec<_>>>()?;

    HEAP.create_array_with_values(
        "[Ljava/lang/reflect/Constructor;",
        &method_refs,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getEnclosingMethod0()[Ljava/lang/Object;`
pub(crate) fn get_enclosing_method0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let Some(enclosing_method) = klass.enclosing_method() else {
        return Ok(0);
    };

    let class_name_ref = resolved_clazz_ref(&klass, &enclosing_method.class_name)?;

    // JVMS §4.7.7 allows the attribute to name an enclosing class without an enclosing method,
    // for a class declared in an initializer. `Class.EnclosingMethodInfo` accepts a null name and
    // descriptor (they must be null together), and `getEnclosingClass()` still reads element 0, so
    // the enclosing class has to be reported even when there is no method to go with it.
    let (name_ref, descriptor_ref) = match &enclosing_method.method {
        Some(method) => (
            StringPoolHelper::get_string(&method.name)?,
            StringPoolHelper::get_string(&method.descriptor)?,
        ),
        None => (0, 0),
    };

    let array_ref = HEAP.create_array_with_values(
        "[Ljava/lang/reflect/Method;",
        &[class_name_ref, name_ref, descriptor_ref],
        &mut RootedSlots::new(),
    )?;
    Ok(array_ref) // new Object[] {class_name, name, descriptor}
}

pub(crate) fn get_generic_signature0(class_ref: i32) -> Result<i32> {
    let class = klass(class_ref)?;
    class
        .attributes()
        .get_signature(class.constant_pool())?
        .as_deref()
        .map(StringPoolHelper::get_string)
        .transpose()
        .map(|signature| signature.unwrap_or(0))
}

/// `java.lang.Class.getRawAnnotations()[B`
pub(crate) fn get_raw_annotations(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let annotations_raw = klass.annotations_raw().to_owned();
    let annotations_ref = match annotations_raw {
        Some(raw) => HEAP.create_byte_array(&raw, &mut RootedSlots::new())?,
        None => 0,
    };

    Ok(annotations_ref)
}

/// `java.lang.Class.getSimpleBinaryName0()Ljava/lang/String;`
pub(crate) fn get_simple_binary_name0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;

    let string_ref = klass
        .binary_name()
        .rsplit_once('$')
        .map(|(_, last_token)| last_token)
        .filter(|s| s.parse::<u64>().is_err())
        .map(StringPoolHelper::get_string)
        .unwrap_or(Ok(0));
    string_ref
}

/// `java.lang.Class.isAssignableFrom(Ljava/lang/Class;)Z`
pub(crate) fn is_assignable_from(
    assignable_class_ref: i32,
    assignee_class_ref: i32,
) -> Result<bool> {
    let assignable = klass(assignable_class_ref)?;
    let assignee = klass(assignee_class_ref)?;

    InstanceChecker::checkcast(assignee.this_class_name(), assignable.this_class_name())
}

/// `java.lang.Class.isHidden()Z`
pub(crate) fn is_hidden(class_ref: i32) -> Result<bool> {
    Ok(klass(class_ref)?.hidden())
}

/// The mirror of the class a symbolic `name` from `referrer`'s class file denotes, resolved
/// through `referrer`'s defining loader.
fn resolved_clazz_ref(
    referrer: &crate::vm::method_area::java_class::JavaClass,
    name: &str,
) -> Result<i32> {
    clazz_ref(&referrer.resolve_class_name(name)?)
}

/// `java.lang.Class.isInstance(Ljava/lang/Object;)Z`
pub(crate) fn class_is_instance(this_class_ref: i32, obj_ref_to_check: i32) -> Result<bool> {
    if obj_ref_to_check == 0 {
        return Ok(false);
    }

    let klass = klass(this_class_ref)?;
    let class_name_to_check = HEAP.get_instance_name(obj_ref_to_check)?;

    InstanceChecker::checkcast(&class_name_to_check, klass.this_class_name())
}

/// `java.lang.Class.getConstantPool()Ljdk/internal/reflect/ConstantPool;`
pub(crate) fn get_constant_pool(clazz_ref: i32) -> Result<i32> {
    const NAME: &str = "jdk/internal/reflect/ConstantPool";
    let constant_pool_ref = Executor::invoke_default_constructor(NAME)?;
    HEAP.set_object_field_value(
        constant_pool_ref[0].value(),
        NAME,
        CONSTANT_POOL_OOP,
        vec![clazz_ref],
    )?;

    Ok(constant_pool_ref[0].value())
}

/// `java.lang.Class.getNestHost0()Ljava/lang/Class;`
pub(crate) fn get_nest_host0(class_ref: i32) -> Result<i32> {
    clazz_ref(klass(class_ref)?.nest_host())
}

/// `java.lang.Class.isRecord0()Z`
pub(crate) fn is_record0(clazz_ref: i32) -> Result<bool> {
    let klass = klass(clazz_ref)?;
    let record = klass.class_modifiers().contains(ClassModifier::Final)
        && klass
            .parent()
            .as_ref()
            .is_some_and(|p| p == "java/lang/Record");
    Ok(record)
}

/// `java.lang.Class.getRecordComponents0()[Ljava/lang/reflect/RecordComponent;`
///
/// Builds one `RecordComponent` per `Record` attribute entry, filling the same fields HotSpot's
/// `java_lang_reflect_RecordComponent::create` does. Returns null for a non-record class.
pub(crate) fn get_record_components0(class_ref: i32) -> Result<i32> {
    const RC: &str = "java/lang/reflect/RecordComponent";
    let klass = klass(class_ref)?;
    let Some(components) = klass
        .attributes()
        .get_record_components(klass.constant_pool())?
    else {
        return Ok(0);
    };

    let mut roots = JavaThread::hold_temp_roots(vec![Slot::Ref(class_ref)]);
    let mut component_refs = Vec::with_capacity(components.len());
    for component in &components {
        let rc_ref = Executor::invoke_default_constructor(RC)?[0].value();
        roots.add_root(Slot::Ref(rc_ref));

        let name_ref = StringPoolHelper::get_string(&component.name)?;
        let type_ref = resolved_clazz_ref(&klass, &component.descriptor)?;
        let accessor_ref = match klass
            .try_get_method(&format!("{}:(){}", component.name, component.descriptor))
        {
            Some(method) => method.reflection_ref()?,
            None => 0,
        };
        let signature_ref = match component.signature.as_deref() {
            Some(signature) => StringPoolHelper::get_string(signature)?,
            None => 0,
        };
        let annotations_ref = match component.annotations_raw.as_deref() {
            Some(raw) => HEAP.create_byte_array(raw, &mut RootedSlots::new())?,
            None => 0,
        };

        HEAP.set_object_field_value(rc_ref, RC, "clazz", vec![class_ref])?;
        HEAP.set_object_field_value(rc_ref, RC, "name", vec![name_ref])?;
        HEAP.set_object_field_value(rc_ref, RC, "type", vec![type_ref])?;
        HEAP.set_object_field_value(rc_ref, RC, "accessor", vec![accessor_ref])?;
        HEAP.set_object_field_value(rc_ref, RC, "signature", vec![signature_ref])?;
        HEAP.set_object_field_value(rc_ref, RC, "annotations", vec![annotations_ref])?;
        component_refs.push(rc_ref);
    }

    HEAP.create_array_with_values(
        "[Ljava/lang/reflect/RecordComponent;",
        &component_refs,
        &mut RootedSlots::new(),
    )
}

/// `java.lang.Class.getNestMembers0()[Ljava/lang/Class;`
///
/// The nest host first, then every member it lists (HotSpot `JVM_GetNestMembers`).
pub(crate) fn get_nest_members0(class_ref: i32) -> Result<i32> {
    let host_ref = get_nest_host0(class_ref)?;
    let host = klass(host_ref)?;
    let mut refs = vec![host_ref];
    if let Some(members) = host.attributes().get_nest_members(host.constant_pool())? {
        // As in HotSpot, a listed member counts only if it loads and names this host back.
        for member in members {
            let Ok(key) = host.resolve_class_name(&member) else {
                continue;
            };
            let Ok(member_class) = CLASSES.get(&key) else {
                continue;
            };
            if member_class.nest_host() == host.this_class_name() {
                refs.push(member_class.mirror_clazz_ref()?);
            }
        }
    }
    HEAP.create_array_with_values("[Ljava/lang/Class;", &refs, &mut RootedSlots::new())
}

/// `java.lang.Class.getDeclaredClasses0()[Ljava/lang/Class;`
pub(crate) fn get_declared_classes0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    let refs = klass
        .attributes()
        .get_declared_classes(klass.constant_pool(), klass.this_class_name())
        .iter()
        .map(|name| resolved_clazz_ref(&klass, name))
        .collect::<Result<Vec<_>>>()?;
    HEAP.create_array_with_values("[Ljava/lang/Class;", &refs, &mut RootedSlots::new())
}

/// `java.lang.Class.getClassAccessFlagsRaw0()I`: the class file's own `access_flags`.
pub(crate) fn get_class_access_flags_raw0(class_ref: i32) -> Result<i32> {
    let klass = klass(class_ref)?;
    Ok(klass.class_modifiers().bits() as i32)
}

/// `java.lang.Class.getClassFileVersion0()I`: `minor << 16 | major`. Primitive classes (no class
/// file) report the newest version this VM supports, as HotSpot does.
pub(crate) fn get_class_file_version0(class_ref: i32) -> Result<i32> {
    const LATEST_MAJOR: i32 = 69; // Java 25
    let klass = klass(class_ref)?;
    let (major, minor) = klass.class_file_version();
    if major == 0 {
        return Ok(LATEST_MAJOR);
    }
    Ok(((minor as i32) << 16) | major as i32)
}

/// `java.lang.Class.getRawTypeAnnotations()[B`. Type annotations are not retained by the class
/// parser, so this reports none (null), which `TypeAnnotationParser` treats as "no annotations".
pub(crate) fn get_raw_type_annotations(_class_ref: i32) -> Result<i32> {
    Ok(0)
}
