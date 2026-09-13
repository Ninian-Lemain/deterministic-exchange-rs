# Performance Evidence

All timings in this document come from Windows desktop smoke runs. No qualified
Linux latency result has been recorded. The tables describe separate builds and
workloads, not one measurement of the current checkout.

The [layout measurements](LAYOUT.md) cover book, risk, and route-table storage
changes and their paired desktop runs.

## What Is Measured

`hft-bench` measures in-memory operations with fixed workloads. It emits one
JSON record per line for each benchmark cell using schema
`hft-bench-results/1`. The suite covers:

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
- canonical snapshot encoding, verified restore, and journal tail replay
- bounded event admission, batch publication, and full-ring refusal
- multi-instrument route publication, shard processing, event retrieval, and
  command-queue refusal
- engine admission against direct event and journal composition

The schema supports component, gateway, and network boundaries. The current
suite records component and gateway cells. It has no network benchmark.

## What Is Not Measured

The timings exclude NIC processing, the kernel network stack, wire transit,
production load balancing, durable journal writes, filesystem flushes, snapshot
publication, replication, and standby promotion. They do not measure end-to-end
exchange latency or define a service objective.

Timer quantization, scheduler preemption, frequency changes, and background work
affect the desktop results. These runs can expose large regressions and invalid
workloads, but cannot establish performance on dedicated hardware.

## Methodology

The release profile uses fat LTO, one codegen unit, and aborting panics. Most
component cells record individual operations with `Instant`. Per-sample timer
cost is included.

Most workloads warm their fixtures before sampling. Destructive workloads use
untimed repair steps where they need a constant state. The session fixture
exceptions are described [below](#session-and-recovery-window). In the full suite,
the gateway pair cell skips 1,000 rest/fill iterations, or 2,000 messages, after
an initial warmup pair. The seeded gateway mix skips 1,000 commands.

Each record includes sample count, mean, p50, p90, p99, p99.9, max, throughput,
allocation deltas, workload parameters, and a checksum field. Maxima are not
trimmed and often include desktop scheduler interference.

The benchmark process uses a counting allocator. Most hot-path cells fail if
an allocation or deallocation occurs within their allocation-check window.
Recovery cells record allocation totals for cold-path work. The
[allocation policy](#allocation-policy) explains the limits of these checks.
Fixtures, sample arrays, input frames, and report storage are prepared outside
the timed operations.

Where implemented, workload assertions and checksums check that the intended
work occurred and help prevent dead-code removal. A timing alone does not
establish which path ran.

## Reference Environments

### Windows desktop smoke environment

The reference runs used:

- AMD Ryzen 7 7735HS with eight cores and sixteen threads
- Windows 11 Home build 26200
- about 28 GB RAM
- Rust 1.96.0 with LLVM 22.1.2
- `x86_64-pc-windows-msvc`
- fat LTO
- one codegen unit
- aborting panics

The desktop was shared and unisolated. `Instant` commonly quantized short cells
to 100 ns. Hardware performance counters were not collected.

The v0.16 journal audit also ran on a Lenovo 83J3 with an AMD Ryzen 7 7735HS,
8 cores, 16 threads, 29.3 GB RAM, Windows 11 Home build 26200, Rust 1.96.0,
and LLVM 22.1.2. Hyper-V was active.

### Qualified Linux environment

Qualification requires a host manifest recording CPU, topology, SMT, NUMA
placement, microcode, kernel, mitigations, governor, frequency behavior,
isolated CPUs, process affinity, IRQ affinity, memory, page size, huge pages,
compiler, target flags, linker, LTO, codegen units, workload, seed, sample count,
capacity, and batch size.

Docker can reproduce the build and profiling tools. Host isolation still has
to be configured and verified separately.

## Recorded Desktop Smoke Summary

These selected values come from different documented Windows runs. They are
historical reference points. Timer resolution limits comparisons between
cells near 100 ns.

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
| Component | Journal enqueue on the AMD reference host | 48 ns mean |
| Component | Snapshot encode on the AMD reference host | 2.2 us p50 |
| Component | Verified snapshot restore on the AMD reference host | 5.5 us p50 |
| Component | Snapshot plus eight-command tail on the AMD reference host | 9.0 us p50 |
| Gateway | Event admission through batch pop on the AMD reference host | 100 ns p50 |
| Gateway | Full event ring refusal on the AMD reference host | 40 ns mean |

The hot-path records reported zero allocation and deallocation deltas. The
three recovery cells allocated 12, 4, and 8 times per sample respectively. The
gateway pair workload retained digest `64321af91735b704`.

## Snapshot Recovery

The v0.17 recovery run used the AMD Windows reference host with 2,000 samples
per cell. The gateway state occupied 37,824 bytes. Its canonical
snapshot was 820 bytes and contained eight applied commands. Tail replay added
eight 64-byte journal records.

| Workload | Mean | p50 | p99 | p99.9 | Max | Allocations/sample |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Canonical snapshot encode | 2.188 us | 2.2 us | 2.3 us | 5.9 us | 17.1 us | 12 |
| Verified snapshot restore | 5.501 us | 5.5 us | 5.8 us | 11.5 us | 24.5 us | 4 |
| Snapshot plus journal tail | 9.163 us | 9.0 us | 13.3 us | 18.6 us | 24.9 us | 8 |

These cells time in-memory encoding, SHA-256 verification, state rebuild, and
tail replay. They do not time file open, write, flush, directory sync, or
snapshot selection after restart.

## Bounded Events

The v0.18 event run used the AMD Windows reference host with 2,000 samples per
cell. The admitted cell includes gateway processing, a two-event batch publish,
and consumer pop. Untimed cancellation restores the fixture. The refusal cell
keeps a capacity-one ring full and retries the same next command.

| Workload | Mean | p50 | p99 | p99.9 | Max | Allocations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Accepted command and batch pop | 119 ns | 100 ns | 200 ns | 200 ns | 200 ns | 0 |
| Full event ring refusal | 40 ns | 0 ns | 100 ns | 100 ns | 100 ns | 0 |

These cells measure an in-process SPSC handoff. They do not include event wire
encoding, network publication, storage, or a second process.

Event admission token commit `1cebd63` was compared with the `c3d5efd` layout
baseline on the same AMD Windows host. Ten paired runs used 2,000 samples per
event or router cell. The second five pairs reversed run order. All 103 cells
retained their checksums and allocation counts. Values below are medians of
per-run means. [Raw results](evidence/admission-2026-09-11.zip) include binary hashes.

| Workload | Before | After | Before range | After range |
| --- | ---: | ---: | --- | --- |
| Accepted command and batch pop | 107 ns | 108 ns | 103 to 116 ns | 105 to 118 ns |
| Full event ring refusal | 38 ns | 37 ns | 36 to 41 ns | 36 to 40 ns |
| Route, process, retrieve event | 106 ns | 109 ns | 102 to 123 ns | 106 to 113 ns |

The routed cell exercises normalized-command admission. The event cells use
the unchanged frame path. The overlapping ranges do not establish a latency
improvement.

## Multi-Instrument Routing

The v0.19 router run used two instruments, two shards, 64 command slots per
shard, and 64 event slots per shard. Five full-suite runs used 2,000 samples
per cell. The table reports the median mean and median percentiles from those
runs.

| Workload | Mean | p50 | p99 | p99.9 | Allocations |
| --- | ---: | ---: | ---: | ---: | ---: |
| Route lookup and command publish | 44 ns | 0 ns | 100 ns | 100 ns | 0 |
| Route, shard processing, event pop | 179 ns | 200 ns | 200 ns | 300 ns | 0 |
| Full command queue refusal | 42 ns | 0 ns | 100 ns | 100 ns | 0 |

Checksums matched across all five runs. At v0.19, the route cell timed binary
search and SPSC publication. The combined cell uses IOC commands and includes
route lookup, command publication, shard processing, event publication, and
event retrieval. It does not cross processes or physical cores.

## Matching and Cancellation

The current cancel path uses a fixed open-addressed order index and stable slot
handles. These historical comparisons measured their introduction.

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
fill, report-capacity rejection, and full-level rejection. A suite audit found
that the original multi-fill fixture crossed only one maker. Its results are
invalid. The corrected fixture asserts eight reports and restores makers
outside the timed region.

## Risk

Fixed open-addressed indexes replaced account and reservation scans whose cost
grew with occupancy. The table retains valid historical lookup results. The
original v0.4 fill, cancel, and settle cells reused closed reservation IDs and
measured rejection. Those results are withdrawn.

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

These are Windows desktop smoke means. At that change,
`RiskEngine::<64, 1024>` grew from 25,632 to 93,216 bytes. The added 67 KiB was
fixed at construction. Later storage reductions are recorded in
[Layout measurements](LAYOUT.md). Corrected live fill, cancel, and settle cells
later measured 69 to 150 ns mean across 10% to 90% reservation occupancy, with
large run-to-run desktop variance.

## Price Discovery

The current book uses a sorted level index for best-price discovery and a
free-slot pool for level creation. The v0.5 results below measured discovery
after the sorted index was added, before the later level-creation improvements.

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

The gateway packet-to-report cell alternates rest and fill frames. It parses
each frame, enforces sequence, applies risk, matches, and writes reports. Frame
encoding and the final stable digest calculation are outside the timer. Kernel
and network work are excluded.

A five-run Windows smoke set processed 200,000 messages per run. Reported means
were 128, 106, 76, 68, and 68 ns per message. Median p50 and p90 were 100 ns.
Median p99 and p99.9 were 200 and 300 ns. Maxima ranged from 300 ns to 73.4 us.
Every run recorded zero allocation deltas, queue occupancy 64, one explicit
backpressure event, and the same digest.

The reproducible suite later added alternating new-order and cancel parsing, a
20,000-command seeded gateway mix, and deep-book takers that cross 1, 8, or 64
levels. Each cell records a checksum and asserts the expected work.

## Book Totals and Direct Routing

Ten paired full-suite runs on 2026-09-12 used the same AMD Windows host and
Rust 1.96.0 release settings. Both processes used logical CPU 2 with affinity
mask `0x4`. That CPU was not isolated. The second five pairs reversed run order.
The baseline was `011ca7b` with benchmark changes from `1dca1b5`. The final
candidate was `795908a`.

Values below are medians of ten per-run means. Book reads inspect both sides,
with 2,000 samples after 64 warmup iterations. The cycle submits one order,
reads both sides, cancels it, and reads again. Lookup cells use 2,000 batches
after 128 warmup batches. Each batch contains 64 calls. Reported integer
nanoseconds per call include the batch loop and result storage.

| Workload | Before ns | After ns |
| --- | ---: | ---: |
| Best-level pair, 8 orders per side | 55 | 37 |
| Best-level pair, 64 orders per side | 334.5 | 37 |
| Best-level pair, 512 orders per side | 5,948.5 | 37 |
| Submit/read/cancel/read, depth 64 | 1,779.5 | 108 |
| Route, process, retrieve event | 179.5 | 164 |
| Dense route hit, 64 instruments | 4 | 1.5 |
| Dense route hit, 1,024 instruments | 8.5 | 3.5 |
| Sparse route hit, 64 instruments | 6.5 | 8 |
| Sparse route hit, 1,024 instruments | 12.5 | 17 |
| Reverse lookup, 1,024 instruments | 1 | 1.5 |

Cached level totals remove FIFO walks from event publication. Sparse lookup
still performs binary search after checking the table's fixed lookup mode.
Reverse lookup saves storage but adds an array read. The sparse and reverse
lookup cells regressed in this comparison. Route/process/event means ranged
from 122 to 377 ns before and 126 to 498 ns after. Dedicated Linux runs are
needed to assess sparse lookup costs and combined route latency.

All 124 cells matched checksums, sample counts, allocation counts, and
parameters other than measured structure sizes. Hot-path records reported zero
allocations. Recovery cells retain their cold-path allocations. All four
retained soak seeds passed one million declared steps with the final build.
These were bounded soak runs, not multi-hour evidence.
[Raw evidence](evidence/cache-2026-09-12.zip) includes both the initial
unpinned experiment and the final pinned comparison. The initial speculative
route probe was replaced with construction-time strategy selection.

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
| Replace | unknown order rejection | Withdrawn |
| Risk | reservation adjustment | 56 ns |

IOC never rests its remainder. FOK preflights full execution before mutation.
Post-only checks crossing without walking all levels. Strict same-price
reductions keep priority. Other accepted replaces remove and reinsert the order.

The withdrawn 86 ns unknown-order result measured a successful replacement.
The corrected fixture uses a missing ID. Current replacement fixtures include
warmup and allocation checks around the operation. Earlier replacement results
lacked those checks.

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
41 ns. Both records report zero allocation deltas, but the current counter
checks start after sampling and cover statistics processing. They do not
verify allocation behavior during admission or rejection.

The admission and rejection cells discard operation results and do not check a
final state digest. Their timings are not evidence that every sampled command
took the intended path.

The retransmission cell starts with 64 retained frames. It confirms sequence
numbers 32 through 47 cyclically, then times refill and replay. After the first
16 samples, confirmation stops freeing slots and sampling measures replay
without refill. Its allocation assertion compares two counter reads taken
after sampling, not a before-and-after delta. This cell does not establish
sustained refill performance, allocation behavior, or durable journal recovery.

## Journal

The journal uses a 64-byte versioned record with a sequence, payload length,
CRC32C, and fixed payload storage. The matching side creates the record and
publishes it to a bounded SPSC queue. Persistence runs on the consumer side.
`EveryBatch` flushes the sink after each nonempty batch. `OnShutdown` defers
flushing until clean shutdown. The file sink calls `sync_data` when flushed.

The v0.16 AMD Windows smoke run used SSE4.2 CRC32C. Every cell below recorded
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
range. CRC32C had a lower mean in this run, but the timer resolution limits
conclusions about the difference. CRC32C also provides standard error
detection behavior.

The persistence batch cells use a fixed in-memory sink. They measure record
verification, encoding, batching, and sink copies. They do not measure disk or
flush latency. No filesystem timing is published. File recovery, short writes,
flush failures, corrupt tails, truncated tails, duplicates, and gaps are
correctness fixtures only.

Ten paired runs on 2026-09-11 compared controlled journal channels before and
after shared persistence status. The baseline was `1acb481` with the same
benchmark workloads as candidate `658704a`. The second five pairs reversed
run order. Values below are medians of ten per-run means on the same AMD
Windows host.

| Controlled channel cell | Before | After |
| --- | ---: | ---: |
| Enqueue | 52 ns | 50.5 ns |
| Batch 1, every-batch flush | 73 ns/record | 70.5 ns/record |
| Batch 1, shutdown flush | 73 ns/record | 68 ns/record |
| Batch 8, every-batch flush | 33.5 ns/record | 34 ns/record |
| Batch 8, shutdown flush | 33.5 ns/record | 34 ns/record |
| Batch 32, every-batch flush | 33 ns/record | 32 ns/record |
| Batch 32, shutdown flush | 33 ns/record | 31 ns/record |

All 110 cells matched parameters, sample counts, checksums and allocation
counts in each pair. The controlled journal cells allocated nothing. Baseline
enqueue means ranged from 49 to 1,460 ns, compared with 48 to 68 ns after.
These mixed desktop results do not establish a speedup. The sink was memory,
so flush timing does not represent storage durability latency.
[Raw results](evidence/journal-status-2026-09-11.zip) include sample budgets,
build details and binary hashes.

## Engine Boundary

Ten runs on 2026-09-12 compared `hft-engine` with direct composition of the
event engine and journal writer in the same binary, built from
`14f01b6441eeb879181378b9d5a55213b949fe1d`. Each cell alternates a resting new
order and its cancel. The timer includes joint admission, journal record
encoding and enqueue, matching, and event publication. Event drain and
persistence drain run outside the timer with a fixed sink that discards bytes.

| Path | Median of run means | Range of run means | Admission owner and queues |
| --- | ---: | ---: | ---: |
| Direct composition | 137 ns | 125 to 190 ns | 69,560 bytes |
| Engine facade | 140.5 ns | 125 to 252 ns | 69,648 bytes |

The observed mean difference is +3.5 ns (+2.6%). Storage increases by 88 bytes
(+0.13%) for this shape. Both paths record zero allocations and deallocations.
They produce equal final snapshot digests and checksum `0000000000227068`.
Storage totals include the admission owner and both queue buffers. They exclude
the event consumer, persistence worker, and benchmark arrays.

The facade adds persistence failure polling, sequence alignment checks, and
lifecycle ownership. The direct path has no failure polling. This comparison
measures the cost of those boundary responsibilities. It does not measure a
matching optimization or storage durability.

Each cell uses 128 warmup commands and 2,000 samples. The fixture has one
account, eight risk orders, two levels per side, four orders per level, two
reports, four events per batch, two event queue slots, and 1,024 journal slots.
The persistence worker uses batch size one and `OnShutdown` flush. Allocation
checks also cover the untimed drains.

Processes were pinned to logical CPU 2 on the AMD Windows reference host,
without isolation or fixed frequency. Composition always ran first. Timer
quantization and run order limit conclusions about small differences. All 126
suite cells retained their parameters, sample counts, checksums, and allocation
counts across the ten runs. [Raw results](evidence/engine-2026-09-12.zip) include
the build and binary hash. Linux overhead qualification remains open.

## Report Buffer Storage

`ReportBuffer` stores reports directly in a fixed array and tracks a live
prefix. Clearing the buffer resets its length. Iteration needs no per-report
`Option` check.

Five alternating runs compared commit `6f4878f` with the direct-storage build
on the same Windows x86_64 host. The table reports the median of each build's
five mean values.

| Cell | Before | After | Change |
| --- | ---: | ---: | ---: |
| Eight-report multi-fill | 302 ns | 258 ns | -14.6% |
| Gateway rest/fill | 130 ns | 112 ns | -13.8% |
| Admitted event handoff | 154 ns | 144 ns | -6.5% |
| Single fill | 94 ns | 87 ns | -7.4% |

Checksums matched and every cell recorded zero allocation deltas. A 64-entry
buffer changed from 3,592 to 3,080 bytes on this target, a 512-byte reduction.

## Allocation Policy

A verified zero-allocation result means the allocation and deallocation
counters stayed unchanged around the named operation after fixture setup and
warmup. A zero field without that coverage, as in the session cells, is
insufficient. Process startup, benchmark setup, persistence, and other library
APIs may still allocate.

Index planes, stable slot links, report buffers, retransmission windows, and
SPSC slots reserve memory before the measured path starts. Comparisons report
storage costs when the layout changes.

## Historical Changes

| Release | Change | Evidence outcome |
| --- | --- | --- |
| v0.2 | Order ID index | 1,329 to 26 ns median cancel at 512 levels |
| v0.3 | Stable FIFO slots | Depth-512 head cancel 3,300 to 100 ns p50 |
| v0.4 | Risk indexes | Lookup stopped scaling linearly with occupancy |
| v0.5 | Sorted price index | 76 to 86 ns discovery across tested shapes |
| v0.6 | Match preflight and indexed level lifecycle | Book preflight rejects before mutation |
| v0.6.1 | Warmup and steady-state repair | Removed cold and depleted fixture samples |
| v0.8 | JSON suite and workload checksums | Found an invalid multi-fill fixture and a release-only index defect |
| v0.9 | IOC and FOK | Added policy-specific component cells |
| v0.10 | Post-only | Added shallow and deep crossing checks |
| v0.11 | Replace | Split reduce, increase, reprice, rejection, and risk work |
| v0.12 | Shipped-algorithm Loom tests | Queue smoke mean stayed at 43 ns |
| v0.14 | Session state machine | Split active admission from early rejection |
| v0.15 | Retransmission window | Added bounded in-memory replay workload |
| v0.16 | Bounded command journal | Added versioned records, persistence, recovery, and fault fixtures |
| v0.17 | Canonical state recovery | Added snapshot encoding, verified restore, and journal tail replay cells |
| v0.18 | Bounded command events | Added admitted batch handoff and full-ring refusal cells |
| v0.19 | Multi-instrument routing | Added route, shard round-trip, and command pressure cells |

Fixture and measurement-boundary changes make some historical cells unsuitable
for direct comparison. The v0.4 destructive risk cells and pre-v0.8 multi-fill
result remain documented as benchmark errors. They are excluded from
performance evidence.

## Known Limitations

- No qualified Linux host result exists.
- No hardware counter data is published.
- No network benchmark is published.
- `Instant` resolution is close to many component timings.
- The Windows host is shared and unisolated.
- Journal filesystem write and flush latency is not measured.
- Session allocation checks do not cover sampling. Admission and rejection
  results are unchecked, and the retransmission fixture stops refilling.
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
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
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
