# Contributing to rusty-jvm

Thank you for your interest in contributing!
This guide explains how to build the project, run the tests, and extend the VM with new features.

---

## Table of Contents
- [Building the project](#building-the-project)
  - [Prerequisites](#prerequisites)
- [Running the tests](#running-the-tests)
- [Adding an integration test](#adding-an-integration-test)
  - [Step 1 - Write the Java program](#step-1---write-the-java-program)
  - [Step 2 - Add the test function](#step-2---add-the-test-function)
- [Writing GC-safe VM code](#writing-gc-safe-vm-code)
- [Project structure](#project-structure)

---

## Building the project

### Prerequisites

- **Rust** (stable) - install via [rustup](https://rustup.rs/)
- **JDK 25 or newer** - must be installed and `JAVA_HOME` must point to it
- **C compiler** - MSVC on Windows, Clang on macOS, or GCC/Clang on Linux
- **cargo-nextest** (recommended for running tests) - `cargo install cargo-nextest`

```sh
# Clone and build
git clone https://github.com/hextriclosan/rusty-jvm
cd rusty-jvm
cargo build --workspace
```

The JNI variadic entry points use a small C bridge for the platform's `va_list`,
argument promotions, and `va_copy`. Method lookup and execution remain in Rust and
reuse the JNI array-based entry points. This avoids requiring Rust 1.99 for its
stabilized C-variadic API. The bridge uses the JNI headers under `JAVA_HOME`.

---

## Running the tests

The test suite is an integration test that compiles a set of small Java programs
and runs them through the VM, comparing stdout with expected output.

```sh
# Run all tests. `cargo-nextest` runs each test in its own process and is required here.
cargo nextest run --workspace

# Run a specific test by name
cargo nextest run should_print_hello_world
```

> **Note:** Use `cargo nextest`, not `cargo test --workspace`. Tests share process-global state,
> including a `JAVA_HOME` `OnceLock`, and the standard runner can fail when tests run in one
> process. The integration tests also require the JDK to be on `PATH` (for `javac`) and
> `JAVA_HOME` to be set.

---

## Adding an integration test

Each integration test consists of:
1. A Java source file under `tests/test_data/`.
2. A test function in `tests/integration_tests.rs` (or one of the other `integration_tests_*.rs`
   files) that calls `assert_success(class_name, expected_output)`.

### Step 1 - Write the Java program

Create `tests/test_data/<YourExample>.java`.
The file should define a `public class <YourExample>` with a `main` method.
Place it in the appropriate package under `samples/` if it is more than a trivial snippet.
No need to compile it manually, the test runner will do that for you.

```java
// tests/test_data/samples/javacore/trivial/HelloContributor.java
package samples.javacore.trivial;

public class HelloContributor {
    public static void main(String[] args) {
        System.out.println("Hello, contributor!");
    }
}
```

### Step 2 - Add the test function

Open `tests/integration_tests.rs` and add:

```rust
#[test]
fn hello_contributor() {
    assert_success(
        "samples.javacore.trivial.HelloContributor",
        "Hello, contributor!\n",
    );
}
```

`assert_success` is defined in `tests/utils.rs`; it invokes `cargo run` with the given class
name and asserts that stdout equals the expected string.

---

## Writing GC-safe VM code

The heap is managed by a precise, stop-the-world mark-sweep collector (see
[docs/garbage_collection.md](docs/garbage_collection.md)). A collection can run at any safepoint,
so VM code that holds heap references (`i32` refs) in Rust locals must follow these rules:

- **Root what you hold.** A reference that lives only in a Rust local is invisible to the
  collector. Allocate into a `RootedSlots` destination created *before* the allocation, or keep
  temporaries alive with `JavaThread::hold_temp_roots` for the span that needs them.
- **Never block while Running.** Wrap blocking I/O, sleeps and other waits in
  `safepoint::blocking(|| ...)` so a collection can proceed without this thread.
- **Lock VM mutexes through the safepoint helpers.** Use `safepoint::lock` / `safepoint::wait`
  instead of `Mutex::lock` / `Condvar::wait`, so a thread waiting for a lock does not stall a
  safepoint held by the lock owner.
- **Keep hot paths lock-free.** The interpreter calls into class lookup, allocation and frame
  management on almost every bytecode; a global lock there serializes all Java threads. Check an
  already-published state first and only fall back to a claim or lock on the slow path.
- **Map allocation failures precisely.** Only real exhaustion (`AllocError::Capacity`,
  `HostAllocation`, `IdentityExhausted`, `Arithmetic`) becomes `OutOfMemoryError`; anything else is
  a VM bug and surfaces as `InternalError`.

For stress testing, run a test with `-XX:+GCStressAtSafepoints`, which collects at every safepoint
poll and quickly exposes unrooted references.

---

## Project structure

```
rusty-jvm/
├── src/
│   ├── lib.rs                # Public API: Arguments, run()
│   └── vm/
│       ├── execution_engine/ # Bytecode interpreter & opcode processors
│       ├── heap/             # Object and array storage
│       ├── gc/               # Mark-sweep collector, roots, heap budget (-Xmx)
│       ├── safepoint/        # Safepoint coordinator and thread-state transitions
│       ├── method_area/      # Class loading, vtable, method resolution
│       ├── stack/            # Operand stack and call frames
│       ├── jni/              # JNI ABI bridge
│       ├── system_native/    # Native method implementations & dispatch table
│       ├── exception/        # Exception creation and propagation
│       ├── properties/       # System properties and classpath resolution
│       └── validation/       # Bytecode pre-checks
├── tests/
│   ├── test_data/            # Java source and .class files for integration tests
│   ├── integration_tests.rs  # Main integration test suite
│   └── utils.rs              # Test helpers (assert_success, etc.)
├── jclassfile/               # Sub-crate: .class file parser
├── jclassmodel/              # Sub-crate: runtime-friendly view over a parsed class file
├── jdescriptor/              # Sub-crate: JVM type descriptor parser
├── jimage-rs/                # Sub-crate: JDK jimage archive reader
├── jniname/                  # Sub-crate: JNI name mangling utilities
└── docs/                     # Design docs and architecture diagrams
```
