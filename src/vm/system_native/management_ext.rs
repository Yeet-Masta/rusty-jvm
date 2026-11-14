//! Natives of module `jdk.management` (`com.sun.management.internal.*`), implemented directly
//! instead of through `libmanagement_ext`, whose `JNI_OnLoad` needs HotSpot's `jmm` interface.

use crate::vm::error::Result;
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::create_array_of_strings;
use parking_lot::Mutex;
use std::sync::LazyLock;
use sysinfo::{Pid, ProcessesToUpdate, System};

static SYSTEM: LazyLock<Option<Mutex<System>>> =
    LazyLock::new(|| System::new().ok().map(Mutex::new));

fn with_system<T>(default: T, f: impl FnOnce(&mut System) -> T) -> T {
    match SYSTEM.as_ref() {
        Some(system) => f(&mut system.lock()),
        None => default,
    }
}

fn current_pid() -> Pid {
    Pid::from_u32(std::process::id())
}

// ---- com.sun.management.internal.OperatingSystemImpl ----

pub(crate) fn os_initialize0() -> Result<()> {
    Ok(())
}

pub(crate) fn get_committed_virtual_memory_size0(_this: i32) -> Result<i64> {
    Ok(with_system(-1, |system| {
        let pid = current_pid();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        system
            .process(pid)
            .map(|process| process.virtual_memory() as i64)
            .unwrap_or(-1)
    }))
}

pub(crate) fn get_total_memory_size0(_this: i32) -> Result<i64> {
    Ok(with_system(-1, |system| {
        system.refresh_memory();
        system.total_memory() as i64
    }))
}

pub(crate) fn get_free_memory_size0(_this: i32) -> Result<i64> {
    Ok(with_system(-1, |system| {
        system.refresh_memory();
        system.available_memory() as i64
    }))
}

pub(crate) fn get_total_swap_space_size0(_this: i32) -> Result<i64> {
    Ok(with_system(-1, |system| {
        system.refresh_memory();
        system.total_swap() as i64
    }))
}

pub(crate) fn get_free_swap_space_size0(_this: i32) -> Result<i64> {
    Ok(with_system(-1, |system| {
        system.refresh_memory();
        system.free_swap() as i64
    }))
}

/// CPU time used by the whole process, in nanoseconds.
pub(crate) fn get_process_cpu_time0(_this: i32) -> Result<i64> {
    #[cfg(windows)]
    {
        use winapi::shared::minwindef::FILETIME;
        use winapi::um::processthreadsapi::{GetCurrentProcess, GetProcessTimes};
        let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [creation, exit, kernel, user] = &mut times;
        if unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) } == 0 {
            return Ok(-1);
        }
        let as_nanos = |t: &FILETIME| {
            ((((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64) * 100) as i64
        };
        Ok(as_nanos(&times[2]) + as_nanos(&times[3]))
    }
    #[cfg(unix)]
    {
        use nix::libc;
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return Ok(-1);
        }
        let nanos = |t: libc::timeval| t.tv_sec as i64 * 1_000_000_000 + t.tv_usec as i64 * 1000;
        Ok(nanos(usage.ru_utime) + nanos(usage.ru_stime))
    }
}

/// Recent whole-system CPU load in `[0, 1]`.
pub(crate) fn get_cpu_load0(_this: i32) -> Result<f64> {
    Ok(with_system(-1.0, |system| {
        system.refresh_cpu_usage();
        (system.global_cpu_usage() as f64 / 100.0).clamp(0.0, 1.0)
    }))
}

/// Recent CPU load of this process in `[0, 1]`, across all CPUs.
pub(crate) fn get_process_cpu_load0(_this: i32) -> Result<f64> {
    Ok(with_system(-1.0, |system| {
        let pid = current_pid();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        system
            .process(pid)
            .map(|process| (process.cpu_usage() as f64 / 100.0 / cpus).clamp(0.0, 1.0))
            .unwrap_or(-1.0)
    }))
}

pub(crate) fn get_single_cpu_load0(this: i32, _cpu: i32) -> Result<f64> {
    get_cpu_load0(this)
}

pub(crate) fn get_host_configured_cpu_count0(_this: i32) -> Result<i32> {
    Ok(std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1))
}

pub(crate) fn get_host_online_cpu_count0(this: i32) -> Result<i32> {
    get_host_configured_cpu_count0(this)
}

pub(crate) fn get_host_total_cpu_ticks0(_this: i32) -> Result<i64> {
    Ok(-1)
}

pub(crate) fn get_open_file_descriptor_count0(_this: i32) -> Result<i64> {
    #[cfg(target_os = "linux")]
    {
        Ok(std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.count() as i64 - 1) // minus the directory handle itself
            .unwrap_or(-1))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(-1)
    }
}

pub(crate) fn get_max_file_descriptor_count0(_this: i32) -> Result<i64> {
    #[cfg(unix)]
    {
        use nix::libc;
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
            return Ok(-1);
        }
        Ok(limit.rlim_cur as i64)
    }
    #[cfg(not(unix))]
    {
        Ok(-1)
    }
}

// ---- GC extension attributes ----

/// rusty-jvm's collector exposes no extension attributes.
pub(crate) fn get_num_gc_ext_attributes(_this: i32, _gc: i32) -> Result<i32> {
    Ok(0)
}

pub(crate) fn fill_gc_attribute_info(
    _this: i32,
    _gc: i32,
    _count: i32,
    _names: i32,
    _types: i32,
    _descriptions: i32,
) -> Result<()> {
    Ok(())
}

/// No per-collection `GcInfo` is recorded, so the last one is reported as absent (null).
#[allow(clippy::too_many_arguments)]
pub(crate) fn get_last_gc_info0(
    _this: i32,
    _gc: i32,
    _count: i32,
    _values: i32,
    _types: i32,
    _before: i32,
    _after: i32,
) -> Result<i32> {
    Ok(0)
}

pub(crate) fn gc_set_notification_enabled(_this: i32, _gc: i32, _enabled: bool) -> Result<()> {
    Ok(())
}

// ---- VM flags and diagnostic commands ----

pub(crate) fn flag_initialize() -> Result<()> {
    Ok(())
}

/// No `-XX` flags are exposed through `HotSpotDiagnosticMXBean`.
pub(crate) fn get_all_flag_names() -> Result<i32> {
    create_array_of_strings(&[])
}

pub(crate) fn get_flags(_names: i32, _flags: i32, _count: i32) -> Result<i32> {
    Ok(0)
}

pub(crate) fn get_internal_flag_count() -> Result<i32> {
    Ok(0)
}

pub(crate) fn dcmd_set_notification_enabled(_this: i32, _enabled: bool) -> Result<()> {
    Ok(())
}

/// No diagnostic commands are available.
pub(crate) fn get_diagnostic_commands(_this: i32) -> Result<i32> {
    create_array_of_strings(&[])
}

pub(crate) fn get_diagnostic_command_info(_this: i32, _commands: i32) -> Result<i32> {
    HEAP.create_array_with_values(
        "[Lcom/sun/management/internal/DiagnosticCommandInfo;",
        &[],
        &mut crate::vm::stack::rooted_slots::RootedSlots::new(),
    )
}
