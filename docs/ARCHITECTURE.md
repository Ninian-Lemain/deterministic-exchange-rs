# Architecture

The repository is a set of Rust libraries with two command-processing entry
points. Both use the same gateway, risk engine, book, and event types. They
differ in routing and journal ownership.

[![Execution overview](diagrams/system-overview.svg)](diagrams/system-overview.mmd)

## Entry points

### Routed commands

`MultiInstrumentRouter` parses a borrowed frame or accepts an existing
`Command`. Its fixed route table maps exactly one instrument to each shard.
Contiguous instrument IDs use a checked offset. Sparse IDs use binary search.

The router publishes into that shard's bounded command SPSC. `MatchingShard`
takes the next command, checks its instrument and sequence, and reserves event
capacity before gateway mutation. Event pressure keeps one command pending for
the next call. The shard retries it before dequeuing another command.

The shard publishes a complete event batch through its event SPSC. The caller
retrieves a batch with `try_event(shard)`. Order is per instrument. The router
does not merge events across shards or journal commands.

### Journaled engine

`hft-engine::Engine` accepts frames or commands for one configured instrument.
It owns the gateway mutation path, event producer, and journal writer.

Admission checks lifecycle, observed persistence status, instrument, sequence
alignment, and event capacity. It then enqueues a journal record before
applying the command. A full event or journal queue leaves the command
unconsumed.

`PersistenceWorker` validates, batches, writes, and flushes journal records
outside the matching call. Written and durable progress are separate.
An event reports application, not durability. Observed persistence failure
closes engine admission, although failure can race an in-flight command.

The engine does not own the router or session state machine. Those connections
remain integration work. See [ENGINE.md](ENGINE.md) for construction, failure,
and shutdown contracts.

## Ownership

| Owner | State or resource |
| --- | --- |
| Receive adapter | RX bytes. A `FrameLease` keeps its receive buffer borrowed until drop |
| Router | Fixed routes, one command producer and event consumer per shard |
| Matching shard | Its command consumer, pending command, and bounded event engine |
| Event engine | Gateway and one event producer |
| Gateway | One instrument's risk state, book, command sequence, and order ID watermark |
| Engine facade | Event engine, journal writer, and persistence status reader |
| Engine storage | Fixed event and journal queues for one engine lifetime |
| Persistence worker | Journal consumer, batch buffer, sink, and flush progress |

Queue storage must outlive its borrowed endpoints. `SpscQueue::split(&mut self)`
creates one producer and one consumer. The caller assigns threads. Neither the
router nor engine builder starts worker threads.

Matching has one writer per book. Shards do not share mutable book or risk
state. This avoids locks in the matching call, but it also means cross-shard
account limits are not a shared risk service.

## Book and risk layout

Orders occupy stable FIFO slots within fixed price-level arrays. A linked
live list preserves price-time priority. A separate free list tracks reusable
slots. Cancelling or filling an order does not move its peers.

Each side has a contiguous sorted directory of active prices and their level
slots. Best-price discovery is O(1). Price lookup is O(log n). Creating or
removing a price shifts O(n) directory entries, not order records.

Each level maintains a `u128` aggregate quantity. Inserts, partial fills,
cancels, and replaces update that total. Top-of-book reads do not scan a level's
orders.

Fixed open-addressed indexes locate orders, accounts, and reservations.
Their load stays at or below one half. Lookup is expected O(1), with probe
length affected by collisions. Back-shift deletion repairs moved references.
[LAYOUT.md](LAYOUT.md) records storage costs and measured tradeoffs.

## Gateway application

The gateway consumes an exact next sequence before business checks. New order
IDs must exceed the received-ID watermark, which advances before risk and
book validation.

For a new order, risk checks reserve exposure before the book builds a bounded
`MatchPlan`. Preflight checks policy, liquidity, and capacity without changing
the book. Applying an accepted plan writes fills and any resting remainder.
The gateway accounts for maker and taker fills and releases unused exposure.

Book rejection releases the new order's reservation. Cancel removes an owned
resting remainder and releases its reservation. Replace adjusts risk first
and restores the prior reservation if book checks reject it. A strict
same-price quantity reduction keeps priority. Other accepted replaces rejoin
the destination FIFO tail.

Business rejections consume sequence and may advance order ID watermarks.
They do not mean the entire gateway remained unchanged. Internal risk-state
errors are fatal to the processing contract.

## Events and overload

One event SPSC slot holds a whole command result. A successful command emits
its result, trades in execution order, and top-of-book only when it changed.
A business rejection emits one `Rejected` event.

Event IDs contain the command sequence and batch ordinal. Consumers must retain
instrument context because sequence domains are independent. Parse and sequence
errors emit no event.

Queue pressure is distinct from business rejection. It refuses admission
before gateway mutation. A routed caller retains its refused input. A shard
retains its command when events are blocked. A journaled-engine caller retries
the refused command unchanged.

Risk, report, order, and level capacities have their own rejection paths.
See [OPERATIONS.md](OPERATIONS.md) for failure handling and
[PROTOCOL.md](PROTOCOL.md) for command semantics.

## Recovery and external work

Snapshots encode canonical logical state, not raw arenas. Restore rebuilds
indexes, free lists, and quantity totals before replaying a contiguous journal
tail. The original report bound remains required because snapshot v1 does not
record it.

Storage, snapshot encoding, configuration, sockets, logging, metrics export,
affinity, and process shutdown coordination stay outside matching. Session
deadlines use caller-supplied time. Portable UDP is a syscall baseline.
AF_XDP and proprietary venue adapters are not implemented.
