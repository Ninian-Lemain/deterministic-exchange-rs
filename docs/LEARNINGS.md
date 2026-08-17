# What I Learned

`deterministic-exchange-rs` is a learning project: a high-speed backend
server for an electronic financial exchange, built as a deterministic,
allocation-free matching engine and execution gateway in Rust. I built it to
turn exchange-infrastructure concepts into code with testable ownership,
capacity, determinism, and failure invariants. These are the lessons that
actually cost me something.

## Zero Allocation Is a Layout Decision

The hot path does not stay allocation-free because I was careful while
writing it. It stays allocation-free because every structure it touches
(accounts, reservations, orders, price levels, reports, queue slots) is a
fixed-capacity flat array sized at compile time and allocated once at init.
After that there is no `Vec::push` left to slip in. The release benchmark
runs under a counting allocator and exits nonzero if the measured path
allocates or deallocates after warm-up, so a regression fails the process
instead of showing up as a slower percentile.

## Lifetimes Can Replace Runtime Coordination

An RX frame cannot be recycled while the parser still borrows it. Modeling
that with `FrameLease` and lifetimes makes use-after-recycle a compile
error and deletes a class of runtime bookkeeping. The borrow checker ended
up doing coordination work I would otherwise have done with reference counts
or protocol comments.

## Zero-Copy Claims Need Exact Boundaries

The parser reads straight out of the borrowed frame. The cross-core handoff
is one fixed-size copy into a preallocated SPSC slot. Calling the pipeline
zero-copy would be wrong; writing down exactly where the single bounded copy
happens made the design reviewable and killed the marketing adjective.

## Bounded State Turns Overload into a Specification

Fixed capacities remove allocator jitter, and they also force an explicit
answer to "what happens when this is full." Every full condition (book,
risk table, report buffer, queue) rejects or returns backpressure without
touching live state. Overload behavior became something I could write tests
against instead of leaving it unspecified.

## Preflight Everything, Then Apply Infallibly

The book originally paired its match preflight with a fixed-capacity undo
log so a late failure could roll back. Auditing the failure set showed the
log was dead code: validation, duplicates, report capacity, and level
capacity are all decidable before the first mutation, and nothing runs
between plan and apply. Deleting the rollback removed a class of
restore-the-world bugs (the undo path itself had silent-failure spots) and
left one rule: preflight is complete, application is infallible. The
gateway still releases a taker reservation when the book rejects, because
that rejection crosses a crate boundary.

## A Sorted Index Needs Its Own Slot Pool

Adding a sorted-level index made best-price discovery O(1), but the insert
path still scanned the level array linearly to find a price or a free slot,
so insertion stayed O(levels). The fix was to make the index own the whole
lifecycle: binary search for prices, and a free-slot pool so allocation and
removal never scan. A data structure that speeds up reads while writes keep
scanning is half an index.

## Cache Locality Is Decided by Layout

The measured latencies are flat because of where bytes sit:

- SPSC head and tail positions are padded onto separate cache lines, so the
  producer and consumer never false-share a line.
- Orders live in flat arrays behind stable slot handles; a fill or cancel
  rewrites two links and moves nothing else, so no peer order ever gets
  copied across a cache line.
- Best-price goes through the sorted index instead of a scan, so the common
  read touches one cache-resident structure.

None of the algorithms are exotic. The wins came from deciding the layout
first and letting the algorithms stay ordinary.

## Identity Lookup Should Not Scale with Occupancy

Cancellation starts with an `OrderId`, so walking live orders makes cancel
latency depend on unrelated book depth. A fixed open-addressed index makes
the lookup expected O(1) while keeping deterministic capacity and zero
hot-path allocation. The cost is explicit memory: 64 KiB for the benchmark
book shape, traded for probe chains that stay short at bounded load.

## Fail Closed or Don't Check at All

Several index helpers returned `Option` for states that were impossible by
construction, and two removal paths returned an error *after* mutating,
which would have corrupted the book if they had ever fired. Unreachable
branches that corrupt on the way out are worse than assertions: they look
like error handling and protect nothing. The rule now: internal invariants
get `debug_assert`/`expect` with the invariant named, stale external handles
fail closed before any mutation, and there is no third category.

## A Deterministic Event Loop Is Mostly About What You Exclude

Most of determinism came from removing things. One writer per book, no
locks, no wall-clock reads, no thread-timing dependence in any state
transition, no hash maps with random iteration order, seeded generators in
the generated-command tests. The one thing I had to add was canonical encoding:
risk state is split into big-endian lanes before hashing, so identical
logical state produces one digest on any architecture and the golden replay
digest stays stable across platforms.

## Release/Acquire Is an Ownership Proof

The SPSC queue publishes a slot with Release and observes it with Acquire;
the same pairing in reverse protects slot reclamation. Thread-private cached
positions need no atomics. A Loom model checks the publication invariant, so
the correctness argument does not rest on my code review alone.

## Risk and Matching State Form One Lifecycle

Reservations cover the worst case for open buys and sells independently.
Partial fills convert only the executed quantity into signed position;
cancels release exactly the remainder. Getting this wrong does not show up
as a crash; it shows up as slow account drift, so the tests exercise the
maker and taker paths symmetrically and check the accounting after every
transition.

## Benchmarks Must Measure Live Work

The risk occupancy harness populated reservations, ran a fill loop that
closed them, then "measured" cancel and settle against the same IDs. Those
cells measured `UnknownOrder` rejections, not live work, and the smallest
occupancy underflowed the ID arithmetic entirely. Destructive operations now
run against freshly populated engines, one live reservation per sample, and
every timed operation carries an assertion that it did the intended work. A
benchmark that compiles and prints plausible numbers can still measure the
wrong path.

## Steady State and Percentiles Beat Raw Means

Two more harness defects hid behind plausible output. The gateway loop
sampled its first, coldest iterations, so frequency ramp-up and first-touch
stack pages landed in the reported distribution. The crossing scenarios were
not in steady state: `submit_cross` depleted its makers within a few dozen
samples and then measured rejections, while `level_create` filled the book
and then measured capacity errors. The fixes were mechanical: warm up before
sampling, pair every timed operation with an untimed teardown that restores
the fixture, report sorted percentiles. On a shared desktop the max is still
a scheduler-preemption artifact; it stays visible instead of being trimmed,
and the percentiles carry the shape of the data.

## Benchmarks Separate Evidence from Claims

The allocation harness proves the measured steady-state path has zero
allocation deltas. Desktop timing is retained as a smoke result only. Real
latency qualification needs pinned Linux cores, controlled frequency, NUMA
placement, a real NIC path, hardware counters, and reproducible load. A
Windows desktop provides none of those, and the README says so.

## Unsafe Marks the Integration Boundaries

Unsafe is confined to three sites: initialized SPSC slot access, the
opaque-handle C ABI wrapper, and the counting allocator. Everything else
(domain logic, parsing, risk, matching, gateway, replay) is
`#![forbid(unsafe_code)]`. Keeping the list short is what makes Miri, the
CI allowlist, and the FFI sanitizers cheap to run.

## Scope Honesty Improves the Project

The slice demonstrates deterministic execution mechanics. It does not
include venue certification, durable recovery, AF_XDP UMEM ownership, or
proprietary vendor integration. Naming those gaps as roadmap items, with
their own operational and hardware requirements, keeps the implemented part
verifiable.
