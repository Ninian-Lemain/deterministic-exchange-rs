# deterministic-exchange-rs

[![CI](https://github.com/Ninian-Lemain/deterministic-exchange-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Ninian-Lemain/deterministic-exchange-rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)

A deterministic, allocation-free Rust matching engine and execution gateway —
the high-speed backend server core of an electronic financial exchange, built
for learning and experimentation. The pipeline is an RX frame lease ->
lifetime-bound binary parsing -> session sequencing -> indexed pre-trade risk
-> price-time matching/cancel -> execution reports -> replay digest.

Current version: **v0.6.2**. The project is actively in development. It exists
to turn exchange-infrastructure concepts — deterministic state transitions,
conservative pre-trade risk, bounded resources, explicit backpressure,
price-time priority, session sequencing, replayability, cache-aware cross-core
handoff, and narrow driver boundaries — into code with testable ownership,
capacity, and failure invariants. It is **not** production ready; the
[roadmap](docs/ROADMAP.md) tracks the remaining correctness, verification, and
operational milestones toward a v1. Nothing here replaces venue certification,
proprietary feed handlers, or measured kernel-bypass deployment work.

## Why This Project

- Honest data movement: parsing borrows the RX frame; cross-core normalized
  orders are a bounded single-copy handoff.
- Deterministic failure: fixed capacities reject explicitly and never silently
  overwrite state.
- Single-writer books: an instrument shard owns its risk state and order book.
- Hot-path discipline: integer prices and quantities, no locks, logging,
  syscalls, formatting, or allocation in the measured packet-to-report path.
- Contained unsafe: only SPSC slot access, the FFI wrapper, and the benchmark
  allocator use unsafe Rust.
- Lifecycle integrity: strict inbound sequencing, owner-authorized cancel,
  partial-fill reservation accounting, and fail-closed capacity behavior.

## Build, Test, Benchmark

Requirements: Rust 1.85 or newer (see `rust-toolchain.toml`), with `rustfmt`
and Clippy. The workspace builds with stable Rust and no required third-party
dependencies for the engine itself.

```console
cargo build --workspace --release
cargo test --workspace --all-features
cargo run --release -p hft-cli -- replay-demo
cargo run --release -p hft-bench
```

The benchmark executable exits nonzero if the measured hot path allocates or
deallocates after warm-up. See [QUICKSTART.md](QUICKSTART.md) for all quality
gates (fmt, Clippy with warnings denied, Loom, fuzz smoke, source ratio).

Supported platforms: the engine is portable safe Rust and is tested on
Windows and Linux x86_64. Linux is the reference platform for latency
qualification; Windows/macOS timing results are correctness and regression
evidence only, never production-latency claims.

## Performance Goals

- Zero measured heap allocation or deallocation on the declared hot paths
  (parsing, sequencing, risk, matching, reports, SPSC handoff) after
  initialization.
- O(1)-with-respect-to-book-shape identity lookups: open-addressed `OrderId`
  and account/reservation indices at bounded load factors.
- O(log levels) price-level maintenance and O(1) best-price discovery through
  per-side sorted-level indices.
- Flat per-operation latency independent of FIFO depth: stable-slot intrusive
  levels never shift peer orders.
- Single-traversal matching: one plan walk, one mutation walk, one report per
  fill, and preflighted capacity so rejection mutates nothing.

Current desktop-smoke evidence (Intel N95, Windows 11, Rust 1.96.0, fat LTO;
methodology and raw tables in [docs/PERFORMANCE.md](docs/PERFORMANCE.md)). All
scenarios warm up before sampling, hold their fixtures in steady state, and
report sorted percentiles, so single scheduler preemptions cannot dominate the
headline numbers:

| Workload | p50 | p99.9 | Mean |
| --- | ---: | ---: | ---: |
| Gateway packet-to-report, 200,000 messages | 300 ns | 400 ns | 194 ns/msg |
| Indexed cancel, 512-level book | — | — | 63-90 ns/cancel |
| Risk check / fill / cancel / settle, 90% occupancy | 100-200 ns | 200-300 ns | 125-144 ns |
| Best-price discovery, 120 active levels | 100 ns | 200 ns | 110 ns |
| Match-plan submit (non-crossing to 8-fill) | 200 ns | 300-900 ns | 206-314 ns |

These are noisy desktop numbers with timer overhead included; the reported
maximum still captures occasional OS preemption and is kept visible rather than
trimmed. They validate the allocation discipline and catch regressions; they
are not Linux/NIC production-latency evidence.

## Determinism Goals

Identical input and initial state must produce identical logical output. In
practice that means: fixed-capacity arrays instead of hash maps with random
iteration order, one writer per book, no wall-clock or thread-timing
dependence in state transitions, canonical big-endian digest lanes, and a
golden replay digest (`hft-replay`) that every change must keep stable.
Generated-command tests use seeded, reproducible generators.

## Safety Policy

Safe Rust is the default. Domain logic, parsing, risk, matching, gateway
coordination, and replay forbid unsafe code (`#![forbid(unsafe_code)]`).
Repository-owned unsafe is confined to three documented sites: SPSC slot
access (Release/Acquire publication, Loom-modelled), the optional vendor C ABI
wrapper (opaque handles, fixed-width types, explicit ownership and error
codes), and the benchmark counting allocator. Default builds are Rust-only;
the optional C test shim builds behind `--features vendor-sdk` and is covered
by ABI tests plus ASan/UBSan in CI. See [docs/SAFETY.md](docs/SAFETY.md) for
the full inventory and native-boundary policy.

## Verification Evidence

| Gate | Evidence |
| --- | --- |
| Workspace correctness | Formatting, all-target check, Clippy with warnings denied, unit/integration/doc tests |
| Parser robustness | Boundary cases, malformed-input smoke, and a `cargo-fuzz` target |
| Concurrency | Cross-thread FIFO stress test and Loom Release/Acquire model |
| Memory safety | Miri on parser/risk/book and AddressSanitizer on the FFI crate |
| Hot-path allocation | Post-warm-up counter asserts zero allocation and deallocation deltas |
| Replay stability | Golden final-state digest with canonical byte-order encoding |
| Language boundary | Rust source-ratio gate and an allowlist for crates containing unsafe code |

## Architecture

| Crate | Responsibility | Hot-path allocation |
| --- | --- | --- |
| `hft-types` | Fixed-width domain types and reports | None |
| `hft-wire` | Validated lifetime-bound parsing | None |
| `hft-io` | RAII frame leases, in-memory and UDP baseline | Preallocated frame |
| `hft-spsc` | Bounded cache-aware SPSC handoff | None after construction |
| `hft-risk` | Fixed-capacity limits, indexed accounts and reservations | None |
| `hft-book` | Price-time matching: stable-slot FIFO levels, sorted-level indices, `OrderId` index, match plans | None |
| `hft-gateway` | Transaction coordination and report accounting | None |
| `hft-replay` | Ordered replay and stable final-state digest | None in engine |
| `hft-ffi` | Optional vendor C ABI ownership wrapper | Vendor-defined |
| `hft-bench` | Allocation assertion and timing smoke harness | Zero measured delta |
| `hft-cli` | Cold-path operational entry point | Out of scope |

Each instrument group is intended to run as a single matching shard. If a
packet crosses cores, the supported design is normalization once into a fixed
size SPSC slot. That is a bounded single-copy handoff, not end-to-end zero-copy.

Matching internals: each side of the book keeps its price levels in a
fixed-capacity array. A per-side sorted-level index (binary search plus a
free-slot pool) provides O(1) best-price discovery and O(log n) level
maintenance. Inside a level, orders live in an intrusive doubly linked FIFO
with stable slot handles, so fills and cancels never shift peer orders. An
open-addressed `OrderId -> slot` index makes cancel/lookup expected O(1).
`submit` builds a bounded `MatchPlan` in one traversal (preflighting every
capacity and validity condition), then applies it infallibly: a rejected order
mutates nothing, and no rollback machinery exists to drift out of sync.

## Workflow Diagrams

### Packet-to-Report Data Path

[![Packet-to-report workflow](docs/diagrams/packet-to-report.svg)](docs/diagrams/packet-to-report.mmd)

The parser borrows the RX frame. Only the optional cross-core handoff copies a
normalized fixed-size order into a preallocated queue slot.

### New-Order Transaction

[![New-order transaction](docs/diagrams/new-order-transaction.svg)](docs/diagrams/new-order-transaction.mmd)

Both SVG images are generated from the linked, version-controlled Mermaid
sources so architectural changes remain reviewable.

## Key Takeaways and Lessons Learned

| Lesson | Design decision | Evidence |
| --- | --- | --- |
| Ownership is part of latency design | Frame leases bind parser borrows to RX-buffer lifetime | `hft-io`, `hft-wire`, lease and truncation tests |
| Bounded state makes overload deterministic | Fixed arrays and SPSC slots reject instead of growing | Capacity, backpressure, and no-overwrite tests |
| Zero-copy claims need exact boundaries | Borrow the frame; describe cross-core normalization as one bounded copy | Architecture and safety documentation |
| Complete preflight beats rollback | Decide every fallible condition before mutation; application becomes infallible | Book capacity-rejection and model-equivalence tests |
| Atomic ordering needs a proof | Release publishes slots; Acquire observes publication and reclamation | Loom model and cross-thread FIFO stress test |
| Replay determinism is cross-platform | Canonical big-endian digest lanes plus a golden final-state value | `hft-risk` digest and `hft-replay` regression test |
| Benchmarks need scope and context | Separate zero-allocation evidence from unmeasured NIC/kernel latency | Release allocator gate and performance protocol |
| Benchmarks must measure live work | Destructive operations need freshly populated fixtures, not closed IDs | Risk occupancy harness with per-operation engines |
| Identity lookup should not scale with occupancy | Trade a fixed memory budget for open-addressed `OrderId` lookup | Before/after cancellation benchmark and collision invariants |
| Stable handles remove cascade updates | Slot handles survive unrelated mutations; stale handles fail closed | Handle-stability and stale-handle tests |
| Unsafe code belongs at narrow boundaries | Keep domain, risk, matching, parsing, and replay in safe Rust | CI unsafe allowlist, Miri, and FFI AddressSanitizer |

The detailed design reflection is in
[Engineering Lessons Learned](docs/LEARNINGS.md).

## Capability Matrix

| Capability | State | Evidence / limitation |
| --- | --- | --- |
| Borrowed binary parser | Implemented | Boundary and malformed-input tests |
| New/cancel wire messages | Implemented | Fixed lengths and big-endian scalar fields |
| RAII RX lease | Implemented | In-memory and UDP buffer ownership |
| Fixed-capacity risk | Implemented | Quantity, notional, position, open-order, collar, duplicate, kill checks |
| Indexed risk state | Implemented | O(1) account/reservation lookup at bounded load; occupancy benchmark |
| Price-time book | Implemented | Stable-slot FIFO levels; fills/cancels never shift peers |
| Sorted-level discovery | Implemented | O(1) best price; binary-search level maintenance; slot reuse |
| Matching transaction plan | Implemented | Single plan traversal; preflighted atomic rejection; no rollback path |
| Deterministic replay | Implemented | Stable final-state digest test |
| Session sequence enforcement | Implemented | Duplicates/gaps fail closed without advancing |
| Owner-authorized cancel | Implemented | FIFO-preserving removal and exact risk release |
| Cache-aware SPSC | Implemented | Release/Acquire docs, stress test, Loom model |
| Allocation audit | Implemented | Release executable asserts zero measured deltas |
| UDP baseline | Implemented | Syscall path; not a latency claim |
| AF_XDP backend | Planned | Feature-gated availability marker only |
| Vendor SDK | Planned | Safe wrapper exists; no proprietary SDK linked |
| Hardware perf counters | Planned | Requires Linux/perf and a dedicated runner |

## Current Limitations

- No external venue traffic, persistence, IOC/FOK/post-only/replace order
  semantics, recovery journal, sequence retransmission protocol,
  authentication, or venue-certified session protocol is implemented.
- Order IDs are monotonically increasing per gateway session. Reuse and
  out-of-order IDs fail closed.
- The book rejects when report, order, per-price FIFO, or price-level capacity
  would be exceeded; capacity is selected at compile time.
- Identity lookup is expected O(1) at the index's bounded live load; probe
  lengths still grow with clustering near maximum occupancy.
- Timing output includes measurement overhead and desktop scheduler noise. See
  [docs/PERFORMANCE.md](docs/PERFORMANCE.md).
- See [docs/SAFETY.md](docs/SAFETY.md) for every unsafe boundary and
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for ownership.

## Review Path

For a focused engineering review:

1. `hft-wire`: borrowed validation and protocol boundaries.
2. `hft-risk`: conservative exposure and deterministic rejection order.
3. `hft-book`: price-time matching, plan preflight, and stable-slot levels.
4. `hft-gateway`: sequence enforcement and risk/book lifecycle coordination.
5. `hft-spsc`: documented Release/Acquire ownership transfer.
6. `hft-bench`: measured allocation assertion and reproducibility limitations.

The protocol is specified in [docs/PROTOCOL.md](docs/PROTOCOL.md); operational
failure behavior is in [docs/OPERATIONS.md](docs/OPERATIONS.md).

## Project

- [Review](docs/REVIEW.md)
- [Engineering lessons learned](docs/LEARNINGS.md)
- [Roadmap](docs/ROADMAP.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
- [Changes](CHANGES.md)

Licensed under the [MIT License](LICENSE-MIT).
