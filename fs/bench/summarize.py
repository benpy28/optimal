#!/usr/bin/env python3
"""Summarises run.sh results: one table per mode, OptimalFS against ext4.

    python3 summarize.py results/*.jsonl > summary.txt

Each input file is one VM (one job). For every metric it prints, per file
system, the median over all runs on all VMs with the lowest and highest run
in brackets, and the ratio OptimalFS / ext4: the median of the per-run
ratios (each run pairs the two file systems on the same VM, in the same
pass), with the lowest and highest ratio. Below 1 means OptimalFS took less
(time, latency, bytes, flushes) except for rates (marked "higher is
better"). It also checks that the kernel's disk counters agree with what
OptimalFS says it wrote.
"""

import json
import statistics
import sys
from collections import defaultdict

# (workload, metric, label, unit divisor, higher_is_better)
KEY = [
    ("page-rewrite", "fsync_p50_us", "4 KiB rewrite + fsync, P50 (us)", 1, False),
    ("page-rewrite", "fsync_p99_us", "4 KiB rewrite + fsync, P99 (us)", 1, False),
    ("page-rewrite", "fsync_p999_us", "4 KiB rewrite + fsync, P99.9 (us)", 1, False),
    ("page-rewrite", "work_dev_write_mib", "device MiB written (2,000 fsyncs)", 1, False),
    ("page-rewrite", "work_dev_flushes", "device flushes", 1, False),
    ("fsync-bg", "fsync_p50_us", "fsync under 2 GiB stream, P50 (us)", 1, False),
    ("fsync-bg", "fsync_p99_us", "fsync under 2 GiB stream, P99 (us)", 1, False),
    ("fsync-bg", "fsync_p999_us", "fsync under 2 GiB stream, P99.9 (us)", 1, False),
    ("fsync-bg", "total_s", "whole loop incl. stream writes (s)", 1, False),
    ("fsync-bg", "work_dev_write_mib", "device MiB written", 1, False),
    ("fsync-burst", "fsync_p50_us", "burst fsync, P50 (us)", 1, False),
    ("fsync-burst", "fsync_p99_us", "burst fsync, P99 (us)", 1, False),
    ("fsync-burst", "fsync_p999_us", "burst fsync, P99.9 (us)", 1, False),
    ("fsync-burst", "burst_p50_us", "1,000-file burst, median duration (ms)", 1000, False),
    ("fsync-burst", "burst_max_us", "1,000-file burst, slowest of 5 (ms)", 1000, False),
    ("fsync-burst", "work_dev_write_mib", "device MiB written (5,000 files)", 1, False),
    ("fsync-burst", "work_dev_flushes", "device flushes", 1, False),
    ("dpkg", "iter_p50_us", "dpkg iteration, P50 (us)", 1, False),
    ("dpkg", "iter_p99_us", "dpkg iteration, P99 (us)", 1, False),
    ("dpkg", "iter_p999_us", "dpkg iteration, P99.9 (us)", 1, False),
    ("dpkg", "total_s", "10,000 iterations (s)", 1, False),
    ("dpkg", "work_dev_write_mib", "device MiB written", 1, False),
    ("dpkg", "work_dev_flushes", "device flushes", 1, False),
    ("syncfs-1g", "write_s", "1 GiB of 1 MiB writes (s)", 1, False),
    ("syncfs-1g", "syncfs_s", "then syncfs (s)", 1, False),
    ("syncfs-1g", "total_mib_per_s", "write + syncfs (MiB/s, higher is better)", 1, True),
    ("syncfs-1g", "work_dev_write_mib", "device MiB written", 1, False),
    ("rand-4k", "read_fresh_p50_us", "random 4 KiB read, cold, P50 (us)", 1, False),
    ("rand-4k", "read_fresh_p99_us", "random 4 KiB read, cold, P99 (us)", 1, False),
    ("rand-4k", "overwrite_iops", "20,000 random overwrites + fsync (IOPS, higher is better)", 1, True),
    ("rand-4k", "overwrite_dev_write_mib", "device MiB written by the overwrites", 1, False),
    ("rand-4k", "read_after_p50_us", "random read after overwrites, P50 (us)", 1, False),
    ("rand-4k", "read_after_p99_us", "random read after overwrites, P99 (us)", 1, False),
    ("meta", "create_s", "create 100,000 files + syncfs (s)", 1, False),
    ("meta", "stat_s", "stat 100,000, cold (s)", 1, False),
    ("meta", "unlink_s", "unlink 100,000 + syncfs (s)", 1, False),
    ("meta", "work_dev_write_mib", "device MiB written", 1, False),
]

OFS_ONLY = [
    ("ofs_fsync_commits", "fsyncs that ran a full commit"),
    ("ofs_periodic_commits", "periodic (5 s) commits"),
    ("ofs_periodic_commit_s", "time in periodic commits (s)"),
    ("ofs_log_blocks", "intent-log blocks written"),
]


def fmt(x):
    if x == 0:
        return "0"
    a = abs(x)
    if a >= 100:
        return f"{x:.0f}"
    if a >= 10:
        return f"{x:.1f}"
    if a >= 1:
        return f"{x:.2f}"
    return f"{x:.3g}"


def rng(v, d=1):
    v = [x / d for x in v]
    return f"{fmt(statistics.median(v))} [{fmt(min(v))}-{fmt(max(v))}]"


def main(paths):
    # (mode, workload, target) -> list of (vm, run, metrics)
    res = defaultdict(list)
    for vm, p in enumerate(paths):
        for line in open(p):
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            res[(d["mode"], d["workload"], d["target"])].append((vm, d["run"], d["metrics"]))
    modes = sorted({k[0] for k in res})
    scales = sorted({json.loads(l)["scale"] for p in paths for l in open(p) if l.strip()})
    if scales != [1]:
        print(f"NOTE: scale {scales}: counts in the labels are for scale 1")
    worst_xcheck = 0.0
    for mode in modes:
        print(f"=== {mode} ===")
        print(f"{'metric':58} {'ext4 median [min-max]':>26} {'OptimalFS median [min-max]':>28} {'OFS/ext4 median [min-max]':>28} runs")
        last = None
        for w, m, label, div, _hib in KEY:
            e = {(vm, r): x[m] for vm, r, x in res.get((mode, w, "ext4"), []) if m in x}
            o = {(vm, r): x[m] for vm, r, x in res.get((mode, w, "ofs"), []) if m in x}
            if not e or not o:
                continue
            if w != last:
                print(f"-- {w}")
                last = w
            pairs = sorted(set(e) & set(o))
            ratios = [o[k] / e[k] for k in pairs if e[k] > 0]
            rs = rng(ratios) if ratios else "n/a"
            print(f"  {label:56} {rng(list(e.values()), div):>26} {rng(list(o.values()), div):>28} {rs:>28} {len(pairs)}")
        print("-- OptimalFS only")
        for w in dict.fromkeys(k[0] for k in KEY):
            rows = res.get((mode, w, "ofs"), [])
            if not rows:
                continue
            parts = []
            for m, label in OFS_ONLY:
                v = [x[m] for _, _, x in rows if m in x]
                if v:
                    parts.append(f"{label} {rng(v)}")
            print(f"  {w}: " + "; ".join(parts))
            for _, _, x in rows:
                k = x.get("work_dev_write_mib", 0)
                b = x.get("ofs_blocks_written", 0) * 4096 / 2**20
                if k > 0:
                    worst_xcheck = max(worst_xcheck, abs(k - b) / k)
        print()
    print(
        f"Cross-check: largest gap between the kernel's disk counter and the blocks "
        f"OptimalFS wrote, over every OptimalFS run: {worst_xcheck * 100:.2f}% "
        f"(the counter also sees any other writer on the disk)"
    )


if __name__ == "__main__":
    main(sys.argv[1:])
