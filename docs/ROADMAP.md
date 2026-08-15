# Roadmap

## Scope and Rules

This is a library-first deterministic execution engine. The matching core is
single-writer and fixed-capacity. Measured hot paths perform no heap allocation
after initialization and contain no locks, blocking I/O, logging, formatting,
or syscalls.

Status is evidence-based:

- **Implemented:** code, tests, documentation, and required evidence exist.
- **Next:** the only committed implementation milestone.
- **Planned:** dependency-ordered work with no completion claim.
- **Experimental:** optional work that cannot define core APIs.
- **Post-v1 research:** hardware or venue-specific investigation.

Every release requires formatting, workspace check, Clippy with warnings
denied, tests, relevant Miri/Loom/sanitizer/property coverage, explicit capacity
and failure behavior, and a documented limitation. Performance changes require
same-workload before/after release results with workload, configuration, CPU,
OS, compiler, memory cost, and allocation delta. CI and Windows timings are not
production-latency evidence.

## Safety and Native Boundaries Before v0.3

- Public APIs, matching, risk, parsing, sessions, and replay remain safe Rust.
- Repository-owned unsafe sites require a documented invariant and test.
- Keep the audited Rust SPSC; do not move it to C++ merely to hide unsafe code.
- Use optional C++ only for a real vendor/NIC SDK behind a C ABI with opaque
  handles, fixed-width fields, explicit ownership/errors, and no exceptions or
  C++ standard-library types across the boundary.
- Default builds stay Rust-only. Native shims require ABI tests and ASan/UBSan.

This is immediate policy and verification work, not an engine feature release.
Policy status: safe-crate forbids and the unsafe allowlist are CI-enforced;
each unsafe site has a documented invariant and test ([safety](SAFETY.md));
the `vendor-sdk` C ABI tests run under ASan/UBSan in CI.

## Implemented

### v0.1.0 - Deterministic Vertical Slice

Borrowed parsing, RAII frame ownership, fixed-capacity risk/matching, bounded
SPSC handoff, owner-authorized cancel, replay digest, and portable CI. Remaining
hot-path limits were linear risk lookup, linear price discovery, FIFO shifts,
and duplicate matching traversal during preflight.

### v0.2.0 - Indexed Order Lookup

Fixed-capacity `OrderId -> resting slot` lookup with deterministic back-shift
deletion. Collision, relocation, fill, cancel, and slot-reuse invariants pass.
The documented 512-level desktop workload measured 1,329 ns to 26 ns median
cancellation latency at a 64 KiB book-size cost. See the
[performance methodology](PERFORMANCE.md) for the workload and evidence.
Per-level FIFO shifts remain.

### v0.3.0 - Stable-Slot FIFO Levels

Intrusive doubly linked per-level FIFOs with head, tail, free-list, and stable
slot handles replaced array shifts. The order index maps IDs to stable slot
handles, never FIFO positions. Handle stability across unrelated mutations,
stale-handle fail-closed behavior, disjoint live/free sets, atomic full-level
rejection, and a 600-command array-model comparison pass. The documented depth
harness measured flat 100 ns p50 head/middle/tail cancel and head fill at
depths 1 through 512 (down from 3,300/1,400/100/3,100 ns at depth 512) for a
+14% `OrderBook<1, 512>` memory cost. See the
[performance methodology](PERFORMANCE.md) for the workload and evidence.
Best-price and risk lookup stay linear.

## Next

### v0.4.0 - Indexed Risk State

- **Purpose / scope:** add fixed-capacity `AccountId -> AccountState` and
  `OrderId -> Reservation` indices plus a bounded reservation free-list. Define
  index load; do not use `HashMap`.
- **Invariants / tests:** collisions/deletion, duplicate/full rejection, stable
  handles, and reservation totals equal live exposure after fill/cancel/settle.
- **Benchmarks:** risk check, both lookups, reservation insert, fill, cancel,
  settle, and reject at 10%, 50%, and 90% configured occupancy versus linear
  lookup; record memory cost.
- **Exit:** expected O(1) lookup at the defined load, zero measured allocation,
  unchanged logical digest, and published before/after results.
- **Dependencies / limits:** follows v0.3 stable slots; account registration may
  remain cold-path.

## Planned Core Work

### v0.5.0 - Price-Level Discovery

- **Purpose / scope:** benchmark the current scan against a dense ladder with
  bitmap, fixed ordered index, and bounded sparse index; select by measured book
  shapes, not theoretical complexity.
- **Invariants / tests:** exact price priority, correct empty/full transitions,
  model-equivalent best bid/ask, boundary prices, and no skipped liquidity.
- **Benchmarks:** insert, remove-last, best bid/ask, single-level match, and
  multi-level walk for shallow/deep and dense/sparse books; record cache misses,
  branches, memory, and price span.
- **Exit:** implement the measured winner or retain scanning with published
  evidence.
- **Dependencies / limits:** requires v0.3 traversal; an instrument-specific
  configuration may be necessary.

### v0.6.0 - Matching Transaction Plan

- **Purpose / scope:** compare full simulation with compact match plans, precise
  report bounds, and fixed-capacity rollback metadata to avoid duplicate walks.
- **Invariants / tests:** capacity rejection is atomic; quantity is conserved;
  each maker appears once; plan application equals the reference matcher;
  failures after each report append or mutation restore the book, report buffer,
  reservation state, and plan metadata; success commits them together.
- **Benchmarks:** non-crossing, single/multi-fill, report-full, and deep rejection;
  record traversal count, branches, latency, allocations, and metadata size.
- **Exit:** adopt a design only if failure atomicity holds and target workloads
  improve.
- **Dependencies / limits:** requires v0.3 and v0.5; FOK still needs complete
  liquidity proof.

### v0.7.0 - Property and Reference Models

- **Purpose / scope:** add deterministic bounded command generation and simple
  reference models before more order semantics.
- **Invariants / tests:** quantity conservation, non-negative remainder,
  price-time priority, owner cancel, unique terminal transition, reservation
  equals live exposure, replay equality, and lossless accepted SPSC elements.
  Later features must add FOK atomicity, IOC/post-only behavior, replace
  atomicity, snapshot-tail equality, and deterministic sharding properties.
- **Benchmarks:** run the release suite to detect instrumentation regressions;
  property-test speed is informational.
- **Exit:** reproducible seeds and model comparisons run in CI.
- **Dependencies / limits:** follows v0.3-v0.6; state exploration is bounded.

### v0.8.0 - Reproducible Benchmark Suite

- **Purpose / scope:** add parser; all risk operations; rest; best-price match;
  single/multi-fill; head/middle/tail cancel; SPSC; gateway; mixed/deep-book;
  and high-reservation-occupancy workloads with fixed seeded distributions.
  IOC, FOK, post-only, and replace releases must add their cases to this suite.
- **Invariants / tests:** deterministic workloads/checksums, warm-up and sample
  validation, allocation failure, and a machine-readable schema fixture.
- **Benchmarks:** p50/p90/p99/p99.9/max, throughput, cycles, instructions,
  branches/misses, cache misses, allocations, queue occupancy, and book shape.
- **Exit:** commit-comparable output with component, gateway, and network
  boundaries separated.
- **Dependencies / limits:** follows v0.4-v0.7; CI validates correctness, not
  latency.

## Planned Order Semantics

### v0.9.0 - IOC and FOK

- **Purpose / scope:** IOC executes available quantity and never rests; FOK
  executes fully or performs no mutation.
- **Invariants / tests:** FOK atomic across book/reports/risk, IOC never rests,
  quantity conserved, and capacity rejection deterministic.
- **Benchmarks:** IOC empty/partial/full and FOK reject/single/multi-fill versus
  equivalent limit paths.
- **Exit:** property/replay/allocation gates pass.
- **Dependencies / limits:** requires v0.6-v0.8; no post-only/replace.

### v0.10.0 - Post-Only

- **Purpose / scope:** reject liquidity-taking post-only orders before mutation.
- **Invariants / tests:** never trades; accepted order joins FIFO tail; rejection
  leaves book/risk unchanged.
- **Benchmarks:** crossing/non-crossing at shallow and deep level counts.
- **Exit:** model, replay, and allocation gates pass.
- **Dependencies / limits:** uses v0.5 best-price API; no replace.

### v0.11.0 - Replace and Order Lifecycle

- **Purpose / scope:** add owned quantity reduction/increase and price change;
  price changes/increases lose priority, allowed reductions retain it.
- **Invariants / tests:** rejected replace atomic, reservation restored on
  failure, terminal orders immutable, no double fill/cancel, explicit states.
- **Benchmarks:** reduce/increase/reprice and accepted/rejected paths; report book
  and risk costs separately.
- **Exit:** property model covers replace churn and every transition.
- **Dependencies / limits:** requires v0.3, v0.4, v0.7, v0.8; venue-specific
  amend policies remain out of scope.

## Planned Verification and Qualification

### v0.12.0 - Actual SPSC Loom and Unsafe Audit

- **Purpose / scope:** model the actual queue algorithm where practical; audit
  `UnsafeCell`, initialization/drop, wrap, full/empty, and endpoint lifetimes.
- **Invariants / tests:** accepted values appear once in FIFO order; no read
  before publication or overwrite before reclamation; values drop once. Use
  Miri for relevant unsafe paths.
- **Benchmarks:** prove testability changes do not regress SPSC latency,
  throughput, cache traffic, or allocation behavior.
- **Exit:** actual-algorithm Loom coverage; if infeasible, require a bounded model
  of the same state machine, updated safety proof, Miri/stress evidence, and
  independent review. A blocker alone cannot pass.
- **Dependencies / limits:** requires v0.8; Loom explores bounded executions.

### v0.13.0 - Dedicated Linux Qualification

- **Purpose / scope:** publish a dedicated-host protocol recording CPU,
  microcode, kernel, compiler/target, governor, turbo, SMT, isolation, affinity,
  IRQs, NUMA, memory, mitigations, NIC/firmware, warm-up, workload, and samples.
- **Invariants / tests:** Linux and Windows correctness produce matching digests
  and benchmark checksums.
- **Benchmarks:** full v0.8 suite with `perf` cycles, instructions, branches,
  misses, cache misses, switches, migrations, and page faults.
- **Exit:** raw results and environment manifest published; claims name hardware
  and measured boundary.
- **Dependencies / limits:** requires v0.8/v0.12; results do not generalize.

## Planned Reliability

### v0.14.0 - Session State Machine

- **Purpose / scope:** disconnected, connecting, logon, active, recovering,
  logout, and failed states outside matching.
- **Invariants / tests:** commands mutate only while active; invalid transitions,
  duplicate, and gap fail closed without advancing state.
- **Benchmarks:** active admission and rejection versus gateway baseline.
- **Exit:** transition table, virtual-time tests, and replay fixtures pass.
- **Dependencies / limits:** requires v0.7; recovery policy follows.

### v0.15.0 - Session Recovery

- **Purpose / scope:** bounded heartbeat, timeout, retained messages,
  retransmission, reconnect, and logout completion.
- **Invariants / tests:** deterministic timeout/reconnect; delayed/reordered/
  duplicated input cannot duplicate commands; retention exhaustion is explicit.
- **Benchmarks:** active traffic, heartbeat, gap entry, replay burst, and full
  retention.
- **Exit:** failure tests and deterministic traffic generator pass.
- **Dependencies / limits:** requires v0.14; no durable recovery.

### v0.16.0 - Bounded Command Journal

- **Purpose / scope:** versioned accepted-command records with sequence/checksum,
  bounded SPSC persistence handoff, flush policy, and explicit backpressure.
- **Invariants / tests:** records occur once/in order; corruption/truncation fails
  closed; saturation is explicit; matching thread performs no storage syscall.
- **Benchmarks:** enqueue, queue occupancy, consumer throughput, off-core flush,
  and saturation.
- **Exit:** crash-point fixtures and allocation checks pass.
- **Dependencies / limits:** requires v0.12/v0.15; snapshot recovery follows.

### v0.17.0 - Recovery and State Integrity

- **Purpose / scope:** canonical versioned state, snapshot plus journal tail,
  compatibility fixtures, and strong off-hot-path hash. Keep the fast 64-bit
  digest for development only.
- **Invariants / tests:** full replay equals snapshot plus tail; canonical bytes
  are portable; corruption/version mismatch rejected; golden formats honored.
- **Benchmarks:** recovery throughput, snapshot size/time, strong-hash cost, and
  journal-tail length; no cryptographic hashing in matching.
- **Exit:** crash/restart reproduces logical state and integrity value.
- **Dependencies / limits:** requires v0.16; replication remains later.

### v0.18.0 - Bounded Events

- **Purpose / scope:** sequenced accepted/rejected/trade/cancel/replace and
  top-of-book events with bounded handoff and explicit backpressure.
- **Invariants / tests:** no accepted state transition is silently lost; event
  order and replay are stable; exhaustion has a defined engine response.
- **Benchmarks:** event creation, enqueue/dequeue, queue occupancy, saturation,
  and gateway overhead with/without consumers.
- **Exit:** model tests and machine-readable mixed workload pass.
- **Dependencies / limits:** requires v0.8/v0.11/v0.12/v0.17; single instrument
  only.

### v0.19.0 - Multi-Instrument Routing

- **Purpose / scope:** deterministic instrument-to-shard mapping, independent
  single-writer books, and bounded command/event queues between router/shards.
- **Invariants / tests:** routing is stable; unknown instruments reject; shards
  cannot mutate each other; accepted queue elements are not lost/duplicated.
- **Benchmarks:** route lookup, instrument/shard counts, cross-core handoff,
  queue occupancy, backpressure, and mixed load imbalance.
- **Exit:** routing properties, actual SPSC verification, and mixed workload pass.
- **Dependencies / limits:** requires v0.12/v0.18; one book is never distributed.

### v0.20.0 - Fault Injection and Soak

- **Purpose / scope:** hours of deterministic cancel/replace/fill churn, gaps,
  reconnect, queue pressure, journal stalls, snapshots, recovery, malformed
  input, exhaustion, routing imbalance, and shutdown races.
- **Invariants / tests:** retain seeds; final reference state matches; accepted
  elements are not lost/duplicated; every exhaustion path is explicit.
- **Benchmarks:** RSS, allocations, descriptors, queue high-water marks,
  journal/event lag, integrity, throughput, and latency drift.
- **Exit:** no unexplained growth or divergence for the declared run.
- **Dependencies / limits:** requires v0.11/v0.14-v0.19; not certification.

## Pre-v1 Stabilization

- Add a small `hft-engine` facade, validated builder, bounded command/report/event
  API, ownership/backpressure rustdoc, MSRV/features, examples, and format/API
  compatibility policy. The public API cannot bypass sequence, risk, or capacity.
- Add only the operational harness needed to validate configuration, health,
  drain, shutdown, backup/restore, upgrade, rollback, and recovery. Keep it
  outside the library core.
- Measure facade overhead against the internal gateway and rerun the dedicated
  Linux suite. No new hot allocation or unexplained regression is accepted.
- Require independent API, unsafe-boundary, recovery-format, and operations
  review before creating a v1 release candidate.

## v1.0 Entry Criteria

- Stable library API; deterministic matching/risk/sessions/events/recovery.
- Zero measured post-initialization allocation on declared hot paths.
- Explicit overload behavior for every capacity and queue.
- Dedicated-Linux raw evidence with no network-latency overclaim.
- Miri, v0.12 actual-algorithm Loom or its required compensating evidence,
  sanitizers, property/fuzz, crash, and soak qualification.
- Install, validate, drain, shutdown, backup/restore, upgrade/rollback, health,
  and incident runbooks for any reference service.
- Explicit unsupported venue, regulatory, hardware, and deployment requirements.

## Experimental

- Tuned Linux UDP batching only after v0.13; it stays outside the engine API.
- Real vendor/NIC SDK shims may use optional C++ under the audited C ABI policy.

## Post-v1 Research

AF_XDP, DPDK, huge pages, hardware timestamps, kernel bypass, replication,
standby promotion, and venue adapters. These require hardware and separate
failure/performance evidence.

## Non-Claims

No exchange certification, regulatory compliance, unimplemented venue
compatibility, production readiness before recovery/operations tests, or
production latency without dedicated measurement of the stated boundary.
