#!/usr/bin/env python3
"""Runs the benchmark programs on rusty-jvm and on HotSpot and compares them.

Each program in bench/java prints one `iter <index> <nanoseconds>` line per run of its workload and
a final `checksum <value>` line (see Bench.java). For every benchmark and VM this script records:

  wall   whole-process wall time, startup included (median over repetitions)
  first  the first iteration: cold code, interpreter or first-tier JIT (median)
  best   the fastest iteration over all repetitions: warmed-up, peak performance
  rss    peak resident set size of the process (largest over repetitions)

and checks that every VM printed the same checksum as HotSpot. Results are printed as a markdown
table, followed by geometric means of rusty-jvm's slowdown against each HotSpot mode, and saved as
JSON so a later run can be compared against them with --baseline.

Only the Python standard library is used. Linux and macOS only (it uses os.wait4).
"""

import argparse
import datetime
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = ROOT / "bench" / "java"
CLASSES = ROOT / "target" / "bench" / "classes"
RESULTS = ROOT / "target" / "bench" / "results"

# name: {preset: (iterations, size)}. Sizes are chosen so the quick preset finishes in seconds on
# HotSpot and in about a minute per benchmark on an interpreter; full runs long enough for C2.
BENCHMARKS = {
    "Hello": {"quick": (1, 0), "full": (1, 0)},
    "Fib": {"quick": (5, 27), "full": (10, 32)},
    "NBody": {"quick": (5, 50_000), "full": (10, 1_000_000)},
    "SpectralNorm": {"quick": (5, 150), "full": (10, 1_000)},
    "Fannkuch": {"quick": (5, 8), "full": (10, 10)},
    "BinaryTrees": {"quick": (5, 12), "full": (10, 16)},
    "Sieve": {"quick": (5, 2_000_000), "full": (10, 20_000_000)},
    "Strings": {"quick": (5, 50_000), "full": (10, 500_000)},
    "Collections": {"quick": (5, 50_000), "full": (10, 500_000)},
    "Streams": {"quick": (5, 200_000), "full": (10, 2_000_000)},
    "Dispatch": {"quick": (5, 1_000_000), "full": (10, 20_000_000)},
    "Monitors": {"quick": (5, 200_000), "full": (10, 2_000_000)},
}

# The HotSpot modes rusty-jvm is measured against, by the tier each corresponds to in the plan.
HOTSPOT_MODES = {
    "int": ["-Xint"],  # template interpreter only
    "c1": ["-XX:TieredStopAtLevel=1"],  # interpreter + C1, no profiling
    "c2": [],  # default tiered: interpreter, C1, C2
}
REFERENCE_ORDER = ["c2", "c1", "int"]
# Benchmarks that only measure startup: left out of the first- and best-iteration means.
STARTUP_ONLY = {"Hello"}


def java_binary(name):
    home = os.environ.get("JAVA_HOME")
    if home:
        candidate = Path(home) / "bin" / name
        if candidate.exists():
            return str(candidate)
    found = shutil.which(name)
    if not found:
        sys.exit(f"error: {name} not found; set JAVA_HOME or put the JDK on PATH")
    return found


def compile_sources():
    sources = sorted(SOURCES.glob("*.java"))
    CLASSES.mkdir(parents=True, exist_ok=True)
    stamp = CLASSES / ".stamp"
    newest = max(s.stat().st_mtime for s in sources)
    if stamp.exists() and stamp.stat().st_mtime >= newest:
        return
    # --release keeps the class files at the version rusty-jvm targets, whatever javac is used.
    subprocess.run(
        [java_binary("javac"), "--release", "25", "-d", str(CLASSES), *map(str, sources)],
        check=True,
    )
    stamp.touch()


def ensure_rusty(path, build):
    if build or not Path(path).exists():
        if Path(path) != ROOT / "target" / "release" / "rusty-jvm":
            sys.exit(f"error: {path} does not exist")
        env = dict(os.environ)
        env.setdefault("JAVA_HOME", str(Path(java_binary("java")).resolve().parent.parent))
        subprocess.run(["cargo", "build", "--release"], cwd=ROOT, env=env, check=True)


def run_once(command, timeout):
    """Runs `command`; returns (exit status or None on timeout, wall seconds, rss bytes, stdout)."""
    with tempfile.TemporaryFile() as out:
        start = time.perf_counter()
        proc = subprocess.Popen(command, stdout=out, stderr=subprocess.STDOUT, cwd=CLASSES)
        timed_out = threading.Event()

        def kill():
            timed_out.set()
            proc.kill()

        timer = threading.Timer(timeout, kill)
        timer.start()
        try:
            _, status, usage = os.wait4(proc.pid, 0)
        finally:
            timer.cancel()
        wall = time.perf_counter() - start
        proc.returncode = os.waitstatus_to_exitcode(status)
        # ru_maxrss is in kilobytes on Linux and in bytes on macOS.
        rss = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
        out.seek(0)
        text = out.read().decode("utf-8", "replace")
    return (None if timed_out.is_set() else proc.returncode), wall, rss, text


ITER = re.compile(r"^iter (\d+) (\d+)$", re.M)
CHECKSUM = re.compile(r"^checksum (-?\d+)$", re.M)
UNSTABLE = re.compile(r"^unstable ", re.M)


def measure(command, reps, timeout):
    walls, firsts, iters, rss, checksums, errors = [], [], [], 0, set(), []
    for _ in range(reps):
        status, wall, peak, text = run_once(command, timeout)
        if status is None:
            errors.append(f"timeout after {timeout}s")
            break
        found = CHECKSUM.search(text)
        if status != 0 or not found:
            tail = text.strip().splitlines()[-3:]
            errors.append(f"exit {status}: " + " | ".join(tail))
            break
        if UNSTABLE.search(text):
            errors.append("checksum changed between iterations")
        times = [int(ns) for _, ns in ITER.findall(text)]
        walls.append(wall)
        firsts.append(times[0] / 1e9)
        iters.extend(t / 1e9 for t in times)
        rss = max(rss, peak)
        checksums.add(found.group(1))
    result = {"command": command, "errors": errors}
    if walls:
        result.update(
            wall=statistics.median(walls),
            first=statistics.median(firsts),
            best=min(iters),
            rss=rss,
            checksum=sorted(checksums)[0] if len(checksums) == 1 else None,
        )
        if len(checksums) > 1:
            errors.append(f"checksum differs between runs: {sorted(checksums)}")
    return result


def fmt_time(seconds):
    if seconds is None:
        return "-"
    if seconds >= 1:
        return f"{seconds:.2f}s"
    if seconds >= 1e-3:
        return f"{seconds * 1e3:.1f}ms"
    return f"{seconds * 1e6:.0f}us"


def geomean(values):
    values = [v for v in values if v and v > 0]
    return math.exp(sum(map(math.log, values)) / len(values)) if values else None


def report(results, vms, baseline):
    hotspot = [vm for vm in vms if vm != "rusty"]
    header = "| benchmark | vm | wall | first | best | rss | " + " | ".join(
        f"rusty/{vm} best" for vm in hotspot
    )
    print()
    print(header + " |")
    print("|" + "---|" * (header.count("|")))
    for name, by_vm in results.items():
        for vm in vms:
            r = by_vm.get(vm)
            if r is None:
                continue
            if r.get("errors") and "best" not in r:
                print(f"| {name} | {vm} | FAILED: {'; '.join(r['errors'])} |")
                continue
            ratios = []
            if vm == "rusty":
                for other in hotspot:
                    o = by_vm.get(other, {})
                    ratios.append(f"{r['best'] / o['best']:.1f}x" if o.get("best") else "-")
            else:
                ratios = [""] * len(hotspot)
            flag = (" **" + "; ".join(r["errors"]) + "**") if r.get("errors") else ""
            cells = [
                name,
                vm + flag,
                fmt_time(r["wall"]),
                fmt_time(r["first"]),
                fmt_time(r["best"]),
                f"{r['rss'] / 2**20:.0f}MB",
                *ratios,
            ]
            print("| " + " | ".join(cells) + " |")

    if "rusty" in vms and hotspot:
        print("\nrusty-jvm slowdown, geometric mean over benchmarks that ran on both:")
        for other in hotspot:
            for metric in ("wall", "first", "best"):
                pairs = [
                    by_vm["rusty"][metric] / by_vm[other][metric]
                    for name, by_vm in results.items()
                    if (metric == "wall" or name not in STARTUP_ONLY)
                    and by_vm.get("rusty", {}).get(metric) and by_vm.get(other, {}).get(metric)
                ]
                g = geomean(pairs)
                if g:
                    print(f"  vs {other:<3} {metric:<5} {g:8.1f}x  ({len(pairs)} benchmarks)")

    if baseline and "rusty" in vms:
        print(f"\nrusty-jvm against baseline {baseline['timestamp']} (negative is faster):")
        for name, by_vm in results.items():
            now = by_vm.get("rusty", {})
            then = baseline["results"].get(name, {}).get("rusty", {})
            cells = []
            for metric in ("wall", "best"):
                if now.get(metric) and then.get(metric):
                    cells.append(f"{metric} {100 * (now[metric] / then[metric] - 1):+6.1f}%")
            if cells:
                print(f"  {name:<14} " + "  ".join(cells))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--vms", default="rusty,int,c1,c2",
                        help="comma-separated VMs: rusty, int, c1, c2 (default: all)")
    parser.add_argument("--filter", default="", help="regex selecting benchmarks by name")
    parser.add_argument("--preset", choices=["quick", "full"], default="quick")
    parser.add_argument("--reps", type=int, default=3, help="process runs per VM (default 3)")
    parser.add_argument("--timeout", type=float, default=600, help="seconds per process run")
    parser.add_argument("--rusty", default=str(ROOT / "target" / "release" / "rusty-jvm"),
                        help="rusty-jvm binary (default: target/release/rusty-jvm)")
    parser.add_argument("--build", action="store_true", help="run cargo build --release first")
    parser.add_argument("--json", help="where to save results (default: target/bench/results/)")
    parser.add_argument("--baseline", help="earlier results JSON to compare rusty-jvm against")
    parser.add_argument("--list", action="store_true", help="list benchmarks and exit")
    args = parser.parse_args()

    if args.list:
        for name, presets in BENCHMARKS.items():
            print(f"{name:<14} quick={presets['quick']} full={presets['full']}  (iterations, size)")
        return

    vms = [vm.strip() for vm in args.vms.split(",") if vm.strip()]
    unknown = [vm for vm in vms if vm != "rusty" and vm not in HOTSPOT_MODES]
    if unknown:
        sys.exit(f"error: unknown VM(s) {unknown}")
    selected = [n for n in BENCHMARKS if re.search(args.filter, n)]
    if not selected:
        sys.exit(f"error: no benchmark matches {args.filter!r}")
    baseline = json.loads(Path(args.baseline).read_text()) if args.baseline else None

    # rusty-jvm reads the JDK's class library from JAVA_HOME, at build time and at run time.
    os.environ.setdefault("JAVA_HOME", str(Path(java_binary("java")).resolve().parent.parent))
    compile_sources()
    if "rusty" in vms:
        ensure_rusty(args.rusty, args.build)
    java = java_binary("java")

    results = {}
    for name in selected:
        iterations, size = BENCHMARKS[name][args.preset]
        program = [name, str(iterations), str(size)]
        results[name] = {}
        for vm in vms:
            if vm == "rusty":
                command = [args.rusty, "-cp", str(CLASSES), *program]
            else:
                command = [java, *HOTSPOT_MODES[vm], "-cp", str(CLASSES), *program]
            print(f"{name:<14} {vm:<5} ...", end=" ", flush=True)
            r = measure(command, args.reps, args.timeout)
            results[name][vm] = r
            print(fmt_time(r.get("best")) if "best" in r else "FAILED", flush=True)

        reference = next(
            (results[name][vm]["checksum"] for vm in REFERENCE_ORDER
             if results[name].get(vm, {}).get("checksum")),
            None,
        )
        for vm, r in results[name].items():
            if reference and r.get("checksum") and r["checksum"] != reference:
                r["errors"].append(f"WRONG checksum {r['checksum']} (HotSpot {reference})")

    report(results, vms, baseline)

    out = Path(args.json) if args.json else RESULTS / (
        datetime.datetime.now().strftime("%Y%m%d-%H%M%S") + f"-{args.preset}.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    git = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                         capture_output=True, text=True).stdout.strip()
    version = subprocess.run([java, "-version"], capture_output=True, text=True).stderr
    out.write_text(json.dumps({
        "timestamp": datetime.datetime.now().isoformat(timespec="seconds"),
        "commit": git,
        "preset": args.preset,
        "reps": args.reps,
        "java": version.splitlines()[0] if version else "",
        "results": results,
    }, indent=2))
    print(f"\nsaved {out.relative_to(ROOT) if out.is_relative_to(ROOT) else out}")
    failed = any(r.get("errors") for by_vm in results.values() for r in by_vm.values())
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
