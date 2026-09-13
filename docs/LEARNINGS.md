# Engineering lessons

The project's useful lessons came from workload failures, release-only bugs,
and changes in data layout. These notes explain the decisions behind the
current implementation.

## Capacity is part of admission

Fixed arrays remove growth from matching, but every full condition needs a
defined response. Command, event, and journal queues return backpressure.
Risk and book capacities produce business rejections.

Those outcomes are different. Backpressure before admission consumes no
sequence. A sequence-valid business rejection consumes the sequence and can
advance order ID watermarks. Resting orders and exposure must still remain
consistent.

## Stable slots and compact indexes solve different costs

Removing an order from a dense FIFO used to move its peers. Intrusive links over
stable slots reduce cancellation to index lookup and link updates. They cost
metadata, so shallow and deep workloads both need measurement.

The price directory is a separate contiguous array of level indexes. Binary
search finds a price, while insertion and removal shift index entries.
Orders do not move with the directory. Best-price lookup reads its first entry.

Compact private handles reduced index storage without reducing the public ID
range. Each level now maintains its quantity total, so top-of-book reads do
not walk the FIFO. Update work and memory cost still belong in the benchmark.
The [layout measurements](LAYOUT.md) include both.

## Preflight removes expected failure from book application

The matcher builds a bounded plan before changing orders. It checks order
validity, liquidity, report capacity, and resting capacity against the unchanged
book. Applying an accepted plan needs no book undo log.

This does not remove every rollback in the gateway. Risk exposure is reserved
before book submission. A rejected new order must release that reservation.
A rejected replace must restore its prior reservation. Book and gateway
atomicity are different contracts.

## Parsing and handoff have different copy boundaries

Parsing borrows an RX frame. Routing converts the parsed fields to a fixed-size
`Command` and copies it into a preallocated queue slot. This is not an
end-to-end zero-copy pipeline.

The journaled engine parses its frame once and enqueues the original wire
bytes. Its normalized-command entry point encodes once. Storage writes belong
to the persistence worker, not the matching call.

## A plausible benchmark can measure the wrong branch

An early risk fixture closed reservations and then timed cancel and settle
against those closed IDs. It measured `UnknownOrder`, not successful cleanup.
A crossing fixture consumed its makers and then timed rejection. Another
replace fixture used a live order while claiming to measure unknown-order
rejection.

A workload needs assertions on the operation it names. Destructive operations
also need untimed repair or a declared changing workload. Percentiles and
checksums cannot compensate for a fixture that measures the wrong branch.

Allocation counters must bracket the work being claimed. Reading both counters
after sampling cannot prove that sampling allocated nothing. Known coverage
limits remain listed in [performance evidence](PERFORMANCE.md).

## Assertions must not contain required mutation

Two index deletion paths called relocation closures inside `debug_assert!`.
Debug builds updated the moved handles. Release builds omitted the calls and
left stale handles behind.

Required state changes now run unconditionally. Assertions check their results.
Release execution and model comparisons belong in validation, not just timing.

## SPSC ordering protects two transfers

The producer writes a slot, then publishes it with Release. The consumer
observes publication with Acquire before reading. The consumer's release of
that slot must also become visible before the producer reuses it.

Loom exercises the shipped algorithm's atomics and cells. Miri checks the
memory accesses on supported paths. Neither replaces the ownership argument
for one producer, one consumer, and correctly paired initialization and drop.

## Publication is not durability

A journal queue preserves order inside one process. It does not prove that a
record reached disk. Written progress advances after a complete batch write.
Durable progress advances after a successful flush.

The engine reserves event capacity, enqueues the command, and only then applies
it. A full journal queue leaves the command unconsumed. Observed persistence
failure stops admission, although a command already in flight can race it.

Events describe applied state. A crash between application and flush can lose
an applied command from durable history. Exactly-once effects across crashes
need acknowledgment, retry, and delivery rules beyond an SPSC queue and a
sequence number.

## One queue slot holds one command result

Publishing a command's events separately could expose an acknowledgment but
lose later trades when the ring fills. The event queue stores a complete
fixed-capacity batch and publishes it with one tail update.

The command sequence and batch ordinal form the event ID. No extra event
counter is needed. Business rejections produce one rejected event. Successful
commands produce their result, trades in execution order, and top-of-book
only when it changed.

## Snapshots preserve logical state, not arena history

Snapshots store accounts, reservations, levels, and FIFO orders in canonical
order. They do not serialize raw arenas, private handles, or free-list history.
Restore validates logical records and rebuilds those structures.

The applied sequence defines where tail replay starts. Recovery rejects gaps,
overlap, partial records, corruption, and payload sequence mismatch. Snapshot
v1 does not record the report bound, so replay still needs the original
configuration. Restore does not provide durable event redelivery.

## Measurement claims need a named environment

Desktop runs expose large regressions, allocation changes, and workload errors.
They also contain timer quantization, scheduler interruptions, and frequency
changes. Small differences are not evidence of a production speedup.

Dedicated Linux qualification requires the workload, build, CPU placement,
frequency policy, IRQ and NUMA settings, counters, and raw output together.
Docker can reproduce tools. It cannot make shared hardware dedicated.
