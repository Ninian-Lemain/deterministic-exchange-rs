# Performance Methodology

## Allocation Gate

`hft-bench` initializes and warms the gateway, snapshots allocation and
deallocation counters, processes 200,000 messages, then asserts both deltas are
zero. Input frames and report storage are stack/preallocated values.

## Timing Smoke Harness

The executable records aggregate throughput timing and p50, p90, p99, p99.9,
and maximum for 2,000 sampled messages. Per-sample `Instant` overhead is
included. This is useful for catching gross regressions, not establishing a
service-level objective.

The initial local smoke run used Windows 11 Pro build 26200, an Intel N95 (four
cores/four threads), about 16 GB RAM, Rust 1.96.0/LLVM 22.1.2, x86_64 MSVC,
release fat LTO, one codegen unit, aborting panics, 100,000 order pairs, and
2,000 per-message samples after one warm-up pair. The latest run reported
200,000 messages, 64 ns aggregate mean, p50/p90/p99/p99.9/max of
100/100/200/300/800 ns, queue maximum occupancy 64, one explicit
backpressure event, and zero allocation and deallocation deltas. `Instant`
resolution, sampling overhead, and an unisolated desktop scheduler dominate
these figures; Windows results are intentionally not production latency claims.

## Dedicated Linux Protocol

Record all of the following with each result:

- CPU model/microcode, memory speed, NIC/firmware, topology, and NUMA placement.
- Kernel, mitigations, IRQ routing, isolated CPUs, affinity, and CPU governor.
- Rust/compiler revision, target CPU flags, linker, LTO, and codegen units.
- Backend, ring sizes, batch size, prefault/mlock/hugepage settings, warm-up,
  sample count, and offered load.
- Throughput, p50/p90/p99/p99.9/max, queue occupancy/backpressure, branch and
  cache misses, context switches, page faults, and allocation deltas.

Use `perf stat`/`perf record` on pinned Linux cores. CI hosted-runner latency is
informational and must never block a release.
