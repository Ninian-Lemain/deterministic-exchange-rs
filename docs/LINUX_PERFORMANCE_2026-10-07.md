# Linux development performance, 2026-10-07

Ten pinned Linux release runs measured a median mean of **65 ns/message** for
batched frame parsing, risk and matching, and **172 ns/command** for session,
routing, journal enqueue and event publication. All 140 cells retained their
checksums and parameters across the runs. All 137 non-recovery cells recorded
zero allocations and deallocations in their checked windows.

These are WSL 2 development measurements on a shared desktop. They are useful
evidence for the in-memory engine, but do not complete dedicated Linux latency
qualification or measure network and durable-storage latency.

## Environment and method

- Source: `0d71e91befc17ea00ed3c9c822e9b3ed45b8b6a8`, clean Linux checkout.
- AMD Ryzen 7 7735HS, eight cores and sixteen logical processors.
- Ubuntu 26.04.1, WSL 2, Linux 6.6.87.2 Microsoft kernel.
- Rust 1.96.0, LLVM 22.1.2, `x86_64-unknown-linux-gnu`.
- Portable release build, fat LTO, one codegen unit, aborting panics; no PGO.
- Binary SHA-256: `72637169bf39312778361d33db301245ec6ef4adf557e0dc3570e6a002288e0c`.
- One excluded full-suite warmup followed by ten complete runs, all pinned to
  logical CPU 2. No measured runs or outliers were discarded.
- Project builds, safety tests and both sustained soak processes finished before
  sampling. Windows background activity, CPU frequency and host scheduling were
  not controlled. Cells ran in the harness's fixed order.

Tables report medians of ten per-run statistics, not percentiles pooled across
all samples. Means use the recorded integer nanoseconds; throughput is the median
of the harness's reported rates. Most cells have 2,000 samples per run; the mixed
gateway has 19,000 sampled commands after warmup. Per-run p99.9 is therefore a
small tail sample, not a worst-case bound.

## Gateway and service admission

All values below are nanoseconds. Rates are millions of timed operations per
second, derived by the benchmark; they are not sustained exchange throughput.

| Path | Median mean | Median p50 | Median p99 | Median p99.9 | Run mean range | Rate |
| --- | ---: | ---: | ---: | ---: | --- | ---: |
| Batched frame rest/fill | 65 | 56 | 115.5 | 405.5 | 55–78 | 15.26 |
| Batched normalized-command rest/fill | 64.5 | 54 | 115.5 | 624 | 52–94 | 15.44 |
| Individually timed frame rest/fill | 74 | 80 | 96 | 135 | 68–124 | 13.54 |
| Seeded mixed gateway commands | 99 | 90 | 310.5 | 461 | 89–133 | 10.10 |
| Single-instrument journaled engine | 154 | 155 | 196 | 285.5 | 129–174 | 6.49 |
| Routed journaled command | 146.5 | 145 | 220.5 | 401 | 129–173 | 6.83 |
| Routed journaled frame | 143 | 150 | 176 | 401 | 130–295 | 6.99 |
| Session plus routed journaled command | 145 | 145 | 196 | 316 | 126–205 | 6.91 |
| Session plus routed journaled frame | 172 | 160.5 | 291 | 421 | 145–187 | 5.82 |

Batched cells time 64 commands together and report amortized nanoseconds per
command over 2,000 batches. Their percentiles describe **batch-average costs**;
they are not individual-message tail latency. The four routed/session cells
use two independent instruments and time admission through parsing where
applicable, sequence checks, risk, matching, journal encoding/enqueue and event
publication. Event consumption and persistence draining run outside the timer.
The journal sink is fixed in-memory storage, not a disk.

All four routed/session paths produced checksum `eea387195e64e754` and equivalent
final snapshots. Their overlapping ranges and fixed run order do not establish
that a parsed frame is faster than a normalized command, or quantify small
session and facade overheads reliably.

The worst individually timed frame rest/fill sample was **83.438 us**, the worst
mixed gateway sample **58.091 us**, and the worst session/routed frame sample
**29.205 us**. Short typical execution and occasional host interruptions coexist.
These maxima remain in the raw results; no tail guarantee is claimed.

## Components and recovery

| Workload | Median mean | Median p99 | Allocations per sample |
| --- | ---: | ---: | ---: |
| SPSC seeded push/pop walk | 26 ns | 35.5 ns | 0 |
| Valid session admission through gateway | 91 ns | 125.5 ns | 0 |
| Session duplicate refusal | 24.5 ns | 31 ns | 0 |
| Retransmission refill/refusal/replay cycle | 1.900 us | 2.129 us | 0 |
| Canonical snapshot encode | 1.549 us | 2.305 us | 12 |
| Verified snapshot restore | 5.146 us | 11.262 us | 4 |
| Snapshot plus eight-command tail | 8.689 us | 23.149 us | 8 |

The retransmission sample checks an advancing prefix, 32 real refills and replay
of all 64 payloads; it is a cycle cost, not time per frame. Recovery is cold-path
work with an 820-byte snapshot. Its allocations are intentional and separate
from hot admission. Neither recovery timings nor admission timings include file
open, write, flush, directory synchronization or restart process setup.

## Assessment

The measured core is fast on this machine: the simple gateway averages below
0.1 us per command and the journaled session/frame boundary averages about
0.17 us. Avoiding allocation on every declared hot path and retaining identical
workload checksums are stronger results than a single best timing. The two
[completed sustained fault captures](ROADMAP_PROGRESS_2026-10-06.md#sustained-qualification)
also provide evidence for deterministic recovery and bounded resident memory.

Production readiness remains unproven. Dedicated host tail measurements, network
ingress/egress, actual persistence, deployment behavior and independent review
remain necessary. These runs cannot rank the project against commercial HFT
systems or establish a Windows-to-Linux speedup. The September Windows runs used
different host conditions and are retained as separate historical evidence.

## Profiling and raw evidence

Separate `perf stat` and `perf record` runs retained all 140 workload checksums.
The counter run recorded 1,383,091,456 user cycles, 3,920,989,537 instructions,
552,121,161 branches and 444,815 branch misses. These cover the **entire suite,
including untimed fixture setup**. Dividing them by sampled command counts would
not produce valid cycles per command. The sampled profile includes book submit,
fixture work, journal decoding and timer calls; it is diagnostic rather than a
profile of a production service.

- [All 140 cell summaries](evidence/linux-benchmark-2026-10-07.csv)
- [Raw runs, environment, binary identity and profiles](evidence/linux-benchmark-2026-10-07.zip)

The archive includes the excluded warmup, ten measured JSON files, all run
summaries, counter output, profile data and file hashes. Reproduce the workload
with a portable release build and `taskset -c 2 target/release/hft-bench`, keeping
warmup separate. Use the dedicated qualification tooling for a native host;
its preflight correctly rejects WSL.
