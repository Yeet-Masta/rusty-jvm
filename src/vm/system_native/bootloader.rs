use crate::vm::error::Result;

/// `jdk.internal.loader.BootLoader.setBootLoaderUnnamedModule0(Ljava/lang/Module;)V`
pub(crate) fn set_bootloader_unnamed_module0(_module_ref: i32) -> Result<()> {
    Ok(())
}

/// Boot-loader classes grouped by package (`java/lang`), with one mirror per package.
fn boot_packages() -> Result<std::collections::BTreeMap<String, i32>> {
    use crate::vm::heap::heap::HEAP;
    let mut packages = std::collections::BTreeMap::new();
    for klass in crate::vm::method_area::loaded_classes::CLASSES.snapshot() {
        let name = klass.this_class_name();
        if name.starts_with('[') {
            continue;
        }
        let Some((package, _)) = name.rsplit_once('/') else {
            continue;
        };
        if packages.contains_key(package) {
            continue;
        }
        let Some(mirror) = klass.mirror_ref_if_present() else {
            continue;
        };
        let loader = HEAP.get_object_field_value(mirror, "java/lang/Class", "classLoader")?[0];
        if loader == 0 {
            packages.insert(package.to_string(), mirror);
        }
    }
    Ok(packages)
}

/// `BootLoader.getSystemPackageNames()[Ljava/lang/String;`: packages with classes the boot
/// loader has defined, in internal (`/`) form, as HotSpot's `JVM_GetSystemPackages` reports.
pub(crate) fn get_system_package_names() -> Result<i32> {
    let names: Vec<String> = boot_packages()?.into_keys().collect();
    crate::vm::helper::create_array_of_strings(&names)
}

/// `BootLoader.getSystemPackageLocation(Ljava/lang/String;)Ljava/lang/String;`: `jrt:/<module>`
/// for a package of a named boot module, null when the boot loader defines no such package.
pub(crate) fn get_system_package_location(name_ref: i32) -> Result<i32> {
    use crate::vm::heap::heap::HEAP;
    let name =
        crate::vm::system_native::string::get_utf8_string_by_ref(name_ref)?.replace('.', "/");
    let Some(mirror) = boot_packages()?.remove(&name) else {
        return Ok(0);
    };
    let module = HEAP.get_object_field_value(mirror, "java/lang/Class", "module")?[0];
    if module == 0 {
        return Ok(0);
    }
    let module_name = HEAP.get_object_field_value(module, "java/lang/Module", "name")?[0];
    if module_name == 0 {
        return Ok(0);
    }
    let module_name = crate::vm::system_native::string::get_utf8_string_by_ref(module_name)?;
    crate::vm::execution_engine::string_pool_helper::StringPoolHelper::get_string(&format!(
        "jrt:/{module_name}"
    ))
}
