# Handoff: make rusty-jvm match or beat HotSpot

You are taking over performance work on **rusty-jvm**, a JVM written from scratch in Rust
(`/home/yeetmasta/Github/rusty-jvm`, branch `integration/gc-jni`). The project's priorities, in
order: **full parity with HotSpot (JDK 25) first, then speed.** No speed-up may change observable
behavior. That includes exact floating-point bits, exception types and messages, stack traces and
system properties.

Work autonomously. The user does not want to be asked before each step: finish a step, verify it
and go on to the next. Ask only when you need a decision only the user can make. Do not commit,
push or open PRs unless the user asks. Everything below is currently **uncommitted** in the
working tree.

---

## 1. Environment

- Rust nightly 1.102, Linux x86-64, 32 cores. The crate's `rust-version` is now `1.97` (needed
  by Cranelift 0.137). CI uses `dtolnay/rust-toolchain@stable` and also cross-builds s390x
  (big-endian).
- JDK: `export JAVA_HOME=/usr/lib/jvm/temurin-25-jdk-amd64`. **Every** `cargo build/test` needs it
  (the build script compiles Java test data), and so does running the VM.
- `perf` works (`kernel.perf_event_paranoid=1` was set by the user). A release build with line
  tables for profiling:
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo build --release --target-dir target/prof`
- Never change system or security settings yourself. Ask the user.

## 2. Verification commands

```sh
export JAVA_HOME=/usr/lib/jvm/temurin-25-jdk-amd64
cargo build --release
cargo test --test integration_tests          # 302 tests; must all pass
cargo test --lib                              # see "known failures" below
python3 bench/run.py                          # quick benchmark suite vs HotSpot
python3 bench/run.py --vms rusty --baseline target/bench/results/20261115-164231-quick.json
target/release/rusty-jvm -XX:+PrintCompilation -cp target/bench/classes Fib 3 27
```

**Known pre-existing failures:** `cargo test --lib` fails about 39 tests (GC, JNI and budget tests)
on clean `HEAD` too. The failing set changes between runs, and they still fail with
`--test-threads=1`. Individual ones pass when run alone, so they likely depend on run order or
leftover global VM state. Do not treat them as regressions. Compare the failing set before and
after your change (`git stash` to get the baseline). Fixing their isolation is a worthwhile
separate task.

**Parity checks:** for any change that affects results, write a Java program under
`tests/test_data/` (package `samples.…`), run it on HotSpot and rusty-jvm and diff the output.
Then add it to `tests/integration_tests.rs` with HotSpot's output as the expected string. See
`should_compute_math_intrinsics_bit_for_bit_as_hotspot` and `tests/test_data/MathIntrinsics.java`
for the pattern (print raw bits, hash them, call each site twice so warm paths run). Also run
with `-Xint` vs default to make sure the JIT changes nothing.

## 3. What has been done this session (uncommitted)

1. **Benchmark harness.** See `bench/java/*.java` (12 programs plus the `Bench.java` driver),
   `bench/run.py` (Python stdlib only) and `docs/benchmarking.md`. It compares `rusty` with
   HotSpot `int` (`-Xint`), `c1` (`-XX:TieredStopAtLevel=1`) and `c2` (default). It reports
   wall/first/best/RSS, checks checksums against HotSpot and saves JSON to
   `target/bench/results/`. `bench/**` is excluded from the published crate. The baseline is
   `target/bench/results/20261115-164231-quick.json`. Geometric-mean slowdown at best iteration:
   6.7× vs `-Xint`, 68× vs C1, 96× vs C2. With JIT v1 (`20261115-174733-quick.json`): 4.3×,
   45×, 67×. The quick preset is too short for C2 peak, so use
   `--preset full` for those numbers.
2. **Arithmetic intrinsics** (`src/vm/execution_engine/intrinsics.rs`, `Intrinsic::Arithmetic`).
   - Covered: `Math`/`StrictMath` `sqrt`, `abs`, `min`, `max`, `fma`, `floor`, `ceil`, `rint`
     and `round`, plus `FdLibm$Sqrt.compute` and `Integer`/`Long` `numberOfLeadingZeros`,
     `numberOfTrailingZeros` and `bitCount`.
   - Why: JDK 25's `StrictMath.sqrt` is pure Java that works one bit at a time (6.5µs per call
     here). This made NBody 93% faster.
   - Bit-exact with HotSpot `-Xint` and C2:
     - `fma` uses the hardware semantics, because HotSpot's interpreter intrinsifies it (NaN
       payloads pass through). The Java source would return the canonical NaN.
     - `floor`/`ceil`/`rint` given a NaN decline, because hardware rounding would quiet a
       signaling NaN.
   - Deliberately **not** intrinsified: `sin`/`cos`/`tan`/`exp`/`log`/`pow`, because they are
     not bit-exact.
3. **Startup parity fix.** On Linux, `os.name` and `os.version` now come from `uname()`, as
   HotSpot's `java_props_md.c` does. The change is in
   `src/vm/system_native/properties_provider/properties.rs`.
   - Before, `os_info` spawned `lsb_release`, `getopt`, `tr`, `cut` and `getconf` at every start
     and reported the *distribution* version (`26.4.0`) instead of the kernel release.
   - Hello-world wall time went from 85ms to 66ms.
   - `os_info` is now a non-Linux-only dependency, and `nix` gained its `feature` feature.
   - The integration test now compares `os.version` against `uname -r` on Linux.
   - Known remaining parity bug, not fixed: on macOS rusty-jvm reports `os.name="macOS"`, but
     HotSpot reports `"Mac OS X"`.
4. **Baseline JIT, v1** (`src/vm/jit/mod.rs`, `src/vm/jit/compiler.rs`, Cranelift 0.137). Read
   both files' module docs first.
   - **Contract:** compiled code is a drop-in for `StackFrame::run_frame_local` (in
     `src/vm/stack/stack_frame_fast.rs`). It runs frame-local instructions on the frame's own
     slots, stops at the same points and returns `(ran, FrameLocalStop)`. It may decline at
     any pc, and it can be entered at any instruction start through a `Switch` on the entry pc.
     As a result, calls, returns, exceptions, monitors, safepoints and GC root scans are all
     unchanged and still handled by the interpreter.
   - **Hook:** `StackFrame::run_local` in `stack_frame_fast.rs` calls the compiled entry if
     there is one, else interprets and counts. `frame_local_calls::run` now calls `run_local`.
   - **Compilation trigger:** after `-XX:CompileThreshold=` (default 10,000) frame-local
     instructions have run interpreted in a method. Compilation runs synchronously under a
     global `parking_lot::Mutex`. Per-method state is `FrameCode.jit: JitState`.
   - **Options:** `-Xint` disables the JIT, and `-XX:+PrintCompilation` prints each method as
     it is compiled.
   - **Native code:**
     - constants, loads/stores, stack ops, int/long/float/double arithmetic, shifts and logic;
     - div/rem with Java's `MIN/-1` result (exits to the interpreter on a zero divisor);
     - conversions with saturating `f2i`/`d2i`;
     - `lcmp`/`fcmp*`/`dcmp*`, all branches, `tableswitch`/`lookupswitch` and `goto_w`.
   - **Through the `jit_step` callback** (runs `run_frame_local(1)` for one instruction): field
     and array access, `ldc`, `checkcast`, `instanceof`, `arraylength`.
   - **Exits:** invokes and returns exit with `STOP_INVOKE` / `STOP_RETURN+n`.
   - **Budget:** checked at backward branches only.
   - **`Slot` layout:** `Slot` is now `#[repr(u32)]` (`Value = 0`, `Ref = 1`): a `u32` tag at
     offset 0 and the `i32` at offset 4. Compiled code writes tags exactly as the interpreter
     does.
   - **Results so far** (quick sizes, checksums correct): Fib 34.5ms to 22.6ms, Sieve 219ms to
     130ms, Fannkuch 65.7ms to 56ms.
   - **Verified** (section 4.1 is done): integration tests pass 303/303 at the default threshold
     and with `RUSTY_JVM_COMPILE_THRESHOLD=1` (release build); the lib-test failing set is the
     same 38 tests as on clean `HEAD`.
5. **Background compilation.** Methods compile on a "JIT Compiler" thread and stay interpreted
   until the code is ready; `-Xbatch` / `-XX:-BackgroundCompilation` compile in place. Compiling
   costs 0.3ms for the smallest method and 5-10µs per bytecode byte, which added 32ms to hello
   world when it was synchronous. Cranelift's verifier is off in release builds.
6. **NaN parity** (`src/vm/execution_engine/float_arithmetic.rs`), found by the opcode test and
   wrong in the interpreter before the JIT existed:
   - `fadd`/`fmul`/`dadd`/`dmul`: of two NaN operands HotSpot's x86-64 interpreter returns the
     top of the stack (`addss xmm0, [rsp]`). The JIT passes the operands to Cranelift in that
     order; the interpreter tests for NaN explicitly.
   - `frem`/`drem`: HotSpot's `fmod` stub has three variants chosen by CPU (AVX-512, FMA, x87).
     The first two treat NaNs alike; the rules for both groups are implemented and were derived
     from `stubGenerator_x86_64_fmod.cpp` and checked against `java -Xint` (FMA) and
     `java -Xint -XX:UseAVX=0` (x87). The AVX-512 variant was read, not run (no such CPU here).
   - HotSpot's C2 returns other NaNs than its interpreter for the same expression, so parity
     tests on NaN bits must run the reference with `-Xint`.
7. **Tests added:** `tests/test_data/JitOpcodes.java` with
   `should_run_compiled_instructions_bit_for_bit_as_hotspot_interprets_them` (compares with the
   reference JVM run with `-Xint`). `swap`, `nop`, `goto_w` and `dup` forms on two-slot values
   are not covered, because `javac` does not emit them. `ThreadInfoDemo.java` had a race (a
   thread is `WAITING` on the latch before it is on the lock); fixed in the test program.
8. **`ThreadMXBean` race fixed** (`management::live_threads`): a thread stayed listed between
   waking its joiners and unregistering, so `getThreadInfo` after `join()` sometimes returned a
   `TERMINATED` thread where HotSpot returns null. Terminated threads are now left out.

## 4. Next steps, in order

### 4.1 Finish and verify JIT v1 (DONE; kept for reference)

To rerun the strongest check:
`cargo build --release -p jni_test_lib && RUSTY_JVM_COMPILE_THRESHOLD=1 cargo test --release --test integration_tests`
(a debug build takes minutes per program at threshold 1). Do not run `cargo fmt` on the whole
workspace: it rewrites about 25 files that are not formatted with it; use `rustfmt <file>`.

1. **Silence Cranelift's logging.** The VM's tracing subscriber prints `INFO defining function …`
   with the whole IR of every compiled method, which pollutes program output. Find where the
   `tracing_subscriber` filter is built (grep `EnvFilter` / `tracing_subscriber`). Default
   `cranelift_*` targets to `warn`, unless `RUST_LOG` asks otherwise.
2. Run `cargo test --test integration_tests` and the lib-test comparison. Then rerun with
   `-XX:CompileThreshold=1` so nearly every method is compiled. The tests launch the binary, so
   look in `tests/utils.rs` for how to pass VM options, perhaps via an env var such as
   `RUSTY_JVM_TEST_OPTS` that you add. Every test must pass at threshold 1. That is the
   strongest check that compiled code matches the interpreter.
3. Write a `tests/test_data` program that exercises every natively compiled opcode with edge
   values. Include the NaN compares, `MIN_VALUE / -1`, `% -1`, shifts of 32/64+ distance,
   `f2i`/`d2l` of NaN and infinities, `i2c` of negatives, every `dup*` form and switches with
   negative and sparse keys. Run it in hot loops so it compiles, and compare against HotSpot.
4. Run `python3 bench/run.py` and record the new baseline in `docs/benchmarking.md`.
5. Add a short section on the JIT to `docs/architecture.md` and an entry to `CHANGELOG.md`
   under `# Unreleased`. Document the new flags in `src/cli/help.rs`.

### 4.2 JIT v2: registers instead of memory (largest remaining interpreter-loop cost)
- **Analysis pass:** at each pc, compute the static stack depth and the type of each local and
  stack slot (int/long/float/double/ref/top). Use a dataflow over the CFG with JVMS
  verification-by-inference rules. Exception handlers start with depth 1 holding a ref.
- **Codegen:** keep locals and stack slots in Cranelift `Variable`s. Write them to the frame,
  with the correct tags, only at exits, before `jit_step` calls, and before anything that can
  reach a safepoint or GC. Load them on entry. Entry at an arbitrary pc then loads only that
  pc's live slots.
- **Design constraints worked out so far:**
  - Slot tags must be written back exactly as the interpreter writes them, dead slots included:
    the collector reads them, and a `Ref` tag on an int would make it follow a number. Where the
    static type of a local differs between paths, keep the tag in a variable too.
  - Static stack depth past an invoke, a field access or an `ldc` needs the constant pool's
    descriptors (`FrameCode.klass`).
  - Entering at any pc does not fit SSA. Enter only at pc 0, exception handlers, loop headers
    and the instruction after each one the code stops at; publish that set with the code. When a
    frame is elsewhere, `run_local` should single-step the interpreter to the next entry pc
    (today it interprets a whole budget, which could keep a hot loop out of its code).
  - In debug builds, assert the analysis' depth against `io.top` at every instruction, and run
    the suite at threshold 1 before generating code from it.
- **Expected gain:** 3–10× on loop-heavy code (Sieve, SpectralNorm, Fannkuch, NBody).
- **Also look at compile time** while restructuring: v1 emits enough code per bytecode that
  `define_function` takes 5-10µs per bytecode byte.

### 4.3 JIT: inline the heap operations
Replace the `jit_step` callbacks with direct `extern "C"` helpers that take resolved operands:
- array load/store (type-specialized; no `run_frame_local` dispatch);
- `getfield`/`putfield`, using the resolved offset from `JavaClass::resolved_member_ref` at
  compile time when already resolved, else fall back to the step;
- `getstatic`/`putstatic` (resolved `StaticField`);
- `arraylength`.

NBody currently runs slower compiled (266ms) than interpreted (241ms) because of these
callbacks.

Keep the exact null and bounds behavior: exit *before* the instruction, so the handler throws
with the right bci (JEP 358 helpful NPE messages depend on `bci`).

### 4.4 JIT: compiled-to-compiled calls
At this point every invoke exits to `frame_local_calls`.
- **What to add:** for warm `invokestatic`/`invokespecial`, and for monomorphic
  `invokevirtual` using the existing inline caches (`cached_virtual_target`), have compiled
  code push the callee frame through a helper and run the callee's compiled code directly. A
  callee that returns pops its frame and pushes the result.
- **Must stay correct:** stack depth limits (`stack_limit::would_overflow`), `ex_pc`/`bci`
  bookkeeping (see `frame_local_calls::invoke`), synchronized/native targets (exit for these),
  and intrinsics (`JavaMethod::intrinsic`).
- **Why:** Fib is still 45× slower than C2 because of call overhead.

### 4.5 Phase 1: object model (biggest structural win; prerequisite for C1/C2-class speed)
- **Today:** every heap object lives in `ObjectTable` (`src/vm/heap/object_table.rs`), with a
  per-slot `parking_lot::RwLock`. References are dense `i32` IDs that are never reused, and
  objects are `JavaInstance` enums with `Box<[AtomicI32]>` fields. Arrays are
  `Array { type_name: String, data: Vec<u8> }`, which pays a division by a runtime element
  width and a string match on every access.
- **Quick win first:** cache the array element width as a field.
- **Then:**
  - Allocate objects in a raw heap with a header (mark word + klass pointer) and inline
    fields/elements.
  - Keep `i32` refs as compressed oops (base + offset×8), so `Slot` stays 8 bytes.
  - Add thread-local TLAB bump allocation.
  - Put thin locks and the identity hash in the mark word.
- **Sequencing:** do it behind the `Heap` API (`read_field_at`, `write_field_at`,
  `load_array_element`, `store_array_element`, `get_array_len`, `create_instance`, …). Migrate
  one object kind at a time, keeping all tests green.
- **Constraints:** HPROF dumps (`heap/hprof.rs`), JNI opaque tokens (`jni/handles.rs`) and the
  GC root contracts (`docs/garbage_collection.md`) must keep working.

### 4.6 Phase 2: moving generational GC
Copying young generation plus mark-compact old, with a card-marking write barrier (emitted by
the JIT for reference stores). The JIT must publish stack maps (or spill refs to frame slots) at
every safepoint, call and poll. The existing safepoint coordinator
(`src/vm/safepoint/coordinator.rs`) and precise root snapshots are the base.

### 4.7 Phase 3 extras: profiling and tiers
- **Profiles:** type profiles at virtual call sites and type checks, and branch profiles.
- **Background compilation:** compile on background threads instead of synchronously.
- **OSR:** comes for free with the current any-pc-entry design.

### 4.8 Phase 4: optimizing tier
Write an SSA IR above Cranelift. The optimizations, in priority order:
1. Profile-guided inlining.
2. CHA with dependency invalidation when classes load.
3. Speculation with deoptimization to interpreter frames. The any-pc-entry design is already
   the deopt target: write the frame state and return.
4. Escape analysis and scalar replacement.
5. Range-check and null-check elimination.
6. Loop optimizations.
7. Lock elision.

More intrinsics:
- `System.arraycopy`, `String.equals`/`indexOf`/`compareTo`, `StringLatin1`/`StringUTF16`
  helpers, `Arrays.fill`/`equals`/`mismatch`;
- `Unsafe` get/put/CAS, CRC32, `Thread.currentThread`, `Object.hashCode`/`identityHashCode`.

Each one is bit-exact against HotSpot and has a parity test.

### 4.9 Phase 5: startup
A CDS-like archive of parsed class metadata (jimage parsing and class building dominate
startup), an archived heap, and cached `invokedynamic` call sites and lambda forms. Hello world
takes 66ms vs HotSpot's 15ms, with RSS 129MB vs 38MB.

## 5. Findings and decisions to keep

- douchuan/jvm (github.com/douchuan/jvm, MIT) was reviewed, and nothing in it is worth porting:
  - LLVM/inkwell, compiled at O0 with no pass pipeline;
  - compiles on first call, one module per thread;
  - runtime helpers resolve the constant pool on every execution;
  - no GC, safepoints or deopt.
- Cranelift was chosen over LLVM because it is pure Rust, compiles fast, has no system LLVM
  dependency and supports s390x. An LLVM tier can come much later for peak.
- HotSpot's `-Xint` intrinsifies `Math.sqrt`, `fma`, `abs` and a few others in the template
  interpreter. Parity means matching **HotSpot's actual output**, not the Java source, where
  they differ.
- The engine loop (`src/vm/execution_engine/engine.rs`) runs `frame_local_calls::run` with a
  budget, then one handler instruction, then `safepoint::dispatch_bytecodes(ran)`. Keep budget
  semantics so safepoints stay bounded.
- Write comments in the codebase's style: full sentences explaining *why*, with doc comments on
  every item. Never delete existing comments.
