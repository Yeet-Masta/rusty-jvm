//! Natives of module `jdk.management` (`com.sun.management.internal.*`), implemented directly
//! instead of through `libmanagement_ext`, whose `JNI_OnLoad` needs HotSpot's `jmm` interface.

use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::set_pending_io_exception;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::gc;
use crate::vm::heap::heap::HEAP;
use crate::vm::heap::hprof;
use crate::vm::helper::create_array_of_strings;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_limit;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::management::vm_arguments;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use parking_lot::Mutex;
use std::io::ErrorKind;
use std::path::Path;
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

enum FlagValue {
    Bool(bool),
    Uint(u64),
}

/// A VM option as `HotSpotDiagnosticMXBean` reports it. Only options this VM acts on are listed,
/// under HotSpot's names where HotSpot has the same option.
struct VmFlag {
    name: &'static str,
    value: FlagValue,
    /// Given when the VM was started, rather than left at its default.
    on_command_line: bool,
    /// `VMOption.Origin` of a value that was not given.
    default_origin: &'static str,
}

fn vm_flags() -> Vec<VmFlag> {
    let arguments = vm_arguments();
    let given = |prefixes: &[&str]| {
        arguments
            .iter()
            .any(|argument| prefixes.iter().any(|prefix| argument.starts_with(prefix)))
    };
    let mut flags = Vec::new();
    if let Some(config) = gc::get() {
        flags.push(VmFlag {
            name: "MaxHeapSize",
            value: FlagValue::Uint(config.max_heap_bytes),
            on_command_line: given(&["-Xmx"]),
            default_origin: "ERGONOMIC",
        });
        flags.push(VmFlag {
            name: "GCAllocThreshold",
            value: FlagValue::Uint(config.gc_alloc_threshold),
            on_command_line: config.fixed_threshold,
            default_origin: "DEFAULT",
        });
        flags.push(VmFlag {
            name: "GCStressAtSafepoints",
            value: FlagValue::Bool(config.stress_at_safepoints),
            on_command_line: config.stress_at_safepoints,
            default_origin: "DEFAULT",
        });
    }
    flags.push(VmFlag {
        name: "ThreadStackSize",
        value: FlagValue::Uint(stack_limit::stack_bytes() as u64 / 1024),
        on_command_line: given(&["-Xss", "-XX:ThreadStackSize="]),
        default_origin: "DEFAULT",
    });
    flags
}

pub(crate) fn get_all_flag_names() -> Result<i32> {
    let names: Vec<String> = vm_flags()
        .iter()
        .map(|flag| flag.name.to_string())
        .collect();
    create_array_of_strings(&names)
}

/// Builds a `com.sun.management.internal.Flag`, as `Flag.c` does: a `Boolean` or `Long` value,
/// never writeable (no option can be changed in a running VM), and external (a product option).
fn new_flag(flag: &VmFlag) -> Result<i32> {
    const ORIGIN: &str = "com/sun/management/VMOption$Origin";
    let name = StringPoolHelper::create_string(flag.name)?;
    let value = match flag.value {
        FlagValue::Bool(value) => Executor::invoke_static_method(
            "java/lang/Boolean",
            "valueOf:(Z)Ljava/lang/Boolean;",
            &[StackValueKind::I32(value as i32)],
        )?,
        FlagValue::Uint(value) => Executor::invoke_static_method(
            "java/lang/Long",
            "valueOf:(J)Ljava/lang/Long;",
            &[StackValueKind::I64(value as i64)],
        )?,
    };
    let origin_name = StringPoolHelper::create_string(if flag.on_command_line {
        "VM_CREATION"
    } else {
        flag.default_origin
    })?;
    let origin = Executor::invoke_static_method(
        ORIGIN,
        "valueOf:(Ljava/lang/String;)Lcom/sun/management/VMOption$Origin;",
        &[StackValueKind::Ref(origin_name[0].value())],
    )?;
    let args = [
        StackValueKind::Ref(name[0].value()),
        StackValueKind::Ref(value[0].value()),
        StackValueKind::I32(0),
        StackValueKind::I32(1),
        StackValueKind::Ref(origin[0].value()),
    ];
    let created = Executor::invoke_args_constructor(
        "com/sun/management/internal/Flag",
        "<init>:(Ljava/lang/String;Ljava/lang/Object;ZZLcom/sun/management/VMOption$Origin;)V",
        &args,
        None,
    )?;
    Ok(created[0].value())
}

/// `Flag.getFlags`: fills `flags` from the front with the named options that exist, or with
/// every option when `names` is null, and returns how many it stored.
pub(crate) fn get_flags(names: i32, flags: i32, count: i32) -> Result<i32> {
    let all = vm_flags();
    let wanted: Vec<&VmFlag> = if names == 0 {
        all.iter().take(count.max(0) as usize).collect()
    } else {
        let mut wanted = Vec::new();
        for index in 0..count.min(HEAP.get_array_len(names)?) {
            let name_ref = HEAP.get_array_value(names, index)?[0];
            if name_ref == 0 {
                continue;
            }
            let name = get_utf8_string_by_ref(name_ref)?;
            wanted.extend(all.iter().find(|flag| flag.name == name));
        }
        wanted
    };
    let capacity = HEAP.get_array_len(flags)?;
    let mut stored = 0;
    for flag in wanted {
        if stored >= capacity {
            break;
        }
        let flag_ref = new_flag(flag)?;
        let _flag = JavaThread::hold_temp_roots(vec![Slot::Ref(flag_ref)]);
        HEAP.set_array_value(flags, stored, vec![flag_ref])?;
        stored += 1;
    }
    Ok(stored)
}

pub(crate) fn get_internal_flag_count() -> Result<i32> {
    Ok(vm_flags().len() as i32)
}

// ---- com.sun.management.internal.HotSpotDiagnostic ----

/// `HotSpotDiagnostic.dumpHeap0`: writes an HPROF dump of the heap to a file that must not exist
/// yet. Failures are `IOException`s with the operating system's reason, as HotSpot reports them.
pub(crate) fn dump_heap0(_this: i32, output_file: i32, live: bool) -> Result<()> {
    let path = get_utf8_string_by_ref(output_file)?;
    if let Err(error) = hprof::dump(Path::new(&path), live)? {
        let message = match error.kind() {
            ErrorKind::AlreadyExists => "File exists".to_string(),
            ErrorKind::NotFound => "No such file or directory".to_string(),
            ErrorKind::PermissionDenied => "Permission denied".to_string(),
            _ => error.to_string(),
        };
        set_pending_io_exception(&message)?;
    }
    Ok(())
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
