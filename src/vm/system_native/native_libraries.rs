use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::set_pending_unsatisfied_link_error;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::i64_to_vec;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::jni::java_vm_interface_impl::is_supported_jni_version_including_1_1;
use crate::vm::jni::jni_env::get_jni_vm;
use crate::vm::safepoint::{Blocking, Reason};
use crate::vm::system_native::properties_provider::properties::sun_boot_library_path;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use dashmap::DashMap;
use derive_new::new;
use jni_sys::{jint, JavaVM, JNI_VERSION_1_1};
#[cfg(unix)]
use libloading::os::unix::*;
#[cfg(windows)]
use libloading::os::windows::*;
use std::ffi::{c_void, CString};
use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use tracing::{enabled, trace, warn};

static REGISTRY: LazyLock<DashMap<usize, Arc<LibraryEntry>>> = LazyLock::new(DashMap::default);

#[derive(Debug, new)]
struct LibraryEntry {
    library: Library,
    symbols: DashMap<String, usize>,
}

/// `jdk.internal.loader.NativeLibraries.findBuiltinLib(Ljava/lang/String;)Ljava/lang/String;`
pub(crate) fn find_builtin_lib(name_ref: i32) -> Result<i32> {
    if enabled!(tracing::Level::TRACE) {
        let name = get_utf8_string_by_ref(name_ref)?;
        trace!("findBuiltinLib: {name}");
    }

    Ok(0) // we don't have static libraries, so we always return null
}

/// `jdk.internal.loader.NativeLibraries.load(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z`
pub(crate) fn load(
    native_lib_impl_ref: i32,
    name_ref: i32,
    _builtin: bool,
    throw_exception_if_fail: bool,
) -> Result<bool> {
    let name = get_utf8_string_by_ref(name_ref)?;

    // skip loading of jdk system libraries (libzip, libnio, libjimage and so on) since we have this functionality built-in
    let path = Path::new(&name);
    if let Some(parent_dir) = path.parent() {
        if let (Ok(canonic_parent_dir), Ok(canonic_boot_lib_path)) =
            crate::vm::safepoint::blocking(|| {
                (
                    fs::canonicalize(parent_dir),
                    fs::canonicalize(Path::new(sun_boot_library_path())),
                )
            })
        {
            if canonic_parent_dir == canonic_boot_lib_path {
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| extract_lib_name(s))
                    .is_some_and(|lib_name| ["nio", "jimage", "zip", "net"].contains(&lib_name))
                {
                    return Ok(true);
                }
            }
        }
    }

    match crate::vm::safepoint::blocking(|| unsafe { Library::new(&name) }) {
        Ok(lib) => {
            let Some(jni_version) = on_load(&lib, &name)? else {
                // Like HotSpot, a library whose JNI_OnLoad failed is unloaded again
                crate::vm::safepoint::blocking(|| drop(lib));
                return Ok(false);
            };
            let raw_ptr = lib.into_raw();
            REGISTRY.entry(raw_ptr as usize).or_insert_with(|| {
                // Safety: raw_ptr was just obtained from lib.into_raw() and is not yet
                // wrapped by another Library instance.
                let lib = unsafe { Library::from_raw(raw_ptr) };
                Arc::new(LibraryEntry::new(lib, DashMap::default()))
            });
            HEAP.set_object_field_value(
                native_lib_impl_ref,
                "jdk/internal/loader/NativeLibraries$NativeLibraryImpl",
                "jniVersion",
                vec![jni_version],
            )?;
            HEAP.set_object_field_value(
                native_lib_impl_ref,
                "jdk/internal/loader/NativeLibraries$NativeLibraryImpl",
                "handle",
                i64_to_vec(raw_ptr as i64),
            )?;

            Ok(true)
        }
        Err(e) => {
            if throw_exception_if_fail {
                set_pending_unsatisfied_link_error(&format!("Failed to load {name}: {e}"))?;
            }
            Ok(false)
        }
    }
}

/// `JNI_OnLoad`, which a library exports to learn the VM it is loaded into, typically to register
/// its native methods.
type OnLoad = unsafe extern "system" fn(*mut JavaVM, *mut c_void) -> jint;
/// `JNI_OnUnload`, which a library exports to release what it holds before it is unloaded.
type OnUnload = unsafe extern "system" fn(*mut JavaVM, *mut c_void);

/// Calls the library's `JNI_OnLoad`, like HotSpot's `NativeLibraries.load` (`NativeLibraries.c`),
/// and returns the JNI version the library requires: 1.1 for a library without one.
///
/// `None` leaves an exception pending: the one `JNI_OnLoad` threw, or an `UnsatisfiedLinkError`
/// for a version this VM does not support.
fn on_load(library: &Library, name: &str) -> Result<Option<jint>> {
    let on_load = unsafe { library.get::<OnLoad>(b"JNI_OnLoad\0") }
        .ok()
        .map(|symbol| *symbol);
    let version = match on_load {
        Some(on_load) => {
            let vm = get_jni_vm();
            // Third-party code runs in the Safe state and enters the VM through JNI functions
            let _native = Blocking::enter(Reason::Native);
            unsafe { on_load(vm, std::ptr::null_mut()) }
        }
        None => JNI_VERSION_1_1,
    };
    if JavaThread::get_pending_exception().is_some() {
        return Ok(None);
    }
    if !is_supported_jni_version_including_1_1(version) {
        set_pending_unsatisfied_link_error(&format!(
            "unsupported JNI version 0x{version:x} required by {name}"
        ))?;
        return Ok(None);
    }
    Ok(Some(version))
}

/// `jdk.internal.loader.NativeLibraries.unload(Ljava/lang/String;ZJ)V`
///
/// Calls the library's `JNI_OnUnload` and unloads it, like HotSpot. The JDK runs this from a
/// `Cleaner` once the class loader that loaded the library has become unreachable.
pub(crate) fn unload(_name_ref: i32, _builtin: bool, handle: i64) -> Result<()> {
    let Some((_, entry)) = REGISTRY.remove(&(handle as usize)) else {
        return Ok(());
    };
    let on_unload = unsafe { entry.library.get::<OnUnload>(b"JNI_OnUnload\0") }
        .ok()
        .map(|symbol| *symbol);
    let vm = get_jni_vm();
    let _native = Blocking::enter(Reason::Native);
    if let Some(on_unload) = on_unload {
        unsafe { on_unload(vm, std::ptr::null_mut()) };
    }
    // The library is unloaded when the last holder (a racing `findEntry0`) drops its entry
    drop(entry);
    Ok(())
}

fn extract_lib_name(s: &str) -> Option<&str> {
    let s = s.strip_prefix("lib").unwrap_or(s);
    Some(s.rsplit_once('.')?.0)
}

/// `jdk.internal.loader.NativeLibraries$NativeLibraryImpl.findEntry0(JLjava/lang/String;)J`
pub(crate) fn find_entry0(handle: i64, name_ref: i32) -> Result<i64> {
    let lib = match REGISTRY.get(&(handle as usize)) {
        Some(lib) => Arc::clone(lib.value()),
        None => {
            warn!("Library not found in registry for handle: {handle}");
            return Ok(0);
        }
    };

    let name = get_utf8_string_by_ref(name_ref)?;
    let c_name = CString::from_str(&name)?;
    let raw_ptr = crate::vm::safepoint::blocking(|| {
        let symbol = lib.symbols.entry(name.clone()).or_insert_with(|| {
            let sym: core::result::Result<Symbol<*const c_void>, libloading::Error> =
                unsafe { lib.library.get(c_name.as_bytes_with_nul()) };
            match sym {
                Ok(sym) => sym.as_raw_ptr() as usize,
                Err(_) => 0,
            }
        });

        *symbol.value()
    });
    if raw_ptr == 0 {
        return Ok(0);
    }

    Ok(raw_ptr as i64)
}
