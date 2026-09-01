# What I Learned

I built this project to learn how exchange mechanics change when capacity,
ordering, and failure behavior are part of the API. Most useful lessons came
from code that looked reasonable until a test or benchmark proved otherwise.

## Fixed Capacity Defines Overload

Fixed arrays removed allocator activity from the measured path. They also made
every full condition observable. A full book, risk table, report buffer, or
queue must reject the operation or return backpressure without changing live
state. Capacity is part of the protocol, not an implementation detail.

The release benchmark checks allocation and deallocation counters after
warmup. That catches a regression directly. A latency change alone would be a
weaker signal.

## Stable Handles Stop FIFO Movement

Dense price levels made cancellation cost depend on queue depth because
removing an order shifted every order behind it. Stable slots with intrusive
links changed removal into a few link updates. Peer orders stay in place and
keep their identity.

The tradeoff is more metadata per slot and a slower depth-one case. The memory
cost and the shallow regression belong beside the latency result.

## Indexes Trade Memory for Predictable Lookup

Cancellation starts with an order ID. Scanning the book made its cost depend
on unrelated occupancy. Fixed open-addressed indexes removed that scan for
orders, accounts, and reservations.

The first price index only sorted active levels. Insertion still scanned the
level array for a free slot. Giving the index its own free-slot pool removed
that second scan. An index has to own lookup and slot lifecycle to remove the
occupancy-dependent path.

## Preflight Removed Rollback

The matcher once carried an undo log for failures during plan application.
Reviewing the failure set showed that duplicate checks, report capacity, price
level capacity, and match quantity could all be decided before mutation.

After complete preflight, applying a plan has no expected failure path. Removing
the undo log also removed recovery code that could fail while trying to repair
state.

## Zero Allocation Starts With Layout

The hot path does not avoid allocation through local coding discipline alone.
Accounts, reservations, orders, price levels, reports, retransmit frames, and
queue slots all have fixed storage created before processing starts.

The same precision matters for zero-copy claims. Parsing borrows an RX frame.
Cross-core handoff copies a bounded value into a preallocated SPSC slot. The
pipeline is not zero-copy.

## Benchmarks Can Measure the Wrong Work

An early risk benchmark filled reservations, closed them, then timed cancel and
settle against the closed IDs. The output looked plausible but measured
`UnknownOrder` rejection. Another crossing fixture consumed its makers and
spent most samples timing capacity errors.

Timed operations now assert the expected outcome. Destructive workloads use an
untimed repair step so each sample starts from the same state. Warmup happens
before sampling, and the report includes p50, p90, p99, p99.9, and max. A mean
cannot show that the workload drifted onto another branch.

## Release Builds Exposed a Side Effect Bug

Two open-addressed index removal paths called update closures inside
`debug_assert!`. Debug builds updated moved handles. Release builds removed the
calls, left stale handles behind, and eventually filled the tables.

Assertions now inspect results. Required mutation happens outside them. Tests
that only run debug builds would not have found this defect.

## SPSC Ordering Needs a Proof

The queue publishes initialized slot contents with Release and observes them
with Acquire. The reverse handoff protects slot reuse. Cached positions remain
thread-local.

Loom explores the publication and wraparound interleavings in the shipped
algorithm. Miri catches the plain-memory race when publication is weakened.
The memory ordering argument is part of the queue design, not a comment added
afterward.

## Desktop Timing Is Smoke Evidence

Windows runs catch large regressions, workload mistakes, allocation changes,
and digest changes. They do not establish production latency. Scheduler
preemption, timer resolution, frequency changes, and background work are all
visible in the samples.

A qualified result needs a named Linux host, pinned cores, controlled frequency,
IRQ and NUMA placement, hardware counters, and the full environment recorded
beside the raw output. Docker can pin tools and dependencies. It cannot make a
shared host dedicated.

## Journaling Makes Backpressure Part of Admission

A bounded journal queue can refuse a command when persistence falls behind.
That refusal must happen before accepted matching state becomes visible. If the
engine mutates first and discovers a full queue afterward, the journal is no
longer a record of accepted history.

The journal now has a storage writer, batch and shutdown flush policies, writer
failure poisoning, and restart scanning over persisted bytes. The remaining
engine contract is how persistence failure reaches admission before the queue
fills. That belongs in the caller API, not inside the file writer.

## Exactly Once Is a Recovery Rule

An SPSC queue can preserve order within one process. It cannot prove that a
record reached durable storage exactly once. A crash can happen after enqueue,
after write, during a partial write, or after flush but before acknowledgement.

Exactly once requires a versioned disk format, sequence validation, a stated
flush point, and recovery that either returns one ordered prefix or rejects the
file. The crash fixtures now scan actual encoded bytes. Partial tails,
duplicates, gaps, and corrupt records stop recovery.

## Unsafe Code Stays at Narrow Boundaries

Unsafe code is limited to SPSC slot access, the opaque-handle C ABI, and the
benchmark counting allocator. Domain logic, parsing, risk, matching, gateway,
session, replay, and journal code forbid unsafe Rust.

Keeping these boundaries small makes the invariants testable. It also keeps
Miri, sanitizer runs, and the unsafe allowlist focused on code that needs them.

## Snapshots Store Logical State

Copying fixed-capacity arenas would preserve slot placement, tombstones, and
free-list history. Those details are not part of exchange state. The snapshot
stores accounts, reservations, price levels, and FIFO orders in canonical
order. Restore validates those records and rebuilds indexes and free lists.
Two engines with the same logical state therefore produce the same bytes even
when their allocation histories differ.

The snapshot sequence is the boundary between state and the journal tail.
Recovery accepts only the next journal sequence and checks that each record
matches the sequence inside its wire payload. Rejected business commands still
consume a valid sequence. Corrupt framing and sequence errors stop recovery.

## Publish One Command Per Queue Slot

Publishing trades and acknowledgements as separate queue entries permits a full
ring to expose only part of a command. Rolling back matching state would not
remove events already observed by the consumer. The event ring therefore stores
one fixed-capacity batch per command. Capacity is checked before mutation and
the producer publishes the batch with one tail update.

Event IDs do not use a separate counter. The protocol sequence identifies the
command and an ordinal identifies its events. Snapshot restore can reproduce
the same IDs without extending the v1 snapshot format.
