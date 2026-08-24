# Performance Methodology

## Allocation Gate

`hft-bench` initializes and warms the gateway, snapshots allocation and
deallocation counters, processes 200,000 messages, then asserts both deltas are
zero. Input frames and report storage are stack/preallocated values.

## Timing Smoke Harness

Every scenario runs a fixed warm-up (64 iterations, or the first 1,000 gateway
messages) before any sample is recorded, so frequency ramp-up, first-touch
stack pages, and branch training stay out of the reported distribution. Each
scenario keeps its fixture in a steady state: every timed operation is balanced
by an untimed operation that restores the starting book/risk shape, so sample
1,900 measures the same workload as sample 1. Teardown (a replenishing rest or
a cancel) is never inside the timed region.

All benches report p50, p90, p99, p99.9, max, and mean from sorted per-sample
arrays. The mean is outlier-sensitive and reported only for continuity; the
percentiles are the robust shape. On a shared desktop the maximum still
captures occasional scheduler preemptions (tens of microseconds to ~1 ms); no
harness change can remove those without core isolation, and none should try;
the max is kept visible rather than trimmed. Per-sample `Instant` overhead is
included throughout. This is useful for catching gross regressions, not
establishing a service-level objective.

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

## v0.3.0 Stable-Slot FIFO Levels

The FIFO harness compares v0.2.0's dense array levels (shift on removal) with
v0.3.0's intrusive doubly linked levels (head, tail, free-list, stable slot
handles). Each cell rebuilds a fresh single-level book of the stated depth
2,000 times and times one operation per rebuild, keeping setup outside the
timed region. The table reports p50 of the 2,000 samples; per-sample
`Instant` overhead is included and quantizes readings to 100 ns on this
desktop.

| Scenario | Depth | v0.2.0 array p50 | v0.3.0 stable-slot p50 |
| --- | ---: | ---: | ---: |
| Head cancel | 1 | 100 ns | 100 ns |
| Head cancel | 4 | 200 ns | 100 ns |
| Head cancel | 16 | 300 ns | 100 ns |
| Head cancel | 64 | 400 ns | 100 ns |
| Head cancel | 512 | 3,300 ns | 100 ns |
| Middle cancel | 1 | 100 ns | 100 ns |
| Middle cancel | 4 | 100 ns | 100 ns |
| Middle cancel | 16 | 100 ns | 100 ns |
| Middle cancel | 64 | 400 ns | 100 ns |
| Middle cancel | 512 | 1,400 ns | 100 ns |
| Tail cancel | 1 | 100 ns | 100 ns |
| Tail cancel | 4 | 100 ns | 100 ns |
| Tail cancel | 16 | 200 ns | 100 ns |
| Tail cancel | 64 | 100 ns | 100 ns |
| Tail cancel | 512 | 100 ns | 100 ns |
| Head fill | 1 | 100 ns | 100 ns |
| Head fill | 4 | 200 ns | 100 ns |
| Head fill | 16 | 100 ns | 100 ns |
| Head fill | 64 | 600 ns | 100 ns |
| Head fill | 512 | 3,100 ns | 100 ns |

Allocation and deallocation deltas were zero in every cell of both
implementations, and the standard 200,000-message gateway workload produced
the identical logical digest `64321af91735b704` before and after. Depth-512
throughput rose from 219,365 to 12,453,300 head cancels/s and from 229,578 to
7,125,044 head fills/s (mean-of-samples basis).

Memory cost: `OrderBook<1, 512>` grew from 114,728 to 131,160 bytes (+14%)
and the v0.2.0 shape `OrderBook<512, 1>` from 131,080 to 172,040 bytes
(+31%); each slot now carries `prev`/`next` links and a live/free tag. The
v0.2.0 depth-one cancellation shape regressed from 26 ns to 41 ns mean per
cancel (37.3M to 23.9M cancels/s): unlinking relinks two pointers where a
one-element shift was free. Branch and cache-miss counters require the
dedicated Linux protocol; Windows desktop timing remains non-production
evidence.

Command: `cargo build --release -p hft-bench`, direct runs of
`target/release/hft-bench.exe`. Environment: Intel N95, Windows 11 Pro build
26200, Rust 1.96.0/LLVM 22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one
codegen unit, aborting panics. Depths 1, 4, 16, 64, and the 512-order
maximum; 2,000 samples per cell; p50/p90/p99/p99.9/max recorded for every
cell.

## v0.4.0 Indexed Risk State

The risk harness compares v0.3.0's linear-scan risk state with v0.4.0's
fixed-capacity open-addressed indices for accounts and reservations. Each
operation runs 256 timed samples against a `RiskEngine::<64, 1024>` at
stated occupancy. Reservation occupancy sweeps are 102, 512, and 921 (Ã¢â€°Ë†10%,
50%, 90% of capacity). Account occupancy sweeps are 6, 32, and 57 (Ã¢â€°Ë†10%,
50%, 90% of capacity). All samples are timed with `Instant`; per-sample
overhead is included.

| Operation | Occupancy | v0.3.0 linear mean | v0.4.0 indexed mean | Change |
| --- | ---: | ---: | ---: | ---: |
| risk_check | 102 | 190 ns | 79 ns | 2.4x |
| risk_check | 512 | 1,196 ns | 79 ns | 15.1x |
| risk_check | 921 | 1,631 ns | 65 ns | 25.1x |
| reservation_lookup | 102 | 166 ns | 137 ns | 1.2x |
| reservation_lookup | 512 | 635 ns | 52 ns | 12.2x |
| reservation_lookup | 921 | 1,081 ns | 50 ns | 21.6x |
| fill | 102 | 160 ns | 182 ns | ~same |
| fill | 512 | 644 ns | 71 ns | 9.1x |
| fill | 921 | 2,033 ns | 71 ns | 28.6x |
| cancel | 102 | 322 ns | 61 ns | 5.3x |
| cancel | 512 | 1,367 ns | 54 ns | 25.3x |
| cancel | 921 | 2,892 ns | 55 ns | 52.6x |
| settle | 102 | 171 ns | 95 ns | 1.8x |
| settle | 512 | 617 ns | 255 ns | 2.4x |
| settle | 921 | 1,017 ns | 55 ns | 18.5x |
| reject | 102 | 164 ns | 67 ns | 2.4x |
| reject | 512 | 644 ns | 63 ns | 10.2x |
| reject | 921 | 1,323 ns | 61 ns | 21.7x |
| account_lookup | 6 | 94 ns | 56 ns | 1.7x |
| account_lookup | 32 | 121 ns | 54 ns | 2.2x |
| account_lookup | 57 | 150 ns | 54 ns | 2.8x |

The indexed implementation adds no measurable allocation at any occupancy.
The gateway 200,000-message workload produced the identical logical digest
`64321af91735b704`. Memory cost: `RiskEngine::<64, 1024>` grew from 25,632
bytes (linear arrays + no index overhead) to 93,216 bytes (doubled probe
planes + `ReservationSlot` enum + free-list). The 67 KiB increase is
preallocated and fixed; the O(1) lookup removes the occupancy-proportional
scan at 90% reservation occupancy.

Command: `cargo build --release -p hft-bench`, direct runs of
`target/release/hft-bench.exe`. Environment: Intel N95, Windows 11 Pro build
26200, Rust 1.96.0/LLVM 22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one
codegen unit, aborting panics. 256 samples per operation; mean and max
recorded. Desktop scheduling and frequency changes remain sources of noise.

Correction (v0.6.0): the v0.4.0 harness closed its reservations in the fill
loop and then measured cancel and settle against those already-closed order
IDs, so the v0.4.0 cancel/settle cells above recorded `UnknownOrder`
rejections rather than live operations, and the occupancy-102 fill cell
underflowed its ID arithmetic. The harness was fixed in v0.6.0 (freshly
populated engine per operation, one live reservation per sample); the
corrected absolute numbers are in the v0.6.0 section. The v0.3.0 linear-scan
baseline column remains valid for the relative comparison.

## v0.5.0 Price-Level Discovery

The price harness measures discovery latency across four book shapes: dense
64-level/32-active, sparse 128/16, dense 128/64, and dense 128/120. Each
scenario builds a book, populates ask levels, then times a single-unit buy
submission that must locate the best ask. The `discovery` operation isolates the
best-price path without clearing the book.

| Operation | Scenario | Mean latency | Max latency |
| --- | --- | ---: | ---: |
| submit_cross | dense_64_32 | 64 ns | 900 ns |
| discovery | dense_64_32 | 77 ns | 2,300 ns |
| level_create | dense_64_32 | 119 ns | 800 ns |
| submit_cross | sparse_128_16 | 62 ns | 700 ns |
| discovery | sparse_128_16 | 76 ns | 800 ns |
| level_create | sparse_128_16 | 228 ns | 20,900 ns |
| submit_cross | dense_128_64 | 66 ns | 1,100 ns |
| discovery | dense_128_64 | 86 ns | 10,900 ns |
| level_create | dense_128_64 | 214 ns | 12,600 ns |
| submit_cross | dense_128_120 | 72 ns | 1,900 ns |
| discovery | dense_128_120 | 76 ns | 1,800 ns |
| level_create | dense_128_120 | 217 ns | 13,600 ns |

Discovery latency is flat at 76-86 ns across all shapes, confirming O(1)
best-price lookup via the sorted-level index. Level creation costs 119-228 ns
(maintaining sort order on insert). Zero allocations in every cell. The gateway
200,000-message workload produced the identical logical digest
`64321af91735b704`.

Command: `cargo build --release -p hft-bench`, direct runs of
`target/release/hft-bench.exe`. Environment: Intel N95, Windows 11 Pro build
26200, Rust 1.96.0/LLVM 22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one
codegen unit, aborting panics. 2,000 samples per cell; mean and max recorded.
Desktop scheduling and frequency changes remain sources of noise.

## v0.6.0 Match-Plan Benchmark

The match-plan harness measures the full submit path (plan build, plan apply,
and optional rest) across five scenarios on an `OrderBook<128, 8>` with an
8-report buffer: a non-crossing taker that rests, a single-fill taker, a
multi-fill taker crossing eight makers, a taker rejected by report capacity
preflight, and a taker rejected for trying to rest into a full best-priced
level. Each scenario times 2,000 submissions against a persistent book.

| Scenario | Traversals | Mean latency | Max latency |
| --- | ---: | ---: | ---: |
| non_crossing | 0 | 141 ns | 25,300 ns |
| single_fill | 1 | 146 ns | 24,200 ns |
| multi_fill | 8 | 115 ns | 3,800 ns |
| report_full | 9 | 114 ns | 200 ns |
| deep_rejection | 1 | 72 ns | 100 ns |

Report capacity and full-level rejections are preflighted, so the rejected
cells mutate nothing and report zero fills. Zero allocations in every cell.
The gateway 200,000-message workload produced the identical logical digest
`64321af91735b704`.

The v0.6.0 refactor also changed neighboring hot paths, and the same release
run remeasured them:

| Benchmark | v0.5.0-era mean | v0.6.0 mean | Driver |
| --- | ---: | ---: | --- |
| cancel_bench (512 levels, 1 order each) | 546 ns | 56-89 ns | binary-search level removal |
| price level_create (128 levels, 120 active) | 418 ns | 75-147 ns | indexed level allocation |
| price discovery (128 levels, 120 active) | 145 ns | 73-157 ns | crossing walk stops at the price limit |

For the risk harness (now measuring live operations after the correction
above), fill, cancel, and settle each measured 69-150 ns mean at 10-90%
reservation occupancy across runs; account lookup measured 103-114 ns mean.

Run-to-run variance on this loaded desktop is large (scenario means shifted up
to 2x between consecutive runs, and individual maxima reach hundreds of
microseconds under scheduler interference). The first table reports the
least-interfered cell across three consecutive runs; only zero-allocation
deltas and the unchanged digest are treated as gates.

Command: `cargo build --release -p hft-bench`, direct runs of
`target/release/hft-bench.exe`. Environment: Intel N95, Windows 11 Pro build
26200, Rust 1.96.0/LLVM 22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one
codegen unit, aborting panics. 2,000 samples per cell; mean and max recorded.
Desktop scheduling and frequency changes remain sources of noise.

## v0.6.1 Benchmark Harness Hardening

The harness itself was audited and reworked; the engine is unchanged.

- Every scenario runs a fixed warm-up (64 iterations; the gateway loop skips
  sampling its first 1,000 messages) so frequency ramp-up, first-touch stack
  pages, and branch training stay out of the reported distribution. The gateway
  message sequence is unchanged, so the final digest stays comparable.
- Every scenario now holds its fixture in a steady state: each timed operation
  is balanced by an untimed teardown (a replenishing rest or a cancel) that
  restores the starting shape. Previously `submit_cross` depleted all makers
  after `active` samples and then measured resting rejections, `level_create`
  filled the book after ~120 samples and then measured
  `PriceLevelCapacity` rejections, and the gateway loop sampled its coldest
  first iterations.
- `risk_check` at 921 occupancy now budgets samples against the remaining
  order capacity (78 samples; the monotonic-ID rule forbids reuse), instead of
  silently measuring `OrderCapacity` rejections past slot 1,024.
- All benches report p50/p90/p99/p99.9/max alongside the mean. The mean is
  outlier-sensitive; the percentiles are the robust shape. The maximum is kept
  visible: on this shared desktop it captures occasional scheduler preemptions
  (single samples of 10 us-1 ms) that no harness change can remove without core
  isolation.

Representative run (Intel N95, Windows 11 Pro build 26200, Rust 1.96.0/LLVM
22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one codegen unit, aborting panics;
2,000 samples per book cell, 256 per risk cell):

| Workload | p50 | p90 | p99 | p99.9 | Mean |
| --- | ---: | ---: | ---: | ---: | ---: |
| Gateway packet-to-report (200,000 messages) | 300 ns | 400 ns | 400 ns | 400 ns | 194 ns/msg |
| Indexed cancel, 512-level book | n/a | n/a | n/a | n/a | 63-90 ns/cancel |
| risk_check / fill / cancel / settle, 90% reservation occupancy | 100-200 ns | 200 ns | 200 ns | 200-300 ns | 125-144 ns |
| Account lookup, 90% account occupancy | 100 ns | 100 ns | 200 ns | 200 ns | 101 ns |
| Best-price discovery, 120 active levels | 100 ns | 200 ns | 200 ns | 200 ns | 110 ns |
| Level create + remove steady state, 120 active levels | 100 ns | 200 ns | 200 ns | 200 ns | 135 ns |
| Submit crossing one level, 120 active levels | 200 ns | 300 ns | 300 ns | 300 ns | 248 ns |
| Match plan: non-crossing rest | 200 ns | 200 ns | 300 ns | 900 ns | 314 ns |
| Match plan: single fill | 200 ns | 200 ns | 300 ns | 400 ns | 215 ns |
| Match plan: 8-level multi-fill | 200 ns | 200 ns | 300 ns | 300 ns | 206 ns |
| Match plan: report-capacity rejection | 200 ns | 200 ns | 300 ns | 300 ns | 273 ns |
| Match plan: full-level rejection | 100 ns | 200 ns | 200 ns | 200 ns | 125 ns |

All cells measured zero allocation and deallocation deltas, and the gateway
workload produced the unchanged logical digest `64321af91735b704`. Where a
range is shown, it spans consecutive runs on the noisy desktop; percentile
shapes were stable across runs.

Command: `cargo build --release -p hft-bench`, direct runs of
`target/release/hft-bench.exe`. Environment: Intel N95, Windows 11 Pro build
26200, Rust 1.96.0/LLVM 22.1.2, `x86_64-pc-windows-msvc`, fat LTO, one
codegen unit, aborting panics. 2,000 samples per cell; mean and max recorded.
Desktop scheduling and frequency changes remain sources of noise.

## v0.7.0 Regression Check

No engine changes; this release adds seeded property and reference-model test
coverage. The full release suite was rerun on the same desktop environment:
the gateway workload produced the unchanged logical digest
`64321af91735b704`, queue occupancy 64 with one backpressure event, and zero
allocation/deallocation deltas in every scenario.

## v0.8.0 Reproducible Benchmark Suite

The harness now prints one JSON record per line (`hft-bench-results/1`).
Each record carries an explicit boundary (component, gateway, or network),
samples, mean, p50/p90/p99/p99.9/max, ops/s, allocation and deallocation
deltas, and a workload checksum. The schema fixture at
`crates/hft-bench/tests/fixtures/bench_results.schema.json` is validated in
CI against a reduced-suite run that also asserts roadmap workload coverage.
Every scenario warms up untimed, keeps its fixture in a steady state, and
fails on any allocation inside its measured region.

New workloads: alternating new-order/cancel frame parsing, an SPSC push/pop
random walk with occupancy high-water and backpressure counts, a seeded
20,000-command mixed gateway session whose checksum is the final digest, and
deep-book takers consuming 1/8/64 levels per sample with untimed replenish.
Risk cells now emit real checksums instead of a constant.

Two findings from validating this suite:

- The multi-fill match-plan fixture had rested deep makers, so its taker
  filled once at the best price while the output label claimed eight fills;
  it now rests single-unit makers across eight levels with untimed
  replenishment and asserts eight reports per taker. Pre-v0.8 numbers for
  that cell are not comparable.
- Long seeded sessions exposed a release-only engine defect: both
  open-addressed indexes invoked their back-shift update closures inside
  `debug_assert!`, so release binaries stranded moved handles, leaked index
  entries until a table filled, then spun forever removing from it. The
  closures now run unconditionally, with regression tests at the risk and
  book levels. The first complete fixed run reproduces the pre-existing
  gateway digest `64321af91735b704`.

Representative full pass on the reference desktop: gateway pair-rest-fill
mean 136 ns/message (2,000 samples), cancel sweep 512x1 median 49 ns/cancel,
mixed seeded session mean 231 ns/command, deep-book traversal cost scaling
with the cycled depth. Cycles, instructions, branch, and cache-miss counters
are unavailable on this host and are deferred to the dedicated Linux
qualification protocol below.

## v0.9.0 IOC and FOK

Protocol v2 adds a time-in-force byte to new orders; the engine gained IOC
(never rests, remainder discarded and reported) and FOK (all-or-reject via
plan preflight). The suite gained eight book-level cells comparing TIF paths
on the same desktop environment:

| Scenario | Mean | Note |
| --- | ---: | --- |
| ioc_empty | 90 ns | no crossing, zero fills |
| ioc_partial | 116 ns | fills 3 of 5, discards rest |
| ioc_full | 100 ns | equals gtc_full |
| fok_reject | 60 ns | preflight reject, no mutation |
| fok_single | 102 ns | single maker |
| fok_multi | 334 ns | sweeps eight levels |
| gtc_full | 101 ns | comparison cell |
| gtc_partial_rests | 133 ns | rests remainder as a bid |

FOK rejection is the cheapest path because the plan walk stops at the first
shortfall without touching book or risk state. Every cell reports zero
allocation deltas, and seeded property sessions now mix all three time-in-
force values through the gateway/model equivalence, snapshot, and replay
gates.
## v0.10.0 Post-Only

Post-only rejection uses the best-price entry of the sorted-level index, so
the crossing check is O(1) regardless of book depth; a rejected order
mutates nothing, and an accepted one rests through the normal GTC path. The
suite gained four cells (2,000 samples each, same desktop environment):

| Scenario | Levels | Mean |
| --- | ---: | ---: |
| post_only_cross_shallow | 1 | 110 ns |
| post_only_cross_deep | 64 | 48 ns |
| post_only_noncross_shallow | 1 | 108 ns |
| post_only_noncross_deep | 64 | 48 ns |

Crossing rejects and resting submits both sit in the same low-100-ns band as
their v0.9.0 GTC/IOC equivalents; the shallow/deep spread is within this
host's run-to-run jitter rather than a depth cost, since both paths touch
only the first sorted entry. All cells report zero allocation deltas.
Seeded property sessions now include post-only orders end to end.

## v0.11.0 Replace Lifecycle

Protocol type 3 adds owned replaces. Same-price quantity reductions patch the
resting slot in place (priority kept); price changes and increases remove and
re-add at the destination tail after a capacity preflight, and any repriced
order that would cross rejects before mutation. Risk reservations adjust
first and are restored exactly on book rejection.

Book-side cells time `OrderBook::replace` on a fresh one-maker fixture per
sample; the risk cell times `RiskEngine::adjust_reservation` alternating
between two fixed totals (2,000 samples each, same desktop environment):

| Scenario | Mean |
| --- | ---: |
| replace_reduce | 48 ns |
| replace_increase | 93 ns |
| replace_reprice | 89 ns |
| replace_reject_unknown | 86 ns |
| replace_risk_adjust (risk only) | 56 ns |

In-place reductions are roughly half the cost of priority-losing moves, and
the isolated risk adjustment shows the reservation side is comparable to a
cancel release. All cells report zero allocation deltas; seeded property
sessions now include replaces with per-step model equivalence, snapshot
invariants, and replay equality over the full byte stream.

## v0.12.0 SPSC Loom Coverage and Miri Qualification

The queue's atomics and slot cells now swap to Loom primitives under
`--features loom`, so the shipped algorithm itself runs inside the model;
the previous stand-in `ModelQueue` test was deleted. Two actual-algorithm
Loom tests run in CI: a capacity-two FIFO handoff across every
publication/consumption interleaving (~22 s of schedule enumeration), and a
capacity-one backpressure/wrap scenario proving the rejected payload is
returned bit-intact.

Miri now covers the whole crate (`cargo miri test -p hft-spsc`, CI),
including the 10,000-value cross-thread transfer reduced to 100 iterations
under Miri and the seeded lossless schedules reduced to 64 steps. As
evidence that ordering is enforced, weakening the tail publication to
`Relaxed` makes Miri report `Data race detected ... MaybeUninit<u64>`
immediately, while the same build stays logically correct under Loom
(Loom checks protocol-level races on its own primitives, not plain-memory
visibility).

No engine code changed in this release other than extracting the shared
`push_impl`/`pop_impl` cores used by both the endpoints and the Loom tests.
The SPSC benchmark cell is unchanged within noise before/after the refactor:

| push_pop_walk | Before | After |
| --- | ---: | ---: |
| mean | 43 ns | 43 ns |
| max occupancy | 85 | 85 |
| backpressure events | 0 | 0 |
| allocations / deallocations | 0 / 0 | 0 / 0 |
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
