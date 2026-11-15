//! `java.lang.management` support: the natives of `sun.management.*` (module `java.management`),
//! implemented directly instead of through `libmanagement` and HotSpot's `jmm` interface.
//!
//! rusty-jvm has one managed heap and a single stop-the-world collector, so it reports one heap
//! pool (`Java heap`) collected by one collector (`rusty-jvm GC`), and one non-heap pool
//! (`Metaspace`) for class metadata.

use crate::vm::error::Result;
use crate::vm::execution_engine::executor::Executor;
use crate::vm::execution_engine::string_pool_helper::StringPoolHelper;
use crate::vm::gc::collector::{COLLECTION_COUNT, COLLECTION_NANOS};
use crate::vm::heap::heap::HEAP;
use crate::vm::helper::create_array_of_strings;
use crate::vm::jni::java_thread::JavaThread;
use crate::vm::method_area::loaded_classes::CLASSES;
use crate::vm::stack::rooted_slots::RootedSlots;
use crate::vm::stack::slot::Slot;
use crate::vm::stack::stack_value::StackValueKind;
use crate::vm::system_native::string::get_utf8_string_by_ref;
use crate::vm::threads;
use crate::Arguments;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const VM_MANAGEMENT: &str = "sun/management/VMManagementImpl";
const FACTORY_HELPER: &str = "sun/management/ManagementFactoryHelper";
const MEMORY_USAGE: &str = "java/lang/management/MemoryUsage";

const HEAP_POOL: &str = "Java heap";
const METASPACE_POOL: &str = "Metaspace";
const COLLECTOR: &str = "rusty-jvm GC";
const METASPACE_MANAGER: &str = "Metaspace Manager";

/// Rough per-class metadata footprint used for the `Metaspace` pool, which rusty-jvm does not
/// account separately from the Rust heap.
const METADATA_BYTES_PER_CLASS: i64 = 4096;

static VM_START: LazyLock<(Instant, i64)> = LazyLock::new(|| {
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (Instant::now(), epoch_ms)
});
static VM_ARGUMENTS: OnceLock<Vec<String>> = OnceLock::new();
static VERBOSE_GC: AtomicBool = AtomicBool::new(false);
static VERBOSE_CLASS: AtomicBool = AtomicBool::new(false);
static THREAD_CPU_TIME_ENABLED: AtomicBool = AtomicBool::new(true);
static CONTENTION_MONITORING_ENABLED: AtomicBool = AtomicBool::new(false);

/// Records the VM start time and the JVM options `RuntimeMXBean.getInputArguments()` reports.
pub(crate) fn init(arguments: &Arguments) {
    LazyLock::force(&VM_START);
    let mut options = Vec::new();
    for (key, value) in arguments.system_properties() {
        options.push(format!("-D{key}={value}"));
    }
    options.extend(arguments.jvm_options().iter().cloned());
    options.extend(arguments.advanced_jvm_options().iter().cloned());
    let _ = VM_ARGUMENTS.set(options);
}

/// The options the VM was started with, as `RuntimeMXBean.getInputArguments()` reports them.
pub(crate) fn vm_arguments() -> &'static [String] {
    VM_ARGUMENTS.get().map_or(&[], Vec::as_slice)
}

fn set_static_bool(class: &str, field: &str, value: bool) -> Result<()> {
    if let Some(field) = CLASSES.get(class)?.static_field(field) {
        field.set_raw_value(vec![value as i32])?;
    }
    Ok(())
}

// ---- sun.management.VMManagementImpl ----

/// `getVersion0()`: the `jmm` interface version, `major.minor` (HotSpot reports JMM_VERSION_4).
pub(crate) fn get_version0() -> Result<i32> {
    StringPoolHelper::get_string("4.0")
}

pub(crate) fn init_optional_support_fields() -> Result<()> {
    set_static_bool(VM_MANAGEMENT, "compTimeMonitoringSupport", false)?;
    set_static_bool(VM_MANAGEMENT, "threadContentionMonitoringSupport", false)?;
    set_static_bool(VM_MANAGEMENT, "currentThreadCpuTimeSupport", true)?;
    set_static_bool(VM_MANAGEMENT, "otherThreadCpuTimeSupport", false)?;
    set_static_bool(VM_MANAGEMENT, "objectMonitorUsageSupport", true)?;
    set_static_bool(VM_MANAGEMENT, "synchronizerUsageSupport", true)?;
    set_static_bool(VM_MANAGEMENT, "threadAllocatedMemorySupport", false)?;
    set_static_bool(VM_MANAGEMENT, "remoteDiagnosticCommandsSupport", false)?;
    Ok(())
}

pub(crate) fn is_thread_contention_monitoring_enabled(_this: i32) -> Result<bool> {
    Ok(CONTENTION_MONITORING_ENABLED.load(Ordering::Relaxed))
}
pub(crate) fn is_thread_cpu_time_enabled(_this: i32) -> Result<bool> {
    Ok(THREAD_CPU_TIME_ENABLED.load(Ordering::Relaxed))
}
pub(crate) fn is_thread_allocated_memory_enabled(_this: i32) -> Result<bool> {
    Ok(false)
}
pub(crate) fn get_total_class_count(_this: i32) -> Result<i64> {
    Ok(CLASSES.loaded_count() as i64)
}
pub(crate) fn get_unloaded_class_count(_this: i32) -> Result<i64> {
    Ok(0) // class unloading is disabled
}
pub(crate) fn get_verbose_class(_this: i32) -> Result<bool> {
    Ok(VERBOSE_CLASS.load(Ordering::Relaxed))
}
pub(crate) fn get_verbose_gc(_this: i32) -> Result<bool> {
    Ok(VERBOSE_GC.load(Ordering::Relaxed))
}
pub(crate) fn get_process_id(_this: i32) -> Result<i32> {
    Ok(std::process::id() as i32)
}
pub(crate) fn get_vm_arguments0(_this: i32) -> Result<i32> {
    create_array_of_strings(VM_ARGUMENTS.get().map(Vec::as_slice).unwrap_or(&[]))
}
pub(crate) fn get_startup_time(_this: i32) -> Result<i64> {
    Ok(VM_START.1)
}
pub(crate) fn get_uptime0(_this: i32) -> Result<i64> {
    Ok(VM_START.0.elapsed().as_millis() as i64)
}
pub(crate) fn get_available_processors(_this: i32) -> Result<i32> {
    Ok(std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1))
}
pub(crate) fn get_total_thread_count(_this: i32) -> Result<i64> {
    Ok(threads::STARTED_THREADS.load(Ordering::Relaxed) as i64)
}
pub(crate) fn get_live_thread_count(_this: i32) -> Result<i32> {
    Ok(live_threads().len() as i32)
}
pub(crate) fn get_peak_thread_count(_this: i32) -> Result<i32> {
    let live = live_threads().len();
    Ok(threads::PEAK_THREADS
        .fetch_max(live, Ordering::Relaxed)
        .max(live) as i32)
}
pub(crate) fn get_daemon_thread_count(_this: i32) -> Result<i32> {
    let mut count = 0;
    for thread in live_threads() {
        if is_daemon(thread)? {
            count += 1;
        }
    }
    Ok(count)
}
/// Counters HotSpot keeps in perf data and rusty-jvm does not track: reported as zero.
pub(crate) fn zero_counter(_this: i32) -> Result<i64> {
    Ok(0)
}

// ---- threads ----

/// The threads the management interface reports: the registered ones that have not terminated.
/// A thread is still registered for a moment after it woke its joiners, where HotSpot, which
/// finds a thread through its `eetop`, already reports none.
pub(super) fn live_threads() -> Vec<i32> {
    let mut threads = threads::parker_refs();
    threads.retain(|&thread_ref| !is_terminated(thread_ref));
    threads
}

fn is_terminated(thread_ref: i32) -> bool {
    let status = HEAP
        .get_object_field_value(thread_ref, "java/lang/Thread", "holder")
        .ok()
        .filter(|holder| holder[0] != 0)
        .and_then(|holder| {
            HEAP.get_object_field_value(holder[0], "java/lang/Thread$FieldHolder", "threadStatus")
                .ok()
        });
    status.is_some_and(|status| status[0] == threads::thread_status::TERMINATED)
}

fn is_daemon(thread_ref: i32) -> Result<bool> {
    let holder = HEAP.get_object_field_value(thread_ref, "java/lang/Thread", "holder")?[0];
    if holder == 0 {
        return Ok(false);
    }
    Ok(HEAP.get_object_field_value(holder, "java/lang/Thread$FieldHolder", "daemon")?[0] != 0)
}

fn current_thread_id() -> Result<i64> {
    let Some(thread) = JavaThread::current_thread() else {
        return Ok(-1);
    };
    let tid = HEAP.get_object_field_value(thread, "java/lang/Thread", "tid")?;
    Ok(crate::vm::helper::vec_to_i64(&tid))
}

/// `ThreadImpl.getThreads()[Ljava/lang/Thread;`
pub(crate) fn get_threads() -> Result<i32> {
    let threads = live_threads();
    let _roots = JavaThread::hold_temp_roots(threads.iter().map(|t| Slot::Ref(*t)).collect());
    HEAP.create_array_with_values("[Ljava/lang/Thread;", &threads, &mut RootedSlots::new())
}

/// CPU time of the calling OS thread in nanoseconds: `(total, user)`.
fn current_thread_cpu_nanos() -> (i64, i64) {
    #[cfg(windows)]
    {
        use winapi::shared::minwindef::FILETIME;
        use winapi::um::processthreadsapi::{GetCurrentThread, GetThreadTimes};
        let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [creation, exit, kernel, user] = &mut times;
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), creation, exit, kernel, user) };
        if ok == 0 {
            return (-1, -1);
        }
        let as_nanos = |t: &FILETIME| {
            ((((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64) * 100) as i64
        };
        let user_ns = as_nanos(&times[3]);
        (as_nanos(&times[2]) + user_ns, user_ns)
    }
    #[cfg(unix)]
    {
        use nix::libc;
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        let total = if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0
        {
            ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
        } else {
            -1
        };
        #[cfg(target_os = "linux")]
        let user = {
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            if unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) } == 0 {
                usage.ru_utime.tv_sec as i64 * 1_000_000_000 + usage.ru_utime.tv_usec as i64 * 1000
            } else {
                total
            }
        };
        #[cfg(not(target_os = "linux"))]
        let user = total;
        (total, user)
    }
}

fn thread_cpu_nanos(id: i64, user_only: bool) -> Result<i64> {
    if id != 0 && id != current_thread_id()? {
        return Ok(-1); // only the current thread's CPU time is supported
    }
    let (total, user) = current_thread_cpu_nanos();
    Ok(if user_only { user } else { total })
}

pub(crate) fn get_thread_total_cpu_time0(id: i64) -> Result<i64> {
    thread_cpu_nanos(id, false)
}
pub(crate) fn get_thread_user_cpu_time0(id: i64) -> Result<i64> {
    thread_cpu_nanos(id, true)
}
fn fill_cpu_times(ids_ref: i32, times_ref: i32, user_only: bool) -> Result<()> {
    let length = HEAP.get_array_len(ids_ref)?;
    for index in 0..length {
        let id = crate::vm::helper::vec_to_i64(&HEAP.get_array_value(ids_ref, index)?);
        let time = thread_cpu_nanos(id, user_only)?;
        HEAP.set_array_value(times_ref, index, crate::vm::helper::i64_to_vec(time))?;
    }
    Ok(())
}
pub(crate) fn get_thread_total_cpu_time1(ids_ref: i32, times_ref: i32) -> Result<()> {
    fill_cpu_times(ids_ref, times_ref, false)
}
pub(crate) fn get_thread_user_cpu_time1(ids_ref: i32, times_ref: i32) -> Result<()> {
    fill_cpu_times(ids_ref, times_ref, true)
}
pub(crate) fn get_thread_allocated_memory0(_id: i64) -> Result<i64> {
    Ok(-1)
}
pub(crate) fn get_thread_allocated_memory1(ids_ref: i32, sizes_ref: i32) -> Result<()> {
    let length = HEAP.get_array_len(ids_ref)?;
    for index in 0..length {
        HEAP.set_array_value(sizes_ref, index, crate::vm::helper::i64_to_vec(-1))?;
    }
    Ok(())
}
pub(crate) fn get_total_thread_allocated_memory() -> Result<i64> {
    Ok(-1)
}
pub(crate) fn set_thread_cpu_time_enabled0(enabled: bool) -> Result<()> {
    THREAD_CPU_TIME_ENABLED.store(enabled, Ordering::Relaxed);
    Ok(())
}
pub(crate) fn set_thread_contention_monitoring_enabled0(enabled: bool) -> Result<()> {
    CONTENTION_MONITORING_ENABLED.store(enabled, Ordering::Relaxed);
    Ok(())
}
pub(crate) fn set_thread_allocated_memory_enabled0(_enabled: bool) -> Result<()> {
    Ok(())
}
pub(crate) fn reset_peak_thread_count0() -> Result<()> {
    threads::PEAK_THREADS.store(live_threads().len(), Ordering::Relaxed);
    Ok(())
}
pub(crate) fn reset_contention_times0(_id: i64) -> Result<()> {
    Ok(())
}
pub(crate) fn get_internal_thread_count(_this: i32) -> Result<i32> {
    Ok(0)
}
pub(crate) fn get_internal_thread_times0(_this: i32, _names: i32, _times: i32) -> Result<i32> {
    Ok(0)
}

// ---- memory ----

fn memory_usage(init: i64, used: i64, committed: i64, max: i64) -> Result<i32> {
    let usage = Executor::invoke_args_constructor(
        MEMORY_USAGE,
        "<init>:(JJJJ)V",
        &[
            StackValueKind::I64(init),
            StackValueKind::I64(used),
            StackValueKind::I64(committed),
            StackValueKind::I64(max),
        ],
        None,
    )?;
    Ok(usage[0].value())
}

fn heap_usage() -> Result<i32> {
    let (max, used) = HEAP.memory();
    let max = i64::try_from(max).unwrap_or(i64::MAX);
    let used = i64::try_from(used).unwrap_or(i64::MAX).min(max);
    memory_usage(max, used, max, max)
}

fn metaspace_usage() -> Result<i32> {
    let used = CLASSES.loaded_count() as i64 * METADATA_BYTES_PER_CLASS;
    memory_usage(0, used, used, -1)
}

fn pool_usage(name: &str) -> Result<i32> {
    if name == HEAP_POOL {
        heap_usage()
    } else {
        metaspace_usage()
    }
}

/// `MemoryImpl.getMemoryUsage0(Z)Ljava/lang/management/MemoryUsage;`
pub(crate) fn get_memory_usage0(_this: i32, heap: bool) -> Result<i32> {
    if heap {
        heap_usage()
    } else {
        metaspace_usage()
    }
}

fn create_pool(name: &str) -> Result<i32> {
    let name_ref = StringPoolHelper::get_string(name)?;
    let is_heap = name == HEAP_POOL;
    let pool = Executor::invoke_static_method(
        FACTORY_HELPER,
        "createMemoryPool:(Ljava/lang/String;ZJJ)Ljava/lang/management/MemoryPoolMXBean;",
        &[
            StackValueKind::Ref(name_ref),
            (is_heap as i32).into(),
            StackValueKind::I64(-1),
            StackValueKind::I64(-1),
        ],
    )?;
    Ok(pool[0].value())
}

fn create_manager(name: &str) -> Result<i32> {
    let name_ref = StringPoolHelper::get_string(name)?;
    let manager = if name == COLLECTOR {
        let type_ref = StringPoolHelper::get_string("")?;
        Executor::invoke_static_method(
            FACTORY_HELPER,
            "createGarbageCollector:(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/management/GarbageCollectorMXBean;",
            &[StackValueKind::Ref(name_ref), StackValueKind::Ref(type_ref)],
        )?
    } else {
        Executor::invoke_static_method(
            FACTORY_HELPER,
            "createMemoryManager:(Ljava/lang/String;)Ljava/lang/management/MemoryManagerMXBean;",
            &[StackValueKind::Ref(name_ref)],
        )?
    };
    Ok(manager[0].value())
}

fn bean_array(array_class: &str, names: &[&str], create: fn(&str) -> Result<i32>) -> Result<i32> {
    let mut roots = JavaThread::hold_temp_roots(vec![]);
    let mut refs = Vec::with_capacity(names.len());
    for name in names {
        let bean = create(name)?;
        roots.add_root(Slot::Ref(bean));
        refs.push(bean);
    }
    HEAP.create_array_with_values(array_class, &refs, &mut RootedSlots::new())
}

const POOL_ARRAY: &str = "[Ljava/lang/management/MemoryPoolMXBean;";
const MANAGER_ARRAY: &str = "[Ljava/lang/management/MemoryManagerMXBean;";

fn managers_of_pool(pool: &str) -> &'static [&'static str] {
    if pool == HEAP_POOL {
        &[COLLECTOR]
    } else {
        &[METASPACE_MANAGER]
    }
}

fn pools_of_manager(manager: &str) -> &'static [&'static str] {
    if manager == COLLECTOR {
        &[HEAP_POOL]
    } else {
        &[METASPACE_POOL]
    }
}

fn bean_name(bean_ref: i32, class: &str) -> Result<String> {
    let name_ref = HEAP.get_object_field_value(bean_ref, class, "name")?[0];
    get_utf8_string_by_ref(name_ref)
}

/// `MemoryImpl.getMemoryPools0()`
pub(crate) fn get_memory_pools0() -> Result<i32> {
    bean_array(POOL_ARRAY, &[HEAP_POOL, METASPACE_POOL], create_pool)
}
/// `MemoryImpl.getMemoryManagers0()`
pub(crate) fn get_memory_managers0() -> Result<i32> {
    bean_array(
        MANAGER_ARRAY,
        &[COLLECTOR, METASPACE_MANAGER],
        create_manager,
    )
}
pub(crate) fn set_verbose_gc(_this: i32, value: bool) -> Result<()> {
    VERBOSE_GC.store(value, Ordering::Relaxed);
    Ok(())
}
pub(crate) fn set_verbose_class(_this: i32, value: bool) -> Result<()> {
    VERBOSE_CLASS.store(value, Ordering::Relaxed);
    Ok(())
}

/// `MemoryManagerImpl.getMemoryPools0()`
pub(crate) fn manager_get_memory_pools0(this: i32) -> Result<i32> {
    let name = bean_name(this, "sun/management/MemoryManagerImpl")?;
    bean_array(POOL_ARRAY, pools_of_manager(&name), create_pool)
}

const POOL_IMPL: &str = "sun/management/MemoryPoolImpl";

/// `MemoryPoolImpl.getMemoryManagers0()`
pub(crate) fn pool_get_memory_managers0(this: i32) -> Result<i32> {
    let name = bean_name(this, POOL_IMPL)?;
    bean_array(MANAGER_ARRAY, managers_of_pool(&name), create_manager)
}
pub(crate) fn pool_get_usage0(this: i32) -> Result<i32> {
    pool_usage(&bean_name(this, POOL_IMPL)?)
}
/// Peak usage is not sampled between calls, so the peak reported is the current usage.
pub(crate) fn pool_get_peak_usage0(this: i32) -> Result<i32> {
    pool_usage(&bean_name(this, POOL_IMPL)?)
}
pub(crate) fn pool_get_collection_usage0(this: i32) -> Result<i32> {
    let name = bean_name(this, POOL_IMPL)?;
    if name == HEAP_POOL {
        heap_usage()
    } else {
        Ok(0)
    }
}
pub(crate) fn pool_reset_peak_usage0(_this: i32) -> Result<()> {
    Ok(())
}
pub(crate) fn pool_set_threshold0(_this: i32, _current: i64, _new: i64) -> Result<()> {
    Ok(())
}
pub(crate) fn pool_set_sensor(_this: i32, _sensor: i32) -> Result<()> {
    Ok(())
}

/// `GarbageCollectorImpl.getCollectionCount()J`
pub(crate) fn get_collection_count(_this: i32) -> Result<i64> {
    Ok(COLLECTION_COUNT.load(Ordering::Relaxed) as i64)
}
/// `GarbageCollectorImpl.getCollectionTime()J`, in milliseconds.
pub(crate) fn get_collection_time(_this: i32) -> Result<i64> {
    Ok((COLLECTION_NANOS.load(Ordering::Relaxed) / 1_000_000) as i64)
}
