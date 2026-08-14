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

The local smoke environment used Windows 11 Pro build 26200, an Intel N95 (four
cores/four threads), about 16 GB RAM, Rust 1.96.0/LLVM 22.1.2, x86_64 MSVC,
release fat LTO, one codegen unit, aborting panics, 100,000 order pairs, and
2,000 per-message samples after one warm-up pair. Five consecutive runs on
2026-08-14 each processed 200,000 messages. Aggregate means were
128/106/76/68/68 ns per message (76 ns median); p50 and p90 were 100 ns, median
p99/p99.9 were 200/300 ns, and maxima ranged from 300 ns to 73.4 us. Every run
reported queue maximum occupancy 64, one explicit backpressure event, a stable
digest, and zero allocation and deallocation deltas. `Instant` resolution,
sampling overhead, and an unisolated desktop scheduler dominate these figures;
Windows results are intentionally not production latency claims.

## v0.2.0 Indexed Cancellation

The cancellation harness compares v0.1.0's linear whole-book search with
v0.2.0's fixed-capacity open-addressed index. Each run constructs 1,024 fresh
books. Each book contains 512 sell price levels with one order per level, then
cancels all orders from highest to lowest price. Setup is outside the timed
region, leaving 524,288 measured cancellations per run. The first run is
discarded as warm-up and the table reports the median of the next seven.

| Metric | v0.1.0 linear scan | v0.2.0 indexed | Difference |
| --- | ---: | ---: | ---: |
| Median timed duration | 696.869 ms | 13.871 ms | 98.0% lower |
| Median latency | 1,329 ns/cancel | 26 ns/cancel | 98.0% lower |
| Median throughput | 752,347 cancels/s | 37,796,329 cancels/s | 50.2x |
| Allocation / deallocation delta | 0 / 0 | 0 / 0 | Unchanged |
| `OrderBook<512, 1>` size | 65,544 bytes | 131,080 bytes | +65,536 bytes |

Command: `cargo build --release -p hft-bench`, followed by eight direct runs of
`target/release/hft-bench.exe` for each implementation. The baseline was commit
`1b3cf4b`; the indexed result used the v0.2.0 release candidate. Environment:
Intel N95, Windows 11 Pro build 26200, Rust 1.96.0/LLVM 22.1.2,
`x86_64-pc-windows-msvc`, fat LTO, one codegen unit, and aborting panics.

The result isolates in-memory lookup and cancellation. It excludes book setup,
wire parsing, risk, the gateway, kernel, NIC, and network transit. The index
removes the whole-book scan at the cost of 64 KiB for this book shape. A cancel
can still shift later orders within its price level; v0.3.0 will address that
separately. Desktop scheduling and frequency changes remain sources of noise.

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
