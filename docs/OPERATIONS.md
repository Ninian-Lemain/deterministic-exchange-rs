# Operations

The intended Linux deployment assigns one writer to each instrument. Configure
sockets, memory, queues, CPU affinity, NUMA placement, logging, metrics, and
shutdown outside the matching loop. The router sends normalized, fixed-size
commands between cores through SPSC queues.

`hft-router::MatchingShard` and the single-instrument `hft-engine` are separate
components. `hft-engine::RoutedEngine` is a separate fixed-route facade over
journaled engines, and `SessionEngine` owns admission across those routes. See
[Engine](ENGINE.md) for the engine's admission and shutdown contract.

## Rejection and backpressure

| Condition | Result |
| --- | --- |
| Invalid wire version, type, length, or field encoding | Parse error. No command is applied. |
| Duplicate or missing command sequence | Sequence error. Expected sequence is unchanged. |
| Risk limit, kill switch, unknown account, or risk order capacity | Business rejection. Existing orders and account exposure are unchanged. |
| Report, price level, or per-price order capacity | Book rejection after preflight. A new order's risk reservation is released. |
| Unknown or unauthorized cancel | Business rejection. Book and risk state are unchanged. |
| Replace business rejection | Existing book state and reservation are preserved. |
| Full SPSC queue | Push returns the unpublished value. Higher-level callers retain input for retry. |
| Full in-memory RX queue | `QueueError::Full`. Unread frames are preserved. |
| Full event queue | Command is not admitted. Sequence and gateway state are unchanged. |
| Full journal queue in `hft-engine` | Command is not admitted. Event reservation is dropped without applying the command. |

Business rejections consume a valid command sequence. A rejected new order can
also advance order ID watermarks. Retry a backpressured frame or command
unchanged after the consumer frees capacity. Do not retry a consumed business
rejection at the same sequence.

`RiskState` errors require processing to stop. They include sequence exhaustion
and inconsistent internal risk state. `EventCapacityInvariant` and
`PublicationInvariant` indicate a broken event batch bound or producer contract.
These failures can occur after mutation and must not be retried.

## Events and journal admission

One event queue entry contains the complete result of one command. Events appear
in this order: terminal result, trades in execution order, then top-of-book if it
changed. A business rejection produces one rejection event. Parse and sequence
errors produce no events.

`hft-engine` checks the journal sequence, reserves event capacity with `admit`,
enqueues the command, then calls `apply`. An error after enqueue stops admission.
Callers composing the lower-level APIs must enforce that ordering themselves.

Events report command application. They do not acknowledge durable storage.

## Persistence status

A controlled `JournalChannel` exposes a `JournalStatusReader`. Its watermarks
identify the first sequence not covered and start at the sequence passed to
`split`.

| Status | Advances when |
| --- | --- |
| `next_written_sequence` | A complete batch has been written to the sink. |
| `next_durable_sequence` | The sink has successfully flushed all preceding writes. |
| `shutdown_complete` | The producer is closed, the queue is drained, and the final flush succeeds. |

Dequeue advances neither watermark. A partial write failure leaves the written
watermark at the previous complete batch. For a file sink, flush includes
`sync_data`. `EveryBatch` flushes each nonempty batch. `OnShutdown` defers flush
until shutdown. Repeating a successful shutdown does not flush again.

Worker failure or abandonment poisons the shared status. Taking the sink before
shutdown also poisons it. Stop admission when poison is observed. A storage
failure can still race a command already in flight. Raw SPSC journal constructors
do not provide shared persistence status.

## Recovery

`hft-recovery` restores logical gateway, risk, and book state from a versioned
snapshot, then replays a contiguous journal tail. The snapshot includes its
capacity shape, applied sequence, and SHA-256 digest. Restore rejects corruption,
truncation, unsupported formats, noncanonical encoding, invalid logical state,
and capacity mismatch. Tail replay rejects overlap, gaps, partial or corrupt
records, and differences between journal and payload sequence numbers.

Snapshot v1 records account, risk order, level, and per-level order capacities.
It does not record `REPORTS`. Replay must use the original report bound.
Configured engine restore binds that report bound and account risk definitions
through `EngineConfig` before replay. Legacy low-level recovery retains its
caller-assured original-bound requirement.

`persist_snapshot_new` requires a new destination path. It syncs a temporary file,
publishes the destination through a hard link, removes the temporary name, and
syncs the parent directory on Unix. Errors distinguish failure before publication
from failure after the destination became visible.

Generation naming, manifest replacement, retention, and selection of the latest
valid snapshot remain application responsibilities. Keep admission closed while
selecting an authoritative snapshot and tail, restoring them, and verifying the
result. Reopen only after those steps succeed.

## Operational commands and immutable bundles

`RecoveryBundle` publishes `config.v1`, `snapshot.v1`, `tail.v1` and a `COMMITTED`
marker at a new directory. The marker hashes length-framed member bytes with
SHA-256 and is published last. Files and directories are synchronized on Unix;
Windows supports file synchronization but this API makes no directory durability
promise. Publication errors may leave an incomplete directory. Never treat it
as committed or retry by overwriting it. The marker is integrity protection,
not authentication; use trusted independently supplied expected configuration.

The current cold-path CLI has fixed recovery capacities `2,8,4,4,4`. The
library APIs remain generic. A lifecycle example for those CLI capacities is:

```text
cargo run --release -p hft-cli -- checkpoint-demo target/checkpoint-1
cargo run --release -p hft-cli -- config-validate target/checkpoint-1/config.v1
cargo run --release -p hft-cli -- health target/checkpoint-1 target/checkpoint-1/config.v1
cargo run --release -p hft-cli -- backup target/checkpoint-1 target/checkpoint-backup target/checkpoint-1/config.v1
cargo run --release -p hft-cli -- restore-check target/checkpoint-backup target/checkpoint-1/config.v1
cargo run --release -p hft-cli -- compatibility-check target/checkpoint-1/config.v1 target/checkpoint-backup/config.v1
```

`checkpoint-demo` closes admission, flushes its journal, drains events and
publishes the checkpoint. It also requires a new sibling `.journal` path.
`health` reports offline recoverability and the next sequence, not live service
health. Keep the expected configuration in a separate trusted location for a
real deployment; using the generated copy above only demonstrates the workflow.

Upgrade and rollback compatibility currently require identical instrument,
capacities including reports, risk definitions and supported format versions.
Keep the previous binary and committed bundle; validate the candidate against
the independently selected configuration before switching, then restore with
fresh queue storage and verify sequence progress. A change in replay semantics
requires an explicitly designed migration. Automatic newest-bundle selection,
retention, process supervision and durable event delivery are not supplied by
these commands. Incident handling closes admission on poison, preserves evidence
and recovers an authoritative committed bundle rather than resuming failed state.

## Linux deployment checks

Component qualification scripts require a dedicated Linux host and hardware
counters. They do not require a NIC adapter. A network deployment also needs
these checks:

- Pin shard threads to cores free of unrelated interrupts. Record NIC queue and
  interrupt placement separately.
- Place UMEM, book, risk, SPSC, and TX storage on the NIC's NUMA node.
- Prefault and lock memory. Validate the hugepage policy before admission.
- Exercise RX/TX exhaustion, link reset, shutdown, and recovery.
- Record kernel, firmware, mitigations, governor, compiler flags, offered load,
  latency percentiles, queue occupancy, cache and branch misses, context switches,
  page faults, and allocation deltas.

Hosted-runner and Windows timings do not establish a Linux latency SLO.

`UdpRx` provides a portable syscall baseline. The `af-xdp` feature is an
availability marker without descriptor or UMEM ownership. `VendorSession` wraps
SDK ownership, but the vendor SDK is unavailable. AF_XDP and vendor integration
are not implemented production NIC backends.
