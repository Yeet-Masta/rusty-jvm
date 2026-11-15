# Benchmarking against HotSpot

`bench/` holds a small benchmark suite and a runner that measures rusty-jvm against HotSpot in
three modes, one for each tier rusty-jvm is working towards:

| VM      | HotSpot flags              | What it is                         |
|---------|----------------------------|------------------------------------|
| `int`   | `-Xint`                    | template interpreter only          |
| `c1`    | `-XX:TieredStopAtLevel=1`  | interpreter + C1, no profiling     |
| `c2`    | (default)                  | full tiered compilation, C1 and C2 |
| `rusty` | `target/release/rusty-jvm` | this VM                            |

## Running

Requires Python 3 (standard library only), a JDK 25 on `JAVA_HOME` or `PATH`, and Linux or macOS.

```sh
python3 bench/run.py                       # quick preset, every benchmark, every VM, 3 runs each
python3 bench/run.py --preset full         # sizes long enough for C2 to reach peak
python3 bench/run.py --filter 'Fib|NBody'  # a subset, by regex on the name
python3 bench/run.py --vms rusty,c2        # only some VMs
python3 bench/run.py --build               # cargo build --release first
python3 bench/run.py --list                # benchmarks and their (iterations, size) per preset
```

The runner compiles `bench/java/*.java` into `target/bench/classes` (only when a source changed)
and builds `target/release/rusty-jvm` if it does not exist. It exits non-zero if any run failed,
timed out or printed a wrong checksum.

## What is measured

Each benchmark runs its workload several times in one process and prints the time of each run and
a checksum (see `bench/java/Bench.java`). For every benchmark and VM the runner reports:

- **wall**: whole-process time, VM startup included (median over runs). The number that matters
  for short programs.
- **first**: the first iteration, before a JIT has warmed up (median over runs).
- **best**: the fastest iteration over all runs. Peak, steady-state performance.
- **rss**: peak resident memory of the process.
- **rusty/X best**: how many times slower rusty-jvm's best iteration is than VM `X`'s.

It then prints the geometric mean of rusty-jvm's slowdown against each HotSpot mode, for wall,
first and best (startup-only benchmarks are left out of first and best), and saves everything,
with the commit and Java version, to `target/bench/results/<timestamp>-<preset>.json`.

Every VM's checksum must match HotSpot's, so a speed-up that breaks semantics shows up as a
failure rather than a win.

## Tracking progress

Compare a run with an earlier one to see rusty-jvm's change per benchmark:

```sh
python3 bench/run.py --vms rusty --baseline target/bench/results/20261115-120000-quick.json
```

For stable numbers, close other programs, keep the machine on AC power, and use more runs
(`--reps 5`). Differences under about 5% are within run-to-run noise.

## Recorded results

Quick preset, best iteration, on a Core i9-13900KF with Temurin 25. The quick preset is too
short for C2 to reach its peak; use `--preset full` to compare against it.

| Benchmark    | `rusty-jvm -Xint` | With the baseline JIT | vs HotSpot `-Xint` | vs C1  | vs C2  |
|--------------|------------------:|----------------------:|-------------------:|-------:|-------:|
| Fib          |            39.1ms |                22.0ms |               3.4x |  43.5x |  43.3x |
| NBody        |           240.8ms |               266.5ms |               6.3x | 161.2x | 160.6x |
| SpectralNorm |            98.0ms |                66.4ms |               3.7x |  59.0x |  97.5x |
| Fannkuch     |            70.0ms |                56.3ms |               5.1x |  41.8x |  42.3x |
| BinaryTrees  |           239.1ms |               202.8ms |               7.4x |  90.4x | 106.7x |
| Sieve        |           238.5ms |               130.1ms |               3.8x |  29.8x |  34.3x |
| Strings      |           557.6ms |               486.6ms |               3.1x |  64.2x | 111.3x |
| Collections  |           391.5ms |               336.3ms |               5.0x |  59.7x |  86.4x |
| Streams      |           135.0ms |               122.3ms |               4.3x |  65.2x | 298.0x |
| Dispatch     |           349.3ms |               227.5ms |               4.1x |  36.4x |  90.1x |
| Monitors     |            91.6ms |                88.6ms |               2.7x |   3.5x |   4.1x |

NBody is slower compiled than interpreted: its loop is field access, which compiled code still
hands back to the interpreter one instruction at a time.

Geometric mean of the slowdown: 4.3x against HotSpot's `-Xint` (6.7x before the JIT and the arithmetic
intrinsics), 44.9x against C1 (68.5x), 66.5x against C2 (96.5x). Hello world takes 64ms of wall
time, against HotSpot's 15ms. The run is saved as `target/bench/results/20261115-174733-quick.json`.

## The benchmarks

| Benchmark      | Stresses                                                        |
|----------------|-----------------------------------------------------------------|
| `Hello`        | startup: VM boot, class loading, one `println`                  |
| `Fib`          | calls and returns                                               |
| `NBody`        | double arithmetic, instance fields                              |
| `SpectralNorm` | loops over double arrays, a small static call in the inner loop |
| `Fannkuch`     | branchy int code, small int arrays, `System.arraycopy`          |
| `BinaryTrees`  | allocation of short-lived objects, collector throughput         |
| `Sieve`        | boolean array stores in counted loops                           |
| `Strings`      | `StringBuilder`, indy string concatenation, `equals`, hashing   |
| `Collections`  | `HashMap`/`ArrayList`, boxing, interface calls, sorting         |
| `Streams`      | lambdas, streams, `invokedynamic`                               |
| `Dispatch`     | mono-, bi- and megamorphic interface call sites                 |
| `Monitors`     | uncontended and 4-thread contended `synchronized`               |

To add one, write a class in `bench/java` that implements `Bench.Workload` and calls
`Bench.main(args, new YourClass())`, then add it with its sizes to `BENCHMARKS` in `bench/run.py`.
The workload must return the same checksum on every iteration and every VM.

## Profiling

To see where rusty-jvm spends its time on one benchmark:

```sh
perf record -g target/release/rusty-jvm -cp target/bench/classes Fib 5 30
perf report
```
