# Performance Evidence

This document records what the benchmark suite measures and how to interpret
the results. Every timing currently published here is a Windows desktop smoke
result. No qualified Linux latency result exists yet.

## What Is Measured

`hft-bench` measures fixed in-memory operations and emits one JSON record per
line using schema `hft-bench-results/1`. The suite covers:

- packet parsing
- gateway packet-to-report processing
- cancellation at fixed book shapes
- FIFO removal and fill at fixed depths
- risk operations at fixed occupancy
- price discovery and level creation
- match-plan construction and application
- IOC, FOK, post-only, and replace paths
- SPSC push and pop traffic
- session admission and retransmission
- journal record creation, enqueue, verification, in-memory persistence, and recovery scanning

Each record names its measurement boundary as component, gateway, or network.
The current suite has component and gateway cells. It has no measured network
path.

## What Is Not Measured

The published numbers do not include a NIC, kernel network stack, wire transit,
production load balancer, durable journal write, filesystem flush, recovery
scan, replication, or standby promotion. They are not end-to-end exchange
latency and do not define a service objective.

Windows desktop results include timer quantization, scheduler preemption,
frequency changes, and background work. They are useful for finding large
regressions and invalid workloads. They are not dedicated hardware evidence.

## Methodology

The release profile uses fat LTO, one codegen unit, and aborting panics. Most
component cells record individual operations with `Instant`. Per-sample timer
cost is included.

Each workload warms its fixture before sampling. Destructive operations use an
untimed repair step so later samples measure the same state as earlier samples.
The gateway skips its first 1,000 messages before recording samples.

The reporter records sample count, mean, p50, p90, p99, p99.9, max, throughput,
allocation deltas, workload parameters, and a checksum. The maximum is not
trimmed. On the desktop it often records scheduler interference rather than
engine work.

The benchmark process uses a counting allocator. It snapshots allocation and
deallocation counters around each declared measured region and fails if either
counter changes. Fixtures, sample arrays, input frames, and report storage are
created before the allocation gate.

Workload checksums prevent dead-code removal and catch logical drift. Timing
alone is never accepted as proof that a workload exercised its named path.

## Reference Environments

### Windows desktop smoke environment

The recorded reference environment is:

- Intel N95 with four cores and four threads
- Windows 11 Pro build 26200
- about 16 GB RAM
- Rust 1.96.0 with LLVM 22.1.2
- `x86_64-pc-windows-msvc`
- fat LTO
- one codegen unit
- aborting panics

The desktop is shared and unisolated. `Instant` commonly quantizes short cells
to 100 ns. Hardware performance counters were not collected.

The v0.16 journal audit also ran on a Lenovo 83J3 with an AMD Ryzen 7 7735HS,
8 cores, 16 threads, 29.3 GB RAM, Windows 11 Home build 26200, Rust 1.96.0,
and LLVM 22.1.2. Hyper-V was active. These results are desktop smoke evidence.

### Qualified Linux environment

No environment has qualified yet. A result belongs in this section only when
the host manifest records CPU, topology, SMT, NUMA placement, microcode, kernel,
mitigations, governor, frequency behavior, isolated CPUs, process affinity, IRQ
affinity, memory, page size, huge pages, compiler, target flags, linker, LTO,
codegen units, workload, seed, sample count, capacity, and batch size.

Docker can reproduce build and profiling tools. It does not make the host CPU
isolated and does not turn a shared machine into dedicated hardware.

## Current Desktop Smoke Summary

This table keeps the strongest current reference results near the top. Values
come from the documented Windows runs. Timer resolution limits comparisons
between cells near 100 ns.

| Boundary | Workload | Desktop smoke result |
| --- | --- | ---: |
| Gateway | Pair rest and fill | 136 ns mean per message |
| Gateway | Seeded mixed session | 231 ns mean per command |
| Component | Indexed cancel, 512 levels | 49 ns median per cancel |
| Component | SPSC push and pop walk | 43 ns mean |
| Component | Post-only crossing check | 48 to 110 ns mean |
| Component | Replace lifecycle | 48 to 93 ns mean |
| Component | Session active admission | 144 ns mean |
| Component | Session duplicate rejection | 41 ns mean |
| Component | Journal enqueue on the current AMD host | 48 ns mean |

All listed cells reported zero allocation and deallocation deltas in their
measured regions. The gateway pair workload retained digest
`64321af91735b704`.

## Matching and Cancellation

The current cancel path uses a fixed open-addressed order index and stable slot
handles. The historical comparisons show why both changes remain in the design.

| Change | Shape | Before | After | Fixed memory cost |
| --- | --- | ---: | ---: | ---: |
| Indexed order lookup | 512 levels, one order each | 1,329 ns median | 26 ns median | +65,536 bytes |
| Stable-slot head cancel | one level, depth 512 | 3,300 ns p50 | 100 ns p50 | book shape +14% |
| Stable-slot middle cancel | one level, depth 512 | 1,400 ns p50 | 100 ns p50 | included above |
| Stable-slot head fill | one level, depth 512 | 3,100 ns p50 | 100 ns p50 | included above |

These are Windows desktop smoke comparisons from v0.2 and v0.3. The indexed
lookup run measured 524,288 cancellations after one discarded warmup run. The
stable-slot cells used 2,000 samples per depth. Setup was outside the timed
region. Stable slots made the depth-one cancel slightly slower and increased
book size because each slot stores links and live state.

The match-plan harness measures non-crossing rest, single fill, eight-level
fill, report-capacity rejection, and full-level rejection. A later suite audit
found that the original multi-fill fixture only crossed one maker. Results from
that old cell are invalid. The corrected fixture asserts eight reports and
restores makers outside the timed region.

## Risk

Fixed open-addressed indexes replaced occupancy-dependent account and
reservation scans. The valid historical comparison is the lookup scaling. The
original v0.4 fill, cancel, and settle cells are not retained as performance
evidence because they reused closed reservation IDs and measured rejection.

| Operation | Occupancy | Linear mean | Indexed mean |
| --- | ---: | ---: | ---: |
| risk check | 102 | 190 ns | 79 ns |
| risk check | 512 | 1,196 ns | 79 ns |
| risk check | 921 | 1,631 ns | 65 ns |
| reservation lookup | 102 | 166 ns | 137 ns |
| reservation lookup | 512 | 635 ns | 52 ns |
| reservation lookup | 921 | 1,081 ns | 50 ns |
| account lookup | 6 | 94 ns | 56 ns |
| account lookup | 32 | 121 ns | 54 ns |
| account lookup | 57 | 150 ns | 54 ns |

These are Windows desktop smoke means. `RiskEngine::<64, 1024>` grew from
25,632 to 93,216 bytes. The added 67 KiB is fixed at construction. Corrected
live fill, cancel, and settle cells later measured 69 to 150 ns mean across 10%
to 90% reservation occupancy, with large run-to-run desktop variance.

## Price Discovery

The sorted level index keeps best-price discovery independent of active level
count for the measured book shapes. A free-slot pool removed the remaining
linear scan from level creation.

| Active shape | Discovery mean | Level create mean |
| --- | ---: | ---: |
| 32 of 64 levels | 77 ns | 119 ns |
| 16 of 128 levels | 76 ns | 228 ns |
| 64 of 128 levels | 86 ns | 214 ns |
| 120 of 128 levels | 76 ns | 217 ns |

These are v0.5 Windows desktop smoke means with 2,000 samples per cell. Later
steady-state runs measured 73 to 157 ns for discovery and 75 to 147 ns for
level creation. The wide ranges are kept because the host was not isolated.

## Gateway and Wire Parsing

The gateway packet-to-report cell parses two fixed frames, enforces session
sequence, applies risk, matches, writes reports, and updates the stable digest.
It excludes kernel and network work.

A five-run Windows smoke set processed 200,000 messages per run. Reported means
were 128, 106, 76, 68, and 68 ns per message. Median p50 and p90 were 100 ns.
Median p99 and p99.9 were 200 and 300 ns. Maxima ranged from 300 ns to 73.4 us.
Every run recorded zero allocation deltas, queue occupancy 64, one explicit
backpressure event, and the same digest.

The reproducible suite later added alternating new-order and cancel parsing, a
20,000-command seeded gateway mix, and deep-book takers that cross 1, 8, or 64
levels. Each cell records a checksum and asserts the expected work.

## Order Policies and Replace

All values below are Windows desktop smoke means.

| Area | Scenario | Mean |
| --- | --- | ---: |
| IOC | empty book | 90 ns |
| IOC | partial fill | 116 ns |
| FOK | preflight rejection | 60 ns |
| FOK | eight-level fill | 334 ns |
| Post-only | crossing check | 48 to 110 ns |
| Post-only | non-crossing rest | 48 to 108 ns |
| Replace | reduce in place | 48 ns |
| Replace | increase | 93 ns |
| Replace | reprice | 89 ns |
| Replace | unknown order rejection | 86 ns |
| Risk | reservation adjustment | 56 ns |

IOC never rests its remainder. FOK preflights full execution before mutation.
Post-only checks crossing without walking all levels. Replace reductions keep
priority. Increases and price changes remove and reinsert the order.

## SPSC

The queue benchmark runs a seeded push and pop walk and records occupancy and
backpressure. The Windows smoke mean remained 43 ns before and after the Loom
test refactor. Maximum occupancy was 85, with no backpressure in that workload
and zero allocation deltas.

Loom runs the shipped queue algorithm across publication, consumption,
wraparound, and full-queue interleavings. Miri covers the crate with reduced
iteration counts. Weakening Release publication makes Miri report a data race.
These checks support the memory-ordering argument. They are not latency tests.

## Session and Recovery Window

The session cells time active admission through the gateway and duplicate
rejection before the gateway. Windows desktop smoke means were 144 ns and
41 ns. Both measured zero allocation deltas.

The retransmission benchmark retains accepted frames in a bounded window,
confirms a prefix outside the timed region, then times refill and replay of the
remaining suffix. It measures in-memory session recovery. It does not measure a
durable journal restart.

## Journal

The journal uses a 64-byte versioned record with a sequence, payload length,
CRC32C, and fixed payload storage. The matching side creates the record and
publishes it to a bounded SPSC queue. Persistence runs on the consumer side.
`EveryBatch` calls `sync_data` after each nonempty batch. `OnShutdown` defers
that call until clean shutdown.

The current AMD Windows smoke run used SSE4.2 CRC32C. Every cell below recorded
zero allocation and deallocation deltas.

| Component cell | Samples | Mean | p50 | p90 | p99 | p99.9 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Previous FNV-derived checksum | 2,000 | 50 ns | 100 ns | 100 ns | 100 ns | 100 ns | 100 ns |
| Selected CRC32C checksum | 2,000 | 38 ns | 0 ns | 100 ns | 100 ns | 100 ns | 100 ns |
| Complete record creation | 2,000 | 51 ns | 100 ns | 100 ns | 100 ns | 100 ns | 200 ns |
| Record verification | 2,000 | 44 ns | 0 ns | 100 ns | 100 ns | 100 ns | 100 ns |
| Enqueue | 2,000 | 48 ns | 0 ns | 100 ns | 100 ns | 100 ns | 100 ns |
| In-memory persistence, batch 1 | 1,023 | 62 ns/record | 100 ns | 100 ns | 100 ns | 100 ns | 100 ns |
| In-memory persistence, batch 8 | 127 | 33 ns/record | 37 ns | 37 ns | 37 ns | 37 ns | 37 ns |
| In-memory persistence, batch 32 | 31 | 32 ns/record | 31 ns | 34 ns | 37 ns | 37 ns | 37 ns |
| Recovery scan, 512 records | 2,000 | 23 ns/record | 23 ns | 23 ns | 24 ns | 34 ns | 37 ns |
| Full-queue refusal | 2,000 | 45 ns | 0 ns | 100 ns | 100 ns | 100 ns | 100 ns |

The CRC comparison includes function dispatch and the complete covered byte
range. It does not justify a claim below the desktop timer resolution. The
selected CRC was still faster in the mean on this run and has standard error
detection behavior.

The persistence batch cells use a fixed in-memory sink. They measure record
verification, encoding, batching, and sink copies. They do not measure disk or
flush latency. No filesystem timing is published. File recovery, short writes,
flush failures, corrupt tails, truncated tails, duplicates, and gaps are
correctness fixtures only.

## Allocation Policy

Zero allocation means zero measured heap allocation and deallocation after
fixture construction and warmup for the named benchmark boundary. It does not
mean that process startup, benchmark setup, persistence, or every library API
is allocation-free.

Fixed storage makes memory costs explicit. Index planes, stable slot links,
report buffers, retransmission windows, and SPSC slots consume memory before
the measured path starts. Performance comparisons report those costs when the
layout changed.

## Historical Changes

| Release | Change | Evidence outcome |
| --- | --- | --- |
| v0.2 | Order ID index | 1,329 to 26 ns median cancel at 512 levels |
| v0.3 | Stable FIFO slots | Depth-512 head cancel 3,300 to 100 ns p50 |
| v0.4 | Risk indexes | Lookup stopped scaling linearly with occupancy |
| v0.5 | Sorted price index | 76 to 86 ns discovery across tested shapes |
| v0.6 | Match preflight and indexed level lifecycle | Rejection became mutation-free |
| v0.6.1 | Warmup and steady-state repair | Removed cold and depleted fixture samples |
| v0.8 | JSON suite and workload checksums | Found an invalid multi-fill fixture and a release-only index defect |
| v0.9 | IOC and FOK | Added policy-specific component cells |
| v0.10 | Post-only | Added shallow and deep crossing checks |
| v0.11 | Replace | Split reduce, increase, reprice, rejection, and risk work |
| v0.12 | Shipped-algorithm Loom tests | Queue smoke mean stayed at 43 ns |
| v0.14 | Session state machine | Split active admission from early rejection |
| v0.15 | Retransmission window | Added bounded in-memory replay workload |
| v0.16 | Bounded command journal | Added versioned records, persistence, recovery, and fault fixtures |

Historical absolute timings are desktop smoke evidence. Changes to fixtures or
measurement boundaries make some cells unsuitable for direct comparison. The
invalid v0.4 destructive risk cells and pre-v0.8 multi-fill result are retained
only as benchmark mistakes, not performance results.

## Known Limitations

- No qualified Linux host result exists.
- No hardware counter data is published.
- No network benchmark is published.
- `Instant` resolution is close to many component timings.
- The Windows host is shared and unisolated.
- Journal filesystem write and flush latency is not measured.
- Means from different historical harness versions are not always comparable.
- Maximum latency on the desktop often reflects scheduler interference.

## Reproduction

Run the full release suite:

```text
cargo run --release -p hft-bench
```

Run the reduced schema and allocation smoke test:

```text
cargo test -p hft-bench --test suite_smoke
cargo test -p hft-bench --test schema_fixture
```

Before recording results, run the repository validation commands:

```text
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

A Linux qualification run must capture the environment beside the raw JSON and
use pinned benchmark processes. Collect cycles, instructions, branches, branch
misses, cache references, cache misses, context switches, CPU migrations, and
page faults with `perf stat`. Use `perf record` for profile evidence. Do not
publish container timings as dedicated host results.

The checked qualification tooling is under `scripts/linux`:

```text
scripts/linux/capture_environment.sh results/environment.txt
scripts/linux/check_qualification.sh --cpu 4
scripts/linux/run_qualification.sh --cpu 4 --output results
docker build -f scripts/linux/Dockerfile -t hft-linux-tooling .
```
