# Roadmap

## North Star

Build a library-first Rust execution engine that can be embedded in a simulator,
test venue, or controlled trading service without changing its deterministic
core. The repository should eventually provide three separate products:

1. `hft-engine`: a documented Rust facade for deterministic matching, risk,
   sessions, events, and recovery.
2. `hftd`: an optional service that owns networking, configuration, lifecycle,
   observability, and persistence outside the matching hot path.
3. `hft-cli`: replay, inspection, validation, benchmark, and recovery tools.

Operational means bounded failure behavior, restartable state, observable
health, repeatable deployment, and measured performance. It does not mean venue
certification, regulatory approval, or guaranteed production latency.

## What Makes This Project Credible

The goal is depth and evidence, not feature count. Each milestone must leave a
reviewable engineering result:

| Area | Evidence expected |
| --- | --- |
| Matching | Priority, conservation, ownership, and terminal-state invariants |
| Performance | Same-workload before/after results with allocation checks |
| Concurrency | Single-writer ownership, bounded queues, stress tests, and Loom |
| Recovery | Crash-point tests and identical post-recovery state digests |
| Operations | Health, backpressure, drain, restart, and incident runbooks |
| Library design | Small public facade, examples, rustdoc, and compatibility policy |
| Safety | Minimal audited unsafe code, Miri, sanitizers, and fuzzing |

A smaller implemented milestone with strong evidence is more valuable than a
large unverified feature list.

## Development Rules

- Ship one focused semantic version at a time.
- Preserve deterministic results, price-time priority, and explicit capacity
  failures.
- Keep allocation, blocking I/O, formatting, logging, and unbounded queues out
  of the matching path.
- Benchmark every hot-path change before and after on the same workload.
- Keep each book single-writer; scale by sharding instruments, not by locking a
  shared book.
- Stabilize public APIs only after replay, recovery, fault-injection, and soak
  tests have exercised their invariants.
- Keep networking and vendor SDKs behind replaceable adapters.

## Versioned Release Train

| Version | Focus | State |
| --- | --- | --- |
| v0.1.0 | Deterministic execution vertical slice | Complete |
| v0.2.0 | Fixed-capacity indexed order lookup and fast cancellation | Complete |
| v0.2.1 | Safe-core and optional native-boundary policy | Next |
| v0.3.0 | Efficient FIFO price levels | Queued |
| v0.4.0 | IOC and FOK orders | Planned |
| v0.5.0 | Post-only orders | Planned |
| v0.6.0 | Replace orders and priority rules | Planned |
| v0.7.0 | Explicit order-state model | Planned |
| v0.8.0 | Component and mixed-workload benchmarks | Planned |
| v0.9.0 | Optional Linux CPU affinity | Planned |
| v0.10.0 | Linux performance-counter protocol | Planned |
| v0.11.0 | Connection and session state machine | Planned |
| v0.12.0 | Heartbeats, gaps, retransmission, and reconnect | Planned |
| v0.13.0 | Off-hot-path append-only command journal | Planned |
| v0.14.0 | Deterministic journal recovery | Planned |
| v0.15.0 | Versioned snapshots plus journal tail | Planned |
| v0.16.0 | Bounded market-data and lifecycle events | Planned |
| v0.17.0 | Multi-instrument shard routing | Planned |
| v0.18.0 | Cross-core command and event queues | Planned |
| v0.19.0 | Portfolio and gross-exposure risk | Planned |
| v0.20.0 | Systematic fault injection | Planned |

Only the next release is committed work. Later versions describe sequencing and
acceptance criteria; they are not claims that the features already exist.

## Immediate Next Release: v0.2.1 - Safe Core and Native Boundaries

The project will keep its public engine and domain logic in safe Rust. C++ is an
optional integration tool only when a real vendor or NIC SDK requires it; it is
not a way to hide an otherwise reviewable unsafe Rust implementation.

### Deliverables

- Define the future `hft-engine` facade as safe Rust with
  `#![forbid(unsafe_code)]`.
- Keep matching, risk, parsing, sessions, replay, and operational coordination
  in crates that forbid unsafe Rust.
- Document every repository-owned unsafe site, its invariant, owner, tests, and
  reason that safe Rust cannot express the operation directly.
- Classify `hft-bench` allocator instrumentation as non-shipping test code.
- Keep the lock-free Rust SPSC implementation small and audited unless a
  measured replacement improves the actual system.
- Define `hft-ffi` as an optional integration crate; the default library must
  not require a C++ compiler, native SDK, or vendor runtime.
- Specify a native adapter contract for future C++ SDK shims: stable C ABI,
  opaque handles, fixed-width scalars, explicit ownership and error codes, no
  exceptions or C++ standard-library types across the boundary, and no hidden
  callbacks on matching threads.
- Extend CI policy to verify the owned-source unsafe allowlist, Miri coverage,
  FFI layout tests, and ASan/UBSan for native shims when one is introduced.
- Add a dependency-safety audit that distinguishes repository-owned unsafe code
  from audited unsafe used internally by Rust dependencies.

### Acceptance Criteria

- A reviewer can identify every unsafe boundary from one document.
- All core crates continue to reject new unsafe code at compile time.
- Default builds and tests remain Rust-only and portable.
- Any future C++ adapter can be removed without changing matching, risk, replay,
  or the public command/event model.
- README wording says "safe Rust core with audited low-level boundaries" rather
  than making an unverifiable zero-unsafe claim.

### Non-Goals

- Do not rewrite the SPSC queue in C++ merely to move unsafe code out of sight.
- Do not add a placeholder C++ library before a real SDK or measured need
  exists.
- Do not claim that FFI makes C++ memory-safe or eliminates unsafe operations.

## Milestone 1: Matching Core - v0.2.0 to v0.8.0

### v0.2.0 - Indexed Order Lookup

- Map `OrderId` to the resting slot with fixed-capacity storage.
- Keep lookup deterministic and expected O(1) at the configured load factor.
- Preserve FIFO priority across collisions, fills, cancellation, and deletion.
- Publish an allocation-checked before/after cancellation benchmark.

Exit: collision and relocation invariants pass, and the release records both
the measured latency gain and memory cost.

### v0.3.0 - Efficient FIFO Levels

- Replace routine array shifting with fixed-capacity indexed or intrusive FIFO
  links.
- Keep stable order handles and exact insertion priority.
- Cover head, middle, tail, full-level, fill, cancel, and slot-reuse cases.
- Benchmark cancel and match across several per-level occupancies.

Exit: ordinary head fill and cancellation do not copy the rest of a level, and
zero measured hot-path allocation remains enforced.

### v0.4.0 - IOC and FOK

- IOC executes available quantity and never rests.
- FOK executes completely or leaves book, reports, and risk unchanged.
- Preflight liquidity and report capacity before mutation.
- Add replay fixtures for accepted and rejected paths.

### v0.5.0 - Post-Only

- Reject a post-only order that would immediately cross.
- Rest a non-crossing order with normal price-time priority.
- Define validation ordering and deterministic rejection reasons.

### v0.6.0 - Replace Orders

- Support owned price and quantity replacement.
- Price changes and quantity increases lose queue priority.
- Safe quantity reductions retain priority when policy permits.
- Rejected replaces leave matching and risk state unchanged.

### v0.7.0 - Order-State Model

- Model accepted, partially filled, filled, cancelled, replaced, and rejected
  transitions explicitly.
- Prevent double execution, double cancellation, and terminal-state reuse.
- Emit replayable lifecycle events without exposing internal slots.

### v0.8.0 - Benchmark Suite

- Separate parser, risk, insert, cancel, match, SPSC, gateway, replay, and mixed
  workloads.
- Report p50, p90, p99, p99.9, max, throughput, allocation delta, and book
  shape.
- Emit machine-readable results for local comparison.
- Define regression budgets only after a dedicated runner has a stable baseline.

Milestone exit: the matching core supports realistic order behavior with clear
invariants and component-level performance evidence.

## Milestone 2: Host and Session Reliability - v0.9.0 to v0.12.0

### v0.9.0 - Linux CPU Affinity

- Add optional matching, ingress, journal, and event-thread pinning.
- Validate CPU sets and NUMA intent on the cold path.
- Keep Windows, macOS, and unpinned Linux supported.

### v0.10.0 - Performance Counters

- Document reproducible `perf stat` and `perf record` workflows.
- Capture cycles, instructions, branches, branch misses, cache misses, context
  switches, migrations, and page faults.
- Record compiler, kernel, mitigations, governor, affinity, and topology.

### v0.11.0 - Session State Machine

- Add disconnected, connecting, logon, active, recovering, logout, and failed
  states outside the order book.
- Reject application messages outside the active state.
- Make each transition explicit, testable, and replay-visible.

### v0.12.0 - Session Reliability

- Add heartbeat deadlines, sequence gaps, duplicates, retransmission requests,
  timeout policy, reconnect, and logout completion.
- Bound retained retransmission state.
- Test delayed, reordered, duplicated, and missing messages with virtual time.

Milestone exit: timing can be qualified on controlled hardware, and session
failures cannot silently mutate engine state or create an unbounded queue.

## Milestone 3: Persistence and Recovery - v0.13.0 to v0.15.0

### v0.13.0 - Command Journal

- Journal accepted state-changing commands with checksums and monotonic record
  sequence numbers.
- Use a bounded persistence handoff; never write storage from the matching
  critical path.
- Define fail-closed behavior when durability cannot keep up.

### v0.14.0 - Recovery

- Rebuild engine state from the journal and reproduce its stable digest.
- Detect truncation, corruption, duplicates, and sequence gaps.
- Add crash-point tests around publication and durable flush.

### v0.15.0 - Snapshots

- Write versioned deterministic snapshots off the matching core.
- Recover from snapshot plus journal tail and verify the final digest.
- Publish atomically and retain a known-good rollback snapshot.

Milestone exit: a killed process restarts into identical validated state, or
fails closed with a precise recovery error.

## Milestone 4: Events, Sharding, and Risk - v0.16.0 to v0.20.0

### v0.16.0 - Market-Data and Lifecycle Events

- Emit bounded accepted, rejected, trade, cancel, replace, top-of-book, and
  depth events with a monotonic sequence.
- Keep serialization outside matching state mutation.
- Define event-queue backpressure; never silently drop a state transition.

### v0.17.0 - Multi-Instrument Routing

- Route instruments to independent single-writer shards.
- Make shard assignment deterministic and reject unknown instruments.
- Add cross-instrument replay and isolation tests.

### v0.18.0 - Cross-Core Queues

- Use bounded SPSC queues for commands, journal records, and event handoff.
- Expose occupancy, saturation, and backpressure off the hot core.
- Verify ownership with Loom and long-running cross-thread stress tests.

### v0.19.0 - Expanded Risk

- Add gross exposure, instrument groups, account groups, configurable credit,
  and scoped kill switches.
- Define reservation ordering across shards without a shared-book lock.
- Test overflow and conservative behavior during partial failure.

### v0.20.0 - Fault Injection

- Exercise full books, queues, and report buffers; malformed input; sequence
  gaps; journal stalls; disk errors; integer limits; and shutdown races.
- Require explicit behavior for every injected failure.
- Retain deterministic seeds for discovered defects.

Milestone exit: multi-instrument workloads remain deterministic and bounded
under resource exhaustion.

## Productization Track After v0.20.0

These are required for a serious public library and operational reference app,
but assigning exact versions now would imply false schedule certainty.

### Public Rust Facade

- Add one `hft-engine` crate that exposes intentional public types rather than
  workspace internals.
- Provide a validated builder for capacities, instruments, risk, shards, and
  optional services.
- Stabilize bounded command submission, report/event polling, snapshots,
  shutdown, and error types.
- Document thread ownership, lifetimes, backpressure, panic policy, MSRV,
  features, and semantic-versioning commitments.
- Publish examples for simulation, single-threaded embedding, and a sharded
  service.

### Operational Reference Service

- Add an optional `hftd` with validate, start, ready, drain, stop, and recover
  phases.
- Export bounded off-core metrics for queue occupancy, rejections, sequence
  state, recovery, journal lag, event lag, and latency histograms.
- Add liveness, readiness, and degraded-health endpoints.
- Add authenticated kill, drain, account disable, instrument halt, snapshot,
  and diagnostic commands; journal every state-changing admin action.
- Keep cold-path configuration, logging, formatting, and export away from
  matching cores.

### Compatibility and Distribution

- Version wire records, journal records, snapshots, and configuration schemas.
- Add upgrade readers and golden compatibility fixtures.
- Prepare crates.io metadata, rustdoc, feature combinations, MSRV CI, license
  files, checksums, SBOM, and release automation.
- Provide reproducible archives, example systemd service, non-latency container
  image, capacity guidance, and upgrade/rollback/recovery runbooks.

### Protocol and Market-Data Adapters

- Define narrow ingress and egress traits around validated normalized commands
  and events.
- Build one documented binary reference order-entry protocol with logon,
  sequencing, heartbeat, new, cancel, replace, recovery, and logout.
- Publish incremental market data plus bounded snapshots and consumer gap
  recovery.
- Treat adapters as references, not venue-certified protocols.

### Replication and Qualification

- Stream versioned journal records to a deterministic warm standby and compare
  applied sequence plus state digest continuously.
- Define lag limits, divergence handling, and manual promotion before automatic
  failover.
- Run multi-hour deterministic soak tests with churn, reconnects, recovery,
  storage stalls, queue pressure, and injected failures.
- Track memory, file descriptors, queue high-water marks, journal lag, digest,
  and performance drift.

### Native Networking Experiments

- First qualify tuned Linux UDP with `recvmmsg`/`sendmmsg`, bounded batches,
  socket tuning, affinity, prefaulting, timestamps, and NUMA placement.
- Only then add optional AF_XDP ownership for UMEM and descriptor rings.
- Keep portable fallback behavior and benchmark network adapters separately
  from matching latency.

## Definition of v1.0.0

Version 1.0 is not reached by feature count. It requires all of the following:

- A stable documented Rust facade with semantic-versioning commitments.
- Deterministic matching, risk, sessions, bounded events, journal recovery, and
  snapshots covered by explicit invariants.
- Crash recovery that reproduces the expected command sequence and state digest.
- An operational reference service with health, admin controls, secure
  defaults, packaging, upgrade/rollback, and incident runbooks.
- Reproducible performance evidence on named dedicated hardware.
- Miri, sanitizers, fuzzing, Loom, fault injection, recovery, and soak gates.
- An independent review of public APIs, unsafe boundaries, persistence formats,
  and operational failure behavior.
- An explicit list of unsupported venue, regulatory, hardware, and deployment
  requirements.

## Gates Applied to Every Release

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- Relevant doc tests, Loom models, malformed-input smoke, Miri, sanitizers, and
  fuzzing.
- Before/after release benchmarks for hot-path changes.
- Version, changelog, compatibility notes, and measured limitations.
- No new unbounded hot-path queue, hidden allocation, blocking I/O, or broad
  unsafe surface.

## Operational Readiness Checklist

- Correctness: lifecycle, quantity, priority, ownership, and risk invariants.
- Boundedness: capacities and explicit exhaustion behavior for every resource.
- Recovery: journal and snapshot restoration across injected crash points.
- Observability: live, ready, degraded, recovering, saturated, and failed states.
- Security: trust boundaries, authentication, secrets, dependencies, unsafe
  code, and administrative audit.
- Deployment: install, validate, upgrade, rollback, drain, backup, restore, and
  incident procedures.
- Performance: named boundary, hardware, OS, compiler, workload, capacities,
  affinity, counters, jitter, and allocation deltas.
- Compatibility: support windows for APIs, configuration, journal, snapshot,
  and protocol versions.

## Dependency Order

`matching invariants -> order lifecycle -> session reliability -> journal ->
recovery -> bounded events -> sharding -> public library -> operational service
-> replication -> native networking -> API stabilization`

Kernel-bypass networking is deliberately late. It is not a substitute for
correct recovery, bounded failure behavior, or a usable embedding API.

## Explicit Non-Goals Before 1.0

- Claiming venue certification or regulatory compliance.
- Promising production latency from Windows, macOS, CI, containers, or shared
  cloud runners.
- Supporting every exchange protocol or proprietary NIC SDK.
- Distributing one order book across multiple matching writers.
- Hiding overload behind heap growth, unbounded retries, or silent message loss.
- Stabilizing internal crate APIs merely because the workspace exposes them.
