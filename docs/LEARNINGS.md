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

## Preflight Protects Transactionality

The book calculates report and resting capacity before matching. Without this
preflight, a capacity error could occur after makers were already mutated. The
gateway also releases a taker reservation when the book rejects it.

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
