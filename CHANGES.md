# Changelog

## Unreleased

## v0.3.0 - 2026-08-16

### Changed

- Replaced per-level order-array shifts with intrusive doubly linked FIFOs:
  head, tail, free-list, and stable slot handles. Insert, fill, and cancel no
  longer shift peer orders or rewrite their index locations. The order index
  now maps IDs to stable slot handles, never FIFO positions. Public APIs are
  unchanged.

### Added

- Added invariant coverage for handle stability across unrelated mutations,
  stale-handle fail-closed behavior, disjoint live/free sets, and atomic
  full-level rejection, plus a 600-command generated comparison against the
  array reference model.
- Added a FIFO depth benchmark covering head/middle/tail cancel and head fill
  at depths 1, 4, 16, 64, and the 512-order maximum.

### Verification

- Flat 100 ns p50 for head/middle/tail cancel and head fill at all depths
  (down from up to 3,300 ns at depth 512), zero allocation deltas, unchanged
  logical digest `64321af91735b704`; see `docs/PERFORMANCE.md`.
- Added FFI error-path tests covering create failure, null-handle rejection,
  send failure with destroy-on-drop, and oversized payload rejection, plus a
  compile-fail test for session thread confinement.
- Added C ABI tests for the native boundary behind `--features vendor-sdk`,
  with ASan and UBSan coverage in CI.
- Added a counting-allocator invariant test to the benchmark harness.
- Documented the unsafe inventory, invariants, tests, and native boundary
  policy in `docs/SAFETY.md`.
- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.2.0 - 2026-08-14

### Added

- Added a fixed-capacity, open-addressed `OrderId` index with deterministic
  collision and back-shift deletion handling.
- Added invariant coverage for collisions, FIFO compaction, fills,
  cross-side relocation, cancellation, and index-slot reuse.
- Added a staged library-first and operational roadmap with evidence gates.

### Improved

- Cancellation now locates an order through the index instead of scanning both
  sides of the book.
- Index storage is preallocated with the book and does not allocate on the hot
  path.

### Fixed

- No user-visible correctness defect was fixed in this focused release.

### Performance

- In a 512-level cancellation benchmark, median lookup-and-cancel latency fell
  from 1,329 ns to 26 ns per order (98.0% lower), while median throughput rose
  from 752,347 to 37,796,329 cancellations/second (50.2x). Both variants
  reported zero measured allocations and deallocations. See
  `docs/PERFORMANCE.md` for the workload and limitations.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.1.0 - 2026-08-14

- Added an 11-crate Rust workspace and end-to-end execution vertical slice.
- Added borrowed parsing, RAII frames, fixed-capacity risk and price-time book.
- Added bounded SPSC queue, Loom model, deterministic replay, and C ABI wrapper.
- Added a release allocation assertion, CI, source-ratio check, and operations
  documentation.
- Added fixed-size cancel messages, owner-authorized FIFO-preserving cancel,
  exact risk-reservation release, and fail-closed session sequence enforcement.
- Canonicalized risk-state digest encoding across CPU byte orders.
- Added measured-result evidence and an engineering lessons review path.
