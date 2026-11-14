//! rusty-jvm's stand-in for HotSpot's VM library, which lets the JDK's own native libraries run.
//!
//! `libjava`, `libnet`, `libnio`, `libmanagement`, `libawt` and the other JDK native libraries are
//! JNI libraries, but they also import `JVM_*` functions from `jvm.dll` / `libjvm.so` /
//! `libjvm.dylib`, so without that library they cannot even be loaded. `build.rs` compiles
//! `jvm_shim/jvm_shim.c` into a library of that name, which this module embeds, writes to a
//! per-user cache directory and loads before any JDK library. Its `JVM_*` exports either work on
//! their own (time, raw monitors, `jio_*`) or forward to the implementations registered here.
//!
//! Once it is loaded, a native method without a built-in implementation resolves to the JDK's own
//! code (for example `sun.nio.ch.Net`, `java.lang.ProcessImpl`, `java.util.prefs`).

use crate::vm::error::{Error, Result};
#[cfg(unix)]
use libloading::os::unix::{Library as OsLibrary, RTLD_GLOBAL, RTLD_NOW};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{debug, warn};

static SHIM_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jvm_shim_lib"));
/// `rusty_jvm_shim_version()` of the shim this VM was built with.
const SHIM_VERSION: c_int = 1;

static SHIM: OnceLock<Option<Library>> = OnceLock::new();

#[cfg(windows)]
const FILE_NAME: &str = "jvm.dll";
#[cfg(target_os = "macos")]
const FILE_NAME: &str = "libjvm.dylib";
#[cfg(all(unix, not(target_os = "macos")))]
const FILE_NAME: &str = "libjvm.so";

/// Loads the shim once per process. Returns whether it is available; when it is not, JDK native
/// libraries cannot be loaded and the VM relies on its built-in natives alone.
pub(crate) fn install() -> bool {
    SHIM.get_or_init(|| match load() {
        Ok(library) => Some(library),
        Err(error) => {
            warn!("JDK native libraries are unavailable: {error}");
            None
        }
    })
    .is_some()
}

/// Whether the shim is loaded (without trying to load it).
pub(crate) fn is_installed() -> bool {
    SHIM.get().is_some_and(Option::is_some)
}

fn load() -> Result<Library> {
    let path = materialize().map_err(|error| {
        Error::new_execution(&format!("cannot write the JVM shim library: {error}"))
    })?;
    debug!("loading the JVM shim from {}", path.display());
    let library = open(&path)?;
    let version =
        unsafe { library.get::<unsafe extern "C" fn() -> c_int>(b"rusty_jvm_shim_version\0") }
            .map_err(|error| Error::new_execution(&format!("not a rusty-jvm shim: {error}")))?;
    if unsafe { version() } != SHIM_VERSION {
        return Err(Error::new_execution(
            "the cached JVM shim has the wrong version",
        ));
    }
    let set = unsafe {
        library.get::<unsafe extern "C" fn(*const c_char, *mut c_void) -> c_int>(
            b"rusty_jvm_shim_set\0",
        )
    }
    .map_err(|error| Error::new_execution(&format!("not a rusty-jvm shim: {error}")))?;
    for (name, function) in super::jvm_interface::functions() {
        let name = CString::new(name).expect("JVM function names have no NUL");
        if unsafe { set(name.as_ptr(), function) } == 0 {
            warn!("the JVM shim has no export {name:?}");
        }
    }
    Ok(library)
}

#[cfg(unix)]
fn open(path: &Path) -> Result<Library> {
    // RTLD_GLOBAL so the JDK libraries' undefined JVM_* symbols bind to the shim even when they
    // are resolved through the global scope rather than their DT_NEEDED entry.
    unsafe { OsLibrary::open(Some(path), RTLD_NOW | RTLD_GLOBAL) }
        .map(Library::from)
        .map_err(|error| Error::new_execution(&format!("cannot load {}: {error}", path.display())))
}

#[cfg(windows)]
fn open(path: &Path) -> Result<Library> {
    // Loaded under the module name `jvm.dll`, it satisfies every later `jvm.dll` import.
    unsafe { Library::new(path) }
        .map_err(|error| Error::new_execution(&format!("cannot load {}: {error}", path.display())))
}

/// Writes the embedded shim to the cache directory unless an identical copy is already there.
fn materialize() -> std::io::Result<PathBuf> {
    let directory = cache_root().join(format!("jvm-shim-{:016x}", fnv1a(SHIM_BYTES)));
    fs::create_dir_all(&directory)?;
    let path = directory.join(FILE_NAME);
    if fs::read(&path).is_ok_and(|existing| existing == SHIM_BYTES) {
        return Ok(path);
    }
    let staging = directory.join(format!("{FILE_NAME}.{}.tmp", std::process::id()));
    fs::write(&staging, SHIM_BYTES)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755))?;
    }
    // Another VM may have installed (and be using) the same file meanwhile; its copy is identical.
    if let Err(error) = fs::rename(&staging, &path) {
        let _ = fs::remove_file(&staging);
        if !fs::read(&path).is_ok_and(|existing| existing == SHIM_BYTES) {
            return Err(error);
        }
    }
    Ok(path)
}

/// A per-user cache directory, so no other account can plant a library the VM would load.
fn cache_root() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library").join("Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("rusty-jvm")
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, &byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_shim_is_a_shared_library() {
        assert!(SHIM_BYTES.len() > 1024);
        #[cfg(windows)]
        assert_eq!(&SHIM_BYTES[..2], b"MZ");
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(&SHIM_BYTES[..4], b"\x7fELF");
    }

    #[test]
    fn shim_loads_and_registers() {
        assert!(install());
        assert!(is_installed());
    }
}
