# Unreleased

## What's Changed
* Add a precise, nonmoving mark-sweep garbage collector with managed-heap limits, JNI reference
  tracking, Java reference processing and opt-in `-Xlog:gc` output
* Support `ACC_SYNCHRONIZED` on interpreted methods and bytecode-invoked native methods
* Per-class-loader namespaces: user-defined loaders define their own copies of a class, resolve
  the classes those refer to, and may have a null parent; duplicate definitions throw `LinkageError`
* Hidden classes are marked as such (`Class.isHidden`, nest hosts from the `NestHost` attribute)
* `ThreadMXBean.getThreadInfo`/`dumpAllThreads` with lock owners, locked monitors and synchronizers,
  deadlock detection, and stack traces of blocked threads (`Thread.getAllStackTraces`)
* Heap dumps in the HPROF format through `HotSpotDiagnosticMXBean.dumpHeap`, and the VM options
  this VM acts on through `getVMOption`
* `Math.sqrt`, `abs`, `min`, `max`, `fma`, `floor`, `ceil`, `rint` and `round`, and
  `Integer`/`Long` `numberOfLeadingZeros`, `numberOfTrailingZeros` and `bitCount`, run natively
  with HotSpot's results, bit for bit
* On Linux, `os.name` and `os.version` come from `uname` as in HotSpot (`os.version` is the kernel
  release, not the distribution's version), and starting the VM no longer runs `lsb_release`
* A baseline just-in-time compiler (Cranelift) for arithmetic, conversions, comparisons, branches,
  switches and moves between locals and the operand stack, compiling warm methods on a background
  thread; `-Xint`, `-Xbatch`, `-XX:CompileThreshold=<n>`, `-XX:+PrintCompilation`
* Floating-point `+`, `*` and `%` return the NaN HotSpot's interpreter returns (which of two NaN
  operands, with which sign), on x86-64
* A benchmark suite and runner (`bench/run.py`) comparing rusty-jvm with HotSpot's interpreter,
  C1 and C2; see [docs/benchmarking.md](docs/benchmarking.md)

# 0.6.0

## What's Changed
* Java Native Interface: loading shared libraries and a broad set of JNI functions (strings, arrays, fields, method calls, exceptions, class lookup)
* Multithreading: proper main thread, monitors, `wait`/`notify`/`notifyAll`, thread interruption, `Thread.State`, safe-points
* `-cp` and `-jar` options, `-ea`/`-da`/`-esa`/`-dsa` assertion options
* Helpful NullPointerExceptions (JEP 358)
* Support for JDK 26 and 27 core library classes (jimage format 1.1)
* Extended `java.io`/`java.nio` support: `RandomAccessFile`, `File` directory listing, memory-mapped files, `FileChannel`
* Reflection improvements: `Class.getPermittedSubclasses0`, HotSpot-compatible `Reflection.getCallerClass`, boxing of primitive results
* Monotonic `System.nanoTime` and wall clock before 1970
* Locales, `ClassLoader`, shutdown hooks
* Switch to the `jclassmodel` crate
* Fixes for big-endian platforms and Windows


# 0.5.0

## What's Changed
* Switch to java 25 (LTS) core library classes
* Improve exception handling
* Fixes for big-endian platforms


# 0.4.0

## What's Changed
* Native methods `System.nanoTime()`, `Class.getDeclaredMethod(...)`, `Class.getDeclaredConstructor(...)`, `Class.getMethods()`, `Class.getFields()`
* Support for `invokedynamic` instruction and `java.lang.invoke` package (including support for lambdas, stream API, records)
* Basic support of `java.util.zip` package


# 0.3.0

## What's Changed
* Integrated the `jimage-rs` crate for reading JDK image files
* Replaced bundled classes with core library classes from the installed JDK


# 0.2.1

## What's Changed
* Rework installation
* Polishing readme


# 0.2.0

## What's Changed
* Added support for opcodes: `nop`, `dup2_x1`, `dup2_x2`, `swap`, `frem`, `fneg`, `dneg`, `d2f`, `goto_w`
* Added option for installing and removing standard library classes
* Refactored Library API


# 0.1.0

## What's Added
Added support for
* nearly all JVM bytecode instructions
* static initialization
* polymorphism
* exceptions
* program arguments
* core standard library classes
