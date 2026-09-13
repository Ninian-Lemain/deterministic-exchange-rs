# Roadmap

The workspace version is 0.19.0. Matching, risk, sessions, journaling, recovery,
events, and instrument routing are implemented as library components.
Dedicated Linux qualification and v0.20 fault qualification remain open.
The repository is not ready for a production deployment.

## Current work

| Area | Status | Remaining work |
| --- | --- | --- |
| v0.13 Dedicated Linux qualification | Waiting on hardware | Qualified host, environment manifest, raw latency and hardware counters |
| v0.20 Fault injection and soak | In progress | Completed multi-hour runs and combined service fault coverage |
| Pre-v1 engine API | In progress | Router and session ownership, persisted configuration, operations workflows, independent review |

The [engine facade](ENGINE.md) now joins admission, bounded events, and journal
status for one instrument. Its tests cover both queue pressure paths, worker
failure, shutdown, snapshot restart, and separate workers. The router still
uses its own `MatchingShard`, without this facade or a journal.

## Implemented milestones

These entries describe completed implementation work, not a production
qualification. Historical timings belong in [performance evidence](PERFORMANCE.md).

| Version | Change | Behavior and coverage |
| --- | --- | --- |
| v0.1.0 | Deterministic vertical slice | Borrowed parsing, fixed-capacity risk and book, owner cancel, SPSC handoff, and replay digest |
| v0.2.0 | Indexed order lookup | Fixed order ID index with back-shift deletion and collision, relocation, and slot-reuse tests |
| v0.3.0 | Stable-slot FIFO | Per-level linked slots replace order shifting. Tests cover priority, live/free separation, and atomic full-level refusal |
| v0.4.0 | Indexed risk state | Account and reservation indexes with bounded load, stable slots, and exposure reconciliation |
| v0.5.0 | Price-level discovery | Sorted active-level directory and free-slot pool. Best price is O(1), lookup O(log n), and insertion/removal shifts O(n) index entries |
| v0.6.0 | Matching transaction plan | One preflight walk checks liquidity and capacity. A bounded plan applies fills and the resting remainder without a book undo log |
| v0.7.0 | Reference models | Seeded book and gateway comparisons cover quantity, priority, ownership, reservations, and replay |
| v0.8.0 | Benchmark suite | Machine-readable component and gateway measurements with percentiles, checksums, and allocation checks |
| v0.9.0 | IOC and FOK | IOC discards its unfilled remainder. FOK fills completely or rejects before book mutation |
| v0.10.0 | Post-only | Crossing orders reject. Non-crossing orders join the normal FIFO tail |
| v0.11.0 | Replace | A strict same-price reduction keeps priority. Other accepted replaces lose priority. Crossing replaces reject and restore the prior reservation |
| v0.12.0 | SPSC Loom and unsafe audit | Loom runs the shipped queue algorithm. Tests cover publication, wrap, and returned full-queue payloads |
| v0.14.0 | Session state machine | Caller-supplied virtual time drives connection, logon, active, recovery, logout, and failure states |
| v0.15.0 | Session recovery | Bounded retransmission retains unconfirmed frames. Confirmation removes a prefix and recovery replays the retained suffix |
| v0.16.0 | Command journal | Versioned 64-byte CRC32C records, bounded enqueue, batched persistence, short-write handling, flush policies, and corrupt-tail rejection |
| v0.17.0 | State recovery | Canonical SHA-256 snapshots, rebuilt indexes and free lists, and contiguous journal-tail replay |
| v0.18.0 | Bounded events | One fixed batch per processed command, sequence/ordinal IDs, and capacity checks before gateway mutation |
| v0.19.0 | Instrument routing | Fixed instrument-to-shard mapping, independent books and risk state, bounded command/event queues, and explicit unknown-route refusal |

The implementations have limits that remain part of their contracts:

- Identity lookup is expected O(1) at bounded load. Collisions still require
  probing.
- Snapshot v1 stores the gateway capacity shape, but not the report bound.
  Tail replay must use the original `REPORTS` value.
- Event publication acknowledges application, not durable persistence.
- Each instrument has its own sequence and event order. There is no merged
  cross-shard event order or dynamic shard reassignment.
- Session state and retransmission are library mechanisms, not a certified
  external venue protocol.

## v0.13 Dedicated Linux qualification

A qualifying run needs a dedicated Linux host with controlled CPU placement,
frequency policy, SMT, IRQs, and NUMA placement. Publish the environment
manifest, raw samples, binary identity, and hardware counters with the result.

The tooling in `scripts/linux` captures the environment, checks host settings,
pins execution, collects counters, and hashes output. It rejects container
runs as qualification evidence. Passing its checks is a prerequisite, not a
substitute for reviewing the host and workload.

Windows and Docker Desktop can validate code and tooling. They do not meet
this hardware requirement. Later implementation milestones do not depend on
v0.13, but v1 requires its evidence.

## v0.20 Fault injection and soak

Requires v0.11 and v0.14 through v0.19.

The [soak runner](SOAK.md) covers routed churn, recurring queue pressure,
session faults, exhaustion, and repeated snapshot recovery. Each seed runs
twice and must produce matching state, events, and counters. Journal faults
currently use a separate in-memory fixture.

Completion requires retained seeds and completed multi-hour runs covering:

- Churn, gaps, reconnects, malformed input, and capacity exhaustion.
- Command and event pressure, journal stalls, and routing imbalance.
- Snapshots and recovery during the declared workload.
- Shutdown races across the combined service boundaries.
- Recorded memory use with no unexplained growth or state divergence.

A profile name or large step count is not proof of elapsed hours. An interrupted
run without a successful result is not a pass. No completed multi-hour
qualification is currently recorded.

## Pre-v1 stabilization

The initial facade, builder, lifecycle example, MSRV checks, and
[desktop overhead comparison](PERFORMANCE.md#engine-boundary) are present.
Session benchmark fixtures also need repair before their timings or allocation
fields can be used as evidence. The current gaps are listed in
[Performance](PERFORMANCE.md#session-and-recovery-window).

The following work remains:

1. Finish the public engine boundary for routing and session admission.
   Public mutation must not bypass sequence, risk, or capacity checks.
2. Persist and validate configuration needed for replay, including the report
   bound. Define API and format compatibility rules.
3. Add the operational harness for configuration validation, health, drain,
   shutdown, backup/restore, upgrade, and rollback. Keep storage and process
   control outside matching.
4. Measure the completed boundary on dedicated Linux. Resolve unexplained
   regressions and verify allocation behavior on each declared hot path.
5. Obtain independent API, unsafe-boundary, recovery-format, and operations
   review before a v1 release candidate.

## v1 entry criteria

- Stable library API for matching, risk, sessions, events, and recovery.
- Zero measured allocation after initialization on declared hot paths.
- Explicit overload behavior for every fixed capacity and queue.
- Dedicated Linux evidence for the measured boundary, without a network claim
  where no network path was measured.
- Relevant Miri, actual-algorithm Loom, sanitizer, property/fuzz, crash, and
  soak checks.
- Install, validate, drain, shutdown, backup/restore, upgrade/rollback, health,
  and incident procedures for any reference service.
- Documented unsupported venue, regulatory, hardware, and deployment requirements.

## Later work

Linux UDP batching is experimental and follows dedicated qualification.
A real vendor SDK may use a small C ABI shim with ownership and ABI tests.

AF_XDP, DPDK, huge pages, hardware timestamps, kernel bypass, replication,
standby promotion, and venue adapters require separate designs and evidence.
Multi-chain execution is not implemented.

The [safety policy](SAFETY.md) applies throughout. Business logic stays in safe
Rust. Native code is reserved for an actual external boundary, not a way to
move unsafe code out of the Rust inventory.
