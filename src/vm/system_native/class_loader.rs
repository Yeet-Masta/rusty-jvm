use crate::vm::assertions::{java_assertions, AssertionOption};
use crate::vm::error::Result;
use crate::vm::execution_engine::static_init::StaticInit;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::{klass, vec_to_i64};
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::method_area::{with_method_area, HiddenDefinition};
use crate::vm::method_area::namespaces;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::trace;

/// `java.lang.invoke.MethodHandleNatives.Constants`-style flags of `ClassLoader.defineClass0`.
const NESTMATE_CLASS: i32 = 0x1;
const HIDDEN_CLASS: i32 = 0x2;

static COUNTER: AtomicU32 = AtomicU32::new(1);

/// `java/lang/ClassLoader:registerNatives:()V`
pub(crate) fn register_natives() -> Result<()> {
    Ok(()) // todo: implement me
}

const ASSERTION_STATUS_DIRECTIVES: &str = "java/lang/AssertionStatusDirectives";

/// `java/lang/ClassLoader:retrieveDirectives:()Ljava/lang/AssertionStatusDirectives;`
///
/// The `-ea`/`-da` options, which a class loader takes over once a program changes its assertion
/// status (e.g. `ClassLoader.setPackageAssertionStatus`). As in HotSpot, the options keep their
/// command-line order, so later ones override earlier ones, and names use dots.
pub(crate) fn retrieve_directives() -> Result<i32> {
    StaticInit::initialize(ASSERTION_STATUS_DIRECTIVES)?;
    let directives = with_method_area(|method_area| {
        method_area.create_instance_with_default_fields(ASSERTION_STATUS_DIRECTIVES)
    })?;
    let mut destination = RootedSlots::new();
    let directives_ref = HEAP.create_instance(directives, &mut destination)?;

    let assertions = java_assertions();
    let packages = to_java_arrays(assertions.packages())?;
    let classes = to_java_arrays(assertions.classes())?;
    let fields = [
        ("packages", packages[0].value()),
        ("packageEnabled", packages[1].value()),
        ("classes", classes[0].value()),
        ("classEnabled", classes[1].value()),
        ("deflt", assertions.user_default() as i32),
    ];
    for (name, value) in fields {
        HEAP.set_object_field_value(
            directives_ref,
            ASSERTION_STATUS_DIRECTIVES,
            name,
            vec![value],
        )?;
    }
    Ok(directives_ref)
}

/// The parallel `String[]` names and `boolean[]` statuses of `options`.
fn to_java_arrays(options: &[AssertionOption]) -> Result<RootedSlots> {
    let mut destination = RootedSlots::new();
    let names = options
        .iter()
        .map(|option| StringPoolHelper::get_string(&option.name.replace('/', ".")))
        .collect::<Result<Vec<_>>>()?;
    let enabled = options
        .iter()
        .map(|option| option.enabled as i32)
        .collect::<Vec<_>>();
    let names_ref =
        HEAP.create_array_with_values("[Ljava/lang/String;", &names, &mut destination)?;
    let mut enabled_root = RootedSlots::new();
    let enabled_ref = HEAP.create_array_with_values("[Z", &enabled, &mut enabled_root)?;
    destination.replace(vec![Slot::Ref(names_ref), Slot::Ref(enabled_ref)]);
    Ok(destination)
}

/// `java/lang/ClassLoader:defineClass0:(Ljava/lang/ClassLoader;Ljava/lang/Class;Ljava/lang/String;[BIILjava/security/ProtectionDomain;ZILjava/lang/Object;)Ljava/lang/Class;`
///
/// `Lookup.defineClass` and `Lookup.defineHiddenClass` (so every lambda proxy and compiled
/// `LambdaForm`). `name` is an internal name.
pub(crate) fn define_class0(
    class_loader_ref: i32,
    lookup_ref: i32,
    name_ref: i32,
    buf_ref: i32,
    off: i32,
    len: i32,
    _protection_domain_ref: i32,
    initialize: bool,
    flags: i32,
    class_data_ref: i32,
) -> Result<i32> {
    let byte_code = array_bytes(buf_ref, off, len)?;
    let hidden = if flags & HIDDEN_CLASS != 0 {
        Some(HiddenDefinition {
            id: increment_counter(),
            lookup_class: klass(lookup_ref)?,
            nestmate: flags & NESTMATE_CLASS != 0,
        })
    } else {
        None
    };
    let expected_name = match (&hidden, name_ref) {
        (None, name_ref) if name_ref != 0 => {
            Some(get_utf8_string_by_ref(name_ref)?.replace('.', "/"))
        }
        _ => None,
    };

    let defined = with_method_area(|method_area| {
        method_area.define_class(
            expected_name.as_deref(),
            &byte_code,
            class_loader_ref,
            hidden,
        )
    })?;
    let mirror_ref = defined.mirror_clazz_ref()?;
    if class_data_ref != 0 {
        HEAP.set_object_field_value(
            mirror_ref,
            "java/lang/Class",
            "classData",
            vec![class_data_ref],
        )?;
    }
    if initialize {
        StaticInit::initialize_java_class(&defined)?;
    }

    Ok(mirror_ref)
}

fn array_bytes(buf_ref: i32, off: i32, len: i32) -> Result<Vec<u8>> {
    let buf = HEAP.get_entire_array(buf_ref)?;
    Ok(buf
        .get_entire_value()
        .iter()
        .skip(off as usize)
        .take(len as usize)
        .map(|v| v[0] as u8)
        .collect())
}

fn expected_name(name_ref: i32) -> Result<Option<String>> {
    if name_ref == 0 {
        return Ok(None);
    }
    Ok(Some(get_utf8_string_by_ref(name_ref)?.replace('.', "/")))
}

/// `java/lang/ClassLoader:defineClass1:(Ljava/lang/ClassLoader;Ljava/lang/String;[BIILjava/security/ProtectionDomain;Ljava/lang/String;)Ljava/lang/Class;`
pub(crate) fn define_class1(
    class_loader_ref: i32,
    name_ref: i32,
    buf_ref: i32,
    off: i32,
    len: i32,
    _protection_domain_ref: i32,
    _source_ref: i32,
) -> Result<i32> {
    let expected_name = expected_name(name_ref)?;
    let byte_code = array_bytes(buf_ref, off, len)?;

    with_method_area(|method_area| {
        method_area.define_class(expected_name.as_deref(), &byte_code, class_loader_ref, None)
    })?
    .mirror_clazz_ref()
}

/// `java/lang/ClassLoader:defineClass2:(Ljava/lang/ClassLoader;Ljava/lang/String;Ljava/nio/ByteBuffer;IILjava/security/ProtectionDomain;Ljava/lang/String;)Ljava/lang/Class;`
pub(crate) fn define_class2(
    class_loader_ref: i32,
    name_ref: i32,
    nio_byte_buf_ref: i32,
    off: i32,
    len: i32,
    _protection_domain_ref: i32,
    _source_ref: i32,
) -> Result<i32> {
    let expected_name = expected_name(name_ref)?;
    let instance_name = HEAP.get_instance_name(nio_byte_buf_ref)?;
    let addr = HEAP.get_object_field_value(nio_byte_buf_ref, &instance_name, "address")?;
    let addr = vec_to_i64(&addr);

    let addr = addr as usize as *const u8;
    if addr.is_null() {
        return Err(crate::vm::error::Error::new_execution(
            "ByteBuffer address is null",
        ));
    }
    let byte_code = unsafe { std::slice::from_raw_parts(addr.add(off as usize), len as usize) };

    with_method_area(|method_area| {
        method_area.define_class(expected_name.as_deref(), byte_code, class_loader_ref, None)
    })?
    .mirror_clazz_ref()
}

/// `java/lang/ClassLoader:findBootstrapClass:(Ljava/lang/String;)Ljava/lang/Class;`
///
/// Only classes the bootstrap loader defines: a class of another loader is never handed out
/// here, even when it is already loaded under the same name.
pub(crate) fn find_bootstrap_class(name_ref: i32) -> Result<i32> {
    let name = get_utf8_string_by_ref(name_ref)?;
    let internal_name = &name.replace('.', "/");

    let opt_klass = with_method_area(|a| {
        let class_file_path = format!("{internal_name}.class");
        if let Some(module) = a.modules_mapping().get(&class_file_path) {
            // Platform and application modules of the runtime image belong to their own loaders.
            let boot_module = crate::vm::BOOT_MODULES
                .get()
                .is_none_or(|boot| boot.contains(module.as_str()));
            boot_module.then(|| a.load_class_file(internal_name))
        } else if class_file_path.starts_with("java/") {
            // Try file-based loading for patched java.* classes (e.g. compiled with --patch-module java.base=.)
            // This is needed for hacky tests should_support_bootstrap_method_invoker, should_support_string_concat_helper, should_support_system_get_constant_pool
            // which operates with Test classes from java.lang.* package
            match a.load_class_file(internal_name) {
                Ok(klass) => Some(Ok(klass)),
                Err(e) => {
                    trace!(
                        "find_bootstrap_class: could not load patched class {internal_name}: {e}"
                    );
                    None
                }
            }
        } else {
            None
        }
    });
    if let Some(klass) = opt_klass {
        CLASSES.insert_klass(klass?, None)?;
    }

    let Some(loaded) = CLASSES.get_loaded(internal_name) else {
        return Ok(0);
    };
    if loaded.defining_loader().is_some() || namespaces::mirror_loader(&loaded)? != 0 {
        return Ok(0);
    }
    loaded.mirror_clazz_ref()
}

/// `java/lang/ClassLoader:findLoadedClass0:(Ljava/lang/String;)Ljava/lang/Class;`
///
/// The class this loader defined, or was recorded as an initiating loader of, under `name`.
/// The built-in loaders share one namespace, so for them that is a loaded class defined by the
/// loader itself or by one it delegates to (bootstrap, and platform for the application loader).
pub(crate) fn find_loaded_class0(this: i32, name_ref: i32) -> Result<i32> {
    let name = get_utf8_string_by_ref(name_ref)?;
    let internal_name = &name.replace('.', "/");
    let serial = namespaces::namespace_of(this)?;
    if serial != 0 {
        return match namespaces::recorded(serial, internal_name) {
            Some(key) => CLASSES.get(&key)?.mirror_clazz_ref(),
            None => Ok(0),
        };
    }

    let Some(loaded) = CLASSES.get_loaded(internal_name) else {
        return Ok(0);
    };
    if loaded.defining_loader().is_some() {
        return Ok(0);
    }
    let Some(mirror_ref) = loaded.mirror_ref_if_present() else {
        return Ok(0);
    };
    let definer = HEAP.get_object_field_value(mirror_ref, "java/lang/Class", "classLoader")?[0];
    let mut delegate = this;
    while delegate != 0 {
        if delegate == definer {
            return Ok(mirror_ref);
        }
        delegate = HEAP.get_object_field_value(delegate, namespaces::CLASS_LOADER, "parent")?[0];
    }
    Ok(if definer == 0 { mirror_ref } else { 0 })
}

fn increment_counter() -> u32 {
    COUNTER.fetch_add(1, Ordering::SeqCst)
}
