# Engineering Lessons Learned

This project was built to turn low-latency systems concepts into code with
testable ownership, capacity, and failure invariants. These are the main
engineering lessons demonstrated by the implementation.

## Ownership Is Part of the Latency Design

An RX frame cannot be recycled while parsing code still borrows it. Modeling
that rule with `FrameLease` and lifetimes removes a runtime coordination problem
and prevents use-after-recycle at compile time.

## Zero-Copy Claims Need Exact Boundaries

The parser reads directly from a borrowed frame. A cross-core handoff uses one
fixed-size copy into a preallocated SPSC slot. Calling the whole pipeline
zero-copy would be misleading; documenting the copy boundary makes the design
reviewable.

## Bounded State Turns Overload into a Specification

Fixed account, order, price-level, report, and queue capacities avoid allocator
jitter. They also require explicit behavior for exhaustion. Every full
condition therefore rejects or returns backpressure without overwriting live
state.

## Complete Preflight Beats Rollback

The book originally paired its preflight with a fixed-capacity undo log so a
late failure could roll a match back. Auditing the failure set showed the log
was dead code: validation, duplicates, report capacity, and level capacity are
all decidable before the first mutation, and nothing else runs between the
plan and the apply. Deleting the rollback removed an entire class of
restore-the-world bugs (the undo path itself contained silent-failure spots)
and replaced it with one reviewable rule: preflight is complete, application
is infallible. The gateway still releases a taker reservation when the book
rejects it, because that rejection crosses a crate boundary.

## Sorted Indices Need Their Own Slot Pool

Adding a sorted-level index made best-price discovery O(1), but the resting
path still scanned the level array linearly to find a price or a free slot,
which quietly kept order insertion O(levels). The fix was to make the index
own the whole lifecycle: binary search for prices, and a free-slot pool so
allocation and removal never scan. A data structure that only speeds up reads
while writes keep scanning is half an index.

## Defensive Code Must Fail Closed or Not Exist

Several index helpers returned `Option` for conditions that were impossible by
construction, and two removal paths returned an error *after* mutating state,
which would have corrupted the book had they ever fired. Unreachable branches
that corrupt on the way out are worse than assertions: they look like error
handling but protect nothing. The rule now is that internal invariants get
`debug_assert`/`expect` with the invariant named, stale external handles fail
closed before any mutation, and there is no third category.

## Benchmarks Must Measure Live Work

The risk occupancy harness populated reservations, ran a fill loop that
closed them, and then "measured" cancel and settle against the same IDs. Those
cells measured `UnknownOrder` rejections, not live work, and the smallest
occupancy underflowed the ID arithmetic entirely. Destructive operations now
run against freshly populated engines, one live reservation per sample. A
benchmark that compiles and prints plausible numbers can still be measuring
the wrong path; every measured operation needs an assertion that it did the
intended work.

## Identity Lookup Should Not Scale with Book Occupancy

Cancellation starts with an `OrderId`, so walking every live order makes its
latency depend on unrelated book depth. A fixed open-addressed index makes that
lookup expected O(1) while preserving deterministic capacity and zero hot-path
allocation. The measured tradeoff is explicit memory: 64 KiB for the benchmark
book shape. FIFO compaction remains a separate optimization.

## Risk and Matching State Form One Lifecycle

Reservations cover the worst case for open buys and sells independently.
Partial fills convert only the executed quantity into signed position; cancels
release exactly the remainder. Tests check this across maker and taker paths.

## Atomic Ordering Is an Ownership Proof

The SPSC queue uses Release to publish initialized slots and Acquire to observe
them. The same pairing protects slot reclamation in the other direction.
Thread-private cached positions need no atomics. A Loom model checks the core
publication invariant.

## Determinism Requires Canonical Encoding

Stable replay is more than processing inputs in order. Digest inputs must also
use architecture-independent byte order. Risk state is split into canonical
big-endian lanes before hashing so identical logical state has one digest.

## Benchmarks Must Separate Evidence from Claims

The allocation harness proves that the measured steady-state path has zero
allocation deltas. Desktop timing is retained as a smoke result only. Credible
latency qualification still requires pinned Linux cores, controlled frequency,
NUMA placement, a real NIC path, hardware counters, and reproducible load.

## Unsafe Code Should Mark Integration Boundaries

Unsafe Rust is confined to initialized SPSC slot access, a narrow opaque-handle
C ABI wrapper, and the isolated counting allocator. Domain logic, parsing,
risk, matching, gateway coordination, and replay forbid unsafe code.

## Scope Honesty Improves the Project

The portable vertical slice demonstrates deterministic execution mechanics. It
does not pretend to include venue certification, durable recovery, AF_XDP UMEM
ownership, or proprietary vendor integration. Those are named roadmap items
with separate operational and hardware requirements.
