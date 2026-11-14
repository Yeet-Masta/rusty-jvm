//! Per-class-loader namespaces.
//!
//! The class registry is keyed by one string per class. Classes defined by the built-in loaders
//! (bootstrap, platform, application) keep their plain binary name as that key, exactly as before.
//! A class defined by any other loader is registered under `binary/Name#<serial>`, where the
//! serial identifies its defining loader and is never reused, so two loaders can define classes
//! of the same name without colliding. Everything downstream (hierarchy walks, field layouts,
//! `checkcast`) keeps working on keys.
//!
//! What a *symbolic* name means therefore depends on who is asking: [`resolve`] turns a name from
//! a class's constant pool into the key of the class its defining loader produces for it.

use crate::vm::error::{Error, Result};
use crate::vm::exception::pending_helpers::java_exception;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::klass;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::java_class::JavaClass;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::method_area::lookup;
use crate::vm::method_area::primitives_helper::PRIMITIVE_TYPE_BY_CODE;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, LazyLock};

/// Separates a binary name from the serial of its defining loader in a registry key.
pub(crate) const LOADER_KEY_DELIM: char = '#';

pub(crate) const CLASS_LOADER: &str = "java/lang/ClassLoader";
/// VM-only `int` field injected into `java.lang.ClassLoader`: 0 until first asked, -1 for a
/// built-in loader, otherwise the loader's serial.
pub(crate) const LOADER_SERIAL_FIELD: &str = "vmLoaderSerial";
const BUILTIN_MARK: i32 = -1;

const BUILTIN_LOADERS: [&str; 3] = [
    "jdk/internal/loader/ClassLoaders$AppClassLoader",
    "jdk/internal/loader/ClassLoaders$PlatformClassLoader",
    "jdk/internal/loader/ClassLoaders$BootClassLoader",
];
const LOAD_CLASS: &str = "loadClass:(Ljava/lang/String;)Ljava/lang/Class;";

static NEXT_SERIAL: AtomicI32 = AtomicI32::new(1);
static SERIAL_ASSIGNMENT: Mutex<()> = Mutex::new(());

/// `(loader serial, binary name) -> registry key` for every class a non-built-in loader has
/// defined or been recorded as an initiating loader of.
static NAMESPACES: LazyLock<DashMap<(u32, String), String>> = LazyLock::new(DashMap::new);

/// The loader that defined a class outside the built-in namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DefiningLoader {
    pub(crate) serial: u32,
    /// Kept alive by the class mirror's `classLoader` field; classes are never unloaded.
    pub(crate) loader_ref: i32,
}

/// The namespace a loader defines classes in: 0 for the shared built-in namespace (the bootstrap
/// loader and the JDK's own platform and application loaders), otherwise the loader's serial.
pub(crate) fn namespace_of(loader_ref: i32) -> Result<u32> {
    if loader_ref == 0 {
        return Ok(0);
    }
    let read = || -> Result<i32> {
        Ok(HEAP.get_object_field_value(loader_ref, CLASS_LOADER, LOADER_SERIAL_FIELD)?[0])
    };
    let decode = |stored: i32| {
        if stored == BUILTIN_MARK {
            0
        } else {
            stored as u32
        }
    };

    let stored = read()?;
    if stored != 0 {
        return Ok(decode(stored));
    }
    let builtin = BUILTIN_LOADERS.contains(&HEAP.get_instance_name(loader_ref)?.as_str());
    let _guard = crate::vm::safepoint::lock(&SERIAL_ASSIGNMENT);
    let stored = read()?;
    if stored != 0 {
        return Ok(decode(stored));
    }
    let assigned = if builtin {
        BUILTIN_MARK
    } else {
        NEXT_SERIAL.fetch_add(1, Ordering::SeqCst)
    };
    HEAP.set_object_field_value(
        loader_ref,
        CLASS_LOADER,
        LOADER_SERIAL_FIELD,
        vec![assigned],
    )?;
    Ok(decode(assigned))
}

/// The registry key for `binary_name` as defined by the loader with `serial`.
pub(crate) fn key_for(binary_name: &str, serial: u32) -> String {
    format!("{binary_name}{LOADER_KEY_DELIM}{serial}")
}

/// A registry key (or an array descriptor over one) without loader serials: the name as class
/// files and Java code spell it.
pub(crate) fn binary_name(key: &str) -> Cow<'_, str> {
    let Some(start) = key.find(LOADER_KEY_DELIM) else {
        return Cow::Borrowed(key);
    };
    let digits = key[start + 1..]
        .bytes()
        .take_while(u8::is_ascii_digit)
        .count();
    let mut plain = String::with_capacity(key.len());
    plain.push_str(&key[..start]);
    plain.push_str(&key[start + 1 + digits..]);
    Cow::Owned(plain)
}

/// The key `loader` already has on record for `binary_name`, as its definer or as an initiating
/// loader. Never loads.
pub(crate) fn recorded(serial: u32, binary_name: &str) -> Option<String> {
    NAMESPACES
        .get(&(serial, binary_name.to_string()))
        .map(|entry| entry.value().clone())
}

/// Records `key` as what `binary_name` means to the loader with `serial`. Returns false, leaving
/// the earlier entry alone, when the loader already has one.
pub(crate) fn record(serial: u32, binary_name: &str, key: &str) -> bool {
    match NAMESPACES.entry((serial, binary_name.to_string())) {
        dashmap::mapref::entry::Entry::Occupied(_) => false,
        dashmap::mapref::entry::Entry::Vacant(vacant) => {
            vacant.insert(key.to_string());
            true
        }
    }
}

/// Splits `[[Lpkg/Name;` into (`[[`, `pkg/Name`); a non-array name has an empty prefix. `None`
/// for a primitive or an array of primitives, which no loader has a say in.
fn split_element(name: &str) -> Option<(&str, &str)> {
    let dimensions = name.bytes().take_while(|byte| *byte == b'[').count();
    let (prefix, element) = name.split_at(dimensions);
    if dimensions == 0 {
        if PRIMITIVE_TYPE_BY_CODE.contains_key(element) {
            return None;
        }
        return Some((prefix, crate::vm::helper::undecorate(element)));
    }
    element
        .strip_prefix('L')
        .and_then(|element| element.strip_suffix(';'))
        .map(|element| (prefix, element))
}

fn join_element(prefix: &str, element_key: &str) -> String {
    if prefix.is_empty() {
        element_key.to_string()
    } else {
        format!("{prefix}L{element_key};")
    }
}

/// Resolves the symbolic `name` (a class or array name from a constant pool or descriptor) the
/// way `referrer` sees it, returning the registry key. A no-op for classes in the built-in
/// namespace, which is the overwhelmingly common case.
pub(crate) fn resolve<'a>(referrer: &JavaClass, name: &'a str) -> Result<Cow<'a, str>> {
    let Some(loader) = referrer.defining_loader() else {
        return Ok(Cow::Borrowed(name));
    };
    if name == referrer.this_class_name() {
        return Ok(Cow::Borrowed(name));
    }
    let Some((prefix, element)) = split_element(name) else {
        return Ok(Cow::Borrowed(name));
    };
    if element == referrer.this_class_name() || element == referrer.binary_name() {
        return Ok(Cow::Owned(join_element(prefix, referrer.this_class_name())));
    }
    if element.contains(LOADER_KEY_DELIM) {
        return Ok(Cow::Borrowed(name)); // already a key
    }
    let element_key = load_element(loader.loader_ref, loader.serial, element)
        .map_err(|error| no_class_def_found(error, element))?;
    Ok(Cow::Owned(join_element(prefix, &element_key)))
}

/// `Class.forName` semantics for any loader: the class (or array class) `name` denotes to
/// `loader_ref`, loading it if needed. `name` is an internal name.
pub(crate) fn load_with_loader(loader_ref: i32, name: &str) -> Result<Arc<JavaClass>> {
    let serial = namespace_of(loader_ref)?;
    let Some((prefix, element)) = split_element(name) else {
        // Arrays of primitives exist for every loader; a primitive itself has no binary name.
        if name.starts_with('[') {
            return CLASSES.get(name);
        }
        return Err(java_exception("java/lang/ClassNotFoundException", name));
    };
    if loader_ref == 0 {
        let element_class = CLASSES.get(element)?;
        if element_class.defining_loader().is_some() || mirror_loader(&element_class)? != 0 {
            return Err(java_exception(
                "java/lang/ClassNotFoundException",
                &name.replace('/', "."),
            ));
        }
        return CLASSES.get(&join_element(prefix, element_class.this_class_name()));
    }
    let element_key = load_element(loader_ref, serial, element)?;
    CLASSES.get(&join_element(prefix, &element_key))
}

/// The key of the non-array class `element` as `loader_ref` resolves it, consulting the loader's
/// record first and otherwise asking the loader itself (`loadClass`).
fn load_element(loader_ref: i32, serial: u32, element: &str) -> Result<String> {
    if serial != 0 {
        if let Some(key) = recorded(serial, element) {
            return Ok(key);
        }
    }
    let _loader = JavaThread::hold_temp_roots(vec![Slot::Ref(loader_ref)]);
    let name_ref = StringPoolHelper::get_string(&element.replace('/', "."))?;
    let loader_class = HEAP.get_instance_name(loader_ref)?;
    let load_class = lookup::lookup_instance_method(&loader_class, LOAD_CLASS)?
        .ok_or_else(|| Error::new_execution("ClassLoader.loadClass(String) not found"))?;
    let loaded = Executor::invoke_non_static_method(
        load_class.class_name(),
        LOAD_CLASS,
        loader_ref,
        &[StackValueKind::Ref(name_ref)],
    )?;
    let clazz_ref = loaded[0].value();
    if clazz_ref == 0 {
        return Err(java_exception(
            "java/lang/ClassNotFoundException",
            &element.replace('/', "."),
        ));
    }
    let loaded_class = klass(clazz_ref)?;
    let key = loaded_class.this_class_name().clone();
    if serial != 0 {
        record(serial, element, &key);
    }
    Ok(key)
}

/// The defining loader of a published class, as its mirror records it (0 for bootstrap).
pub(crate) fn mirror_loader(class: &JavaClass) -> Result<i32> {
    Ok(
        HEAP.get_object_field_value(class.mirror_clazz_ref()?, "java/lang/Class", "classLoader")?
            [0],
    )
}

/// JVMS §5.3: a `ClassNotFoundException` from the loader during resolution surfaces as a
/// `NoClassDefFoundError` caused by it. Any other failure is passed through.
fn no_class_def_found(error: Error, element: &str) -> Error {
    let Some(cause_ref) = error.throwable_ref() else {
        return error;
    };
    let is_not_found = HEAP
        .get_instance_name(cause_ref)
        .and_then(|name| {
            crate::vm::method_area::instance_checker::InstanceChecker::checkcast(
                &name,
                "java/lang/ClassNotFoundException",
            )
        })
        .unwrap_or(false);
    if !is_not_found {
        return error;
    }
    let wrap = || -> Result<Error> {
        let _cause = JavaThread::hold_temp_roots(vec![Slot::Ref(cause_ref)]);
        let message = StringPoolHelper::create_string(element)?;
        let wrapped = Executor::invoke_args_constructor(
            "java/lang/NoClassDefFoundError",
            "<init>:(Ljava/lang/String;)V",
            &[StackValueKind::Ref(message[0].value())],
            None,
        )?;
        Executor::invoke_non_static_method(
            "java/lang/Throwable",
            "initCause:(Ljava/lang/Throwable;)Ljava/lang/Throwable;",
            wrapped[0].value(),
            &[StackValueKind::Ref(cause_ref)],
        )?;
        Ok(Error::uncaught_exception(wrapped[0]))
    };
    wrap().unwrap_or_else(|failure| failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_strips_loader_serials() {
        assert_eq!(binary_name("pkg/Name"), "pkg/Name");
        assert_eq!(binary_name("pkg/Name#12"), "pkg/Name");
        assert_eq!(binary_name("[[Lpkg/Name#3;"), "[[Lpkg/Name;");
        assert_eq!(
            binary_name("pkg/Name$$Lambda/0x0000000000000001"),
            "pkg/Name$$Lambda/0x0000000000000001"
        );
    }

    #[test]
    fn split_element_separates_array_prefix() {
        assert_eq!(split_element("pkg/Name"), Some(("", "pkg/Name")));
        assert_eq!(split_element("[[Lpkg/Name;"), Some(("[[", "pkg/Name")));
        assert_eq!(split_element("[I"), None);
        assert_eq!(split_element("I"), None);
        assert_eq!(join_element("[[", "pkg/Name#2"), "[[Lpkg/Name#2;");
        assert_eq!(join_element("", "pkg/Name#2"), "pkg/Name#2");
    }
}
