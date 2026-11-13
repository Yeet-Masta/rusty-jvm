//! Garbage collection, managed-heap admission and GC-related VM options, run through the real CLI.
//!
//! Tests that use `-XX:+GCStressAtSafepoints` collect every 4096 bytecodes, including through JDK
//! bootstrap, which makes them very slow in debug builds; they are ignored by default and run with
//! `cargo nextest run --run-ignored all` (or `cargo test -- --include-ignored`).

mod utils;

use std::collections::HashMap;
use std::process::{Command, Output};
use std::time::Duration;
use utils::get_command_with_vm_options;

#[ctor::ctor(unsafe)]
fn before_tests() {
    let status = Command::new("cargo")
        .args(["build", "-p", "jni_test_lib"])
        .status()
        .expect("failed to run cargo build");
    assert!(status.success(), "failed to build jni_test_lib");
}

fn library_path() -> String {
    format!("-Djava.library.path={}", env!("JNI_TEST_LIB_PATH"))
}

/// Runs `entry`, failing a run that hangs instead of waiting for it forever.
fn run(options: &[&str], entry: &str, args: &[&str], deadline: Duration) -> Output {
    get_command_with_vm_options(options, entry, args)
        .timeout(deadline)
        .output()
        .expect("failed to launch rusty-jvm")
}

/// Normalized like the rest of the suite: Java's `println` ends lines with `\r\n` on Windows.
fn text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).replace("\r\n", "\n")
}

fn assert_succeeded(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
}

fn assert_quiet_success(output: &Output, expected_stdout: &str) {
    assert_succeeded(output);
    assert_eq!(text(&output.stdout), expected_stdout);
    assert!(output.stderr.is_empty(), "stderr: {}", text(&output.stderr));
}

/// Asserts at least `minimum` explicit collections that reclaimed something, from `-Xlog:gc`.
fn assert_reclaiming_collections(output: &Output, minimum: usize) {
    let stderr = text(&output.stderr);
    let reclaiming = stderr
        .lines()
        .filter(|line| line.contains("[gc] Explicit:"))
        .filter_map(|line| line.split("reclaimed=").nth(1))
        .filter_map(|reclaimed| reclaimed.split_whitespace().next())
        .filter_map(|bytes| bytes.parse::<u64>().ok())
        .filter(|&bytes| bytes > 0)
        .count();
    assert!(
        reclaiming >= minimum,
        "expected {minimum} reclaiming cycles, saw {reclaiming}; stderr={stderr}"
    );
}

// ------------------------------------------------------------------------------------- options

const PROGRAM_ARGS: &str = "samples.system.programargs.ProgramArgsExample";

#[test]
fn should_reject_invalid_heap_option_before_entry() {
    let output = run(&["-Xmx12q"], PROGRAM_ARGS, &[], Duration::from_secs(180));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("-Xmx12q"), "unexpected stderr: {stderr}");
}

#[test]
fn should_pass_heap_like_argument_after_entry_to_java() {
    let output = run(&[], PROGRAM_ARGS, &["-Xmx12q"], Duration::from_secs(180));
    assert_quiet_success(&output, "[-Xmx12q]\n");
}

#[test]
fn should_accept_valid_heap_option_before_entry() {
    let output = run(&["-Xmx64m"], PROGRAM_ARGS, &[], Duration::from_secs(180));
    assert_quiet_success(&output, "[]\n");
}

#[test]
fn should_accept_gc_logging_option_before_entry_without_emitting_a_fake_event() {
    let output = run(&["-Xlog:gc"], PROGRAM_ARGS, &[], Duration::from_secs(180));
    assert_quiet_success(&output, "[]\n");
}

#[test]
fn should_accept_gc_allocation_threshold_option_before_entry() {
    let output = run(
        &["-XX:GCAllocThreshold=100000"],
        PROGRAM_ARGS,
        &[],
        Duration::from_secs(180),
    );
    assert_quiet_success(&output, "[]\n");
}

#[test]
#[ignore = "GC stress mode collects every 4096 bytecodes and is slow in debug builds"]
fn should_accept_gc_stress_option_before_entry() {
    let output = run(
        &["-XX:+GCStressAtSafepoints"],
        PROGRAM_ARGS,
        &[],
        Duration::from_secs(1800),
    );
    assert_quiet_success(&output, "[]\n");
}

// ------------------------------------------------------------------------------------- budget

#[test]
fn should_enforce_budget_in_bytecodes_reflection_clone_and_emergency_handlers() {
    utils::assert_success(
        "samples.gc.GcBudgetAllocationDemo",
        "negative-dimensions-ok\noversized-arrays-ok\nclone-ok\n",
    );
}

#[test]
fn should_fail_startup_explicitly_when_heap_is_too_small_for_emergency_oom() {
    let output = run(
        &["-Xmx1"],
        "samples.gc.GcBudgetAllocationDemo",
        &[],
        Duration::from_secs(180),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("too small for emergency OutOfMemoryError bootstrap"),
        "{stderr}"
    );
}

#[test]
fn should_recover_from_out_of_memory_in_arrays_clone_jni_and_concurrent_reservations() {
    let library = library_path();
    let output = run(
        &["-Xmx64m", &library],
        "samples.gc.GcBudgetDemo",
        &[],
        Duration::from_secs(300),
    );
    assert_succeeded(&output);
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("bootstrap-used="), "{stdout}");
    for path in [
        "reflective-array-oom-recovered",
        "jni-array-oom-recovered",
        "concurrent-oom-recovered",
        "clone-oom-recovered",
        "recovery-oom-recovered",
    ] {
        assert!(
            stdout.lines().any(|line| line == path),
            "missing {path}: {stdout}"
        );
    }
}

// ------------------------------------------------------------------------------------- collection

fn run_collect_demo(options: &[&str], mode: &str, deadline: Duration) -> Output {
    run(options, "samples.gc.GcCollectDemo", &[mode], deadline)
}

#[test]
fn should_reclaim_on_system_gc_without_stderr() {
    let output = run_collect_demo(&["-Xmx64m"], "basic", Duration::from_secs(180));
    assert_quiet_success(&output, "collect-ok\n");
}

#[test]
fn should_log_collections_with_xlog_gc() {
    let output = run_collect_demo(&["-Xmx64m", "-Xlog:gc"], "basic", Duration::from_secs(180));
    assert_succeeded(&output);
    assert_eq!(text(&output.stdout), "collect-ok\n");
    let stderr = text(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("[gc] Explicit:")),
        "{stderr}"
    );
    assert!(stderr.contains("reclaimed="), "{stderr}");
}

#[test]
fn should_log_explicit_collection_only_when_requested() {
    let quiet = run(&[], "samples.gc.GcLogDemo", &[], Duration::from_secs(180));
    assert_quiet_success(&quiet, "gc-log-ok\n");

    let logged = run(
        &["-Xlog:gc"],
        "samples.gc.GcLogDemo",
        &[],
        Duration::from_secs(180),
    );
    assert_succeeded(&logged);
    assert_eq!(text(&logged.stdout), "gc-log-ok\n");
    let stderr = text(&logged.stderr);
    assert!(
        stderr.lines().any(|line| line.starts_with("[gc]")),
        "{stderr}"
    );
}

#[test]
fn should_deliver_weak_phantom_soft_references_and_cleaners() {
    let output = run_collect_demo(&["-Xmx64m"], "references", Duration::from_secs(180));
    assert_quiet_success(&output, "references-ok\n");
}

#[test]
fn should_process_references_and_cleaners_with_jdk_semantics() {
    let output = run(
        &["-Xmx64m"],
        "samples.gc.GcReferencesDemo",
        &[],
        Duration::from_secs(180),
    );
    assert_quiet_success(&output, "references-ok\n");
}

#[test]
fn should_clear_soft_references_under_pressure_and_recover_from_oom() {
    let output = run_collect_demo(&["-Xmx16m"], "pressure", Duration::from_secs(180));
    assert_quiet_success(&output, "pressure-ok\n");
}

#[test]
#[ignore = "GC stress mode collects every 4096 bytecodes and is slow in debug builds"]
fn should_keep_live_objects_across_engine_and_native_stress_boundaries() {
    // Over 11,000 collections in a debug build; the deadline only detects a hang.
    let output = run_collect_demo(
        &[
            "-Xmx64m",
            "-XX:GCAllocThreshold=128",
            "-XX:+GCStressAtSafepoints",
        ],
        "basic",
        Duration::from_secs(1800),
    );
    assert_quiet_success(&output, "collect-ok\n");
}

#[test]
fn should_keep_a_fixed_live_graph_across_increasing_lifetime_keys() {
    let cycles = if utils::is_bigendian() { 12 } else { 48 };
    let cycle_argument = cycles.to_string();
    let output = run(
        &["-Xmx64m", "-XX:GCAllocThreshold=128"],
        "samples.gc.GcCyclesDemo",
        &[&cycle_argument],
        Duration::from_secs(600),
    );
    assert_succeeded(&output);
    let stdout = text(&output.stdout);
    let observed = stdout
        .lines()
        .filter(|line| line.starts_with("cycle="))
        .count();
    assert_eq!(observed, cycles, "{stdout}");
    assert!(stdout.ends_with("cycles-ok\n"), "{stdout}");
    assert!(output.stderr.is_empty(), "stderr: {}", text(&output.stderr));
}

#[test]
fn should_match_jdk_reflect_array_semantics_under_collection() {
    let output = run(
        &["-Xmx64m", "-XX:GCAllocThreshold=128"],
        "samples.gc.GcCyclesDemo",
        &["reflection"],
        Duration::from_secs(300),
    );
    assert_quiet_success(&output, "reflect-array-ok\n");
}

#[test]
fn should_initialize_classes_recursively_and_under_contention() {
    utils::assert_success(
        "samples.gc.GcCoordinatorInitDemo",
        "recursive-ok\ncontended-ok\nfailure-ok\n",
    );
}

#[test]
fn should_keep_references_across_java_arguments_results_exceptions_and_allocations() {
    utils::assert_success("samples.gc.RefTransfer", "ref-transfer-ok\n");
}

// ------------------------------------------------------------------------------------- threads

fn run_thread_fixture(entry: &str, args: &[&str]) -> Output {
    let class_path = utils::TEST_PATH
        .canonicalize()
        .expect("compiled Java fixture directory is missing")
        .to_string_lossy()
        .into_owned();
    let library = library_path();
    run(
        &[
            "-Xmx64m",
            "-Xlog:gc",
            "-XX:GCAllocThreshold=1024",
            "-cp",
            &class_path,
            &library,
        ],
        entry,
        args,
        Duration::from_secs(300),
    )
}

fn assert_thread_fixture(output: &Output, token: &str, minimum_reclaiming_cycles: usize) {
    assert_succeeded(output);
    let stdout = text(&output.stdout);
    assert!(stdout.contains(token), "expected {token:?} in {stdout:?}");
    assert_reclaiming_collections(output, minimum_reclaiming_cycles);
}

#[test]
fn should_keep_every_root_kind_across_reclaiming_collections() {
    let output = run_thread_fixture("samples.gc.GcRootsDemo", &[]);
    assert_thread_fixture(&output, "roots-ok", 12);
}

#[test]
fn should_finish_blocked_thread_and_class_loading_races_under_collection() {
    let output = run_thread_fixture("samples.gc.GcThreadsDemo", &[]);
    assert_thread_fixture(&output, "threads-ok", 2);
}

#[test]
fn should_load_classes_concurrently_under_collection() {
    let output = run_thread_fixture("samples.gc.GcThreadsDemo", &["class-loading"]);
    assert_thread_fixture(&output, "class-loading-ok", 1);
}

#[test]
fn should_register_cleaners_concurrently_under_collection() {
    let output = run_thread_fixture("samples.gc.GcThreadsDemo", &["cleaner"]);
    assert_thread_fixture(&output, "cleaners-ok", 1);
}

// ------------------------------------------------------------------------------------- JNI

fn run_jni_gc_demo(log: bool) -> Output {
    let library = library_path();
    let mut options = vec![
        "-Xmx64m",
        "-XX:GCAllocThreshold=128",
        "-Djava.system.class.loader=samples.gc.GcJniDemo$TestSystemClassLoader",
        &library,
    ];
    if log {
        options.push("-Xlog:gc");
    }
    run(
        &options,
        "samples.gc.GcJniDemo",
        &[],
        Duration::from_secs(300),
    )
}

#[test]
fn should_keep_jni_reference_handoffs_and_buffer_leases_across_collection() {
    let output = run_jni_gc_demo(true);
    assert_succeeded(&output);
    assert_eq!(
        text(&output.stdout),
        "find-class-subclass-ok\nreferences-ok\nstring-lease-ok\nlease-gc-ok\nrelease-modes-ok\njni-gc-ok\n"
    );
    let stderr = text(&output.stderr);
    assert!(
        stderr.lines().any(|line| line.contains("[gc] Explicit:")),
        "expected explicit collection under JNI references: {stderr}"
    );
}

#[test]
fn should_keep_nested_jni_pointer_returns_and_exceptions_across_collection() {
    let output = run_jni_gc_demo(false);
    assert_succeeded(&output);
    assert!(text(&output.stdout).ends_with("jni-gc-ok\n"));
}

#[test]
fn should_preserve_independent_jni_identities_and_native_handoffs() {
    let library = library_path();
    utils::assert_with_all_args(
        &[&library],
        "samples.gc.GcJniIdentityDemo",
        &[],
        "aliases-ok\nframes-ok\ncapacity-ok\nbuffers-ok\ncallbacks-ok\n",
        "WARNING: A restricted method in java.lang.System has been called\n\
WARNING: java.lang.System::loadLibrary has been called by samples.gc.GcJniIdentityDemo in an unnamed module\n\
WARNING: Use --enable-native-access=ALL-UNNAMED to avoid a warning for callers in this module\n\
WARNING: Restricted methods will be blocked in a future release unless native access is enabled\n\n",
        utils::ExecutionResult::Success,
        0,
        HashMap::new(),
    );
}
