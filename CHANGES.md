# Changelog

## Unreleased

### Added

- Added canonical logical state export and restore for the book, risk engine,
  and gateway.
- Added a versioned big-endian snapshot format with SHA-256 integrity and fixed
  compatibility bytes.
- Added snapshot plus journal tail recovery with sequence, capacity, and state
  validation.
- Added recovery benchmarks for encoding, verified restore, and tail replay.

### Changed

- Marked v0.17 recovery and state integrity as implemented.
- Rewrote the README and the learnings document in a direct technical voice
  and removed typographic dashes from prose. No code changed.

## v0.6.2 - 2026-08-17

### Changed

- Renamed the project from `hft-engine-rs` to `deterministic-exchange-rs` and
  repositioned the documentation: a deterministic, allocation-free matching
  engine and execution gateway, the backend core of an electronic financial
  exchange, built for learning and experimentation. No code changed. The
  workspace crate names (`hft-types`, `hft-book`, etc.) are internal package
  identifiers and are unchanged.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.6.1 - 2026-08-17

### Fixed

- Benchmark harness hardening; no library code changed. Every scenario now runs
  a fixed warm-up before sampling and keeps its fixture in a steady state, with
  teardown (replenishing rest or cancel) outside the timed region. Previously
  the gateway loop sampled its coldest first iterations, `submit_cross`
  depleted all makers after `active` samples and then measured resting
  rejections, `level_create` filled the book and then measured capacity
  rejections, and `risk_check` at 90% occupancy overflowed the 1,024-slot order
  capacity mid-run and measured `OrderCapacity` rejections.
- All benches now report p50/p90/p99/p99.9/max alongside the mean, so a single
  scheduler preemption can no longer dominate the headline number. The maximum
  is still reported, not trimmed.

### Performance

- With cold-start samples excluded, the gateway workload's max dropped from
  34 us-1.4 ms to 400 ns in the representative run (p50 300 ns, p99.9 400 ns).
  Book and risk cells report p50 100-300 ns with p99.9 within 2-3x of p50.
  Residual max spikes are single desktop scheduler preemptions, documented in
  `docs/PERFORMANCE.md`. Zero measured allocations and unchanged logical digest
  `64321af91735b704`.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.6.0 - 2026-08-17

### Changed

- Replaced the duplicate simulation walk with a compact `MatchPlan`:
  `build_plan` preflights validation, duplicates, report capacity, level
  capacity, and liquidity in a single traversal of the sorted-level index, and
  `apply_plan` performs the mutation walk. Because every fallible condition is
  decided during preflight against an unchanged book, plan application is
  infallible; there is no rollback path to test or to get wrong.
- The crossing walk now stops at the taker's price limit instead of scanning
  every occupied level, and the resting path finds or allocates price levels
  through the sorted-level index (binary search plus a free-slot pool) instead
  of linear scans over the level array.
- Level removal from the sorted index is a binary search instead of a linear
  scan, and order-index and risk-index slot math no longer re-derives
  coordinates through fallible helpers.
- `RiskEngine::cancel_reservation` resolves a reservation once instead of
  three times, and fill/settle exposure accounting shares one helper.
- Fixed the risk benchmark: fill, cancel, and settle now measure live
  reservations on freshly populated engines. The previous harness closed the
  reservations in its fill loop and then measured `UnknownOrder` rejections
  for the cancel and settle cells.
- Removed the dead `ReportBuffer::pop` (only the removed rollback path used
  it). Public APIs are unchanged.

### Added

- Book invariant coverage for duplicate-ID rejection atomicity, crossing stops
  at the price limit, partial-cross-then-rest price-time order, exact report
  capacity fit, emptied-level slot reuse, and full-book rest rejection.
- The level-index consistency check now verifies that occupied and free level
  slots partition the level array exactly once.
- Added a match-plan benchmark measuring non-crossing, single/multi-fill,
  report-full rejection, and deep rejection across 2,000 samples each.

### Performance

- Cancel benchmark: 546 ns to 56-89 ns per cancel (binary-search level
  removal).
- Price benchmark: level creation 418 ns to 75-147 ns mean and discovery
  145 ns to 73-157 ns mean on the 128-level/120-active shape (early crossing
  stop, indexed level allocation).
- Match-plan submit latency is 72-146 ns mean across non-crossing,
  single/multi-fill, and both rejection scenarios. Zero measured allocations
  and unchanged logical digest `64321af91735b704`. See `docs/PERFORMANCE.md`.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.5.0 - 2026-08-16

### Changed

- Replaced O(LEVELS) linear best-price scan with a sorted-level index for
  O(1) best-bid/ask discovery. `best_crossing_level` and `simulate_sorted`
  iterate only occupied levels. Insert and remove maintain sort order in the
  index. The order index, FIFO, and risk state are unchanged.

### Added

- Added `LevelIndex` for per-side sorted-level tracking with insert, remove,
  and iteration.
- Added invariant coverage for boundary-price crossing, empty-level rejection,
  model-equivalent digest, churn-survival consistency, and no-skipped-liquidity
  properties.
- Added a price discovery benchmark measuring submit_cross, discovery, and
  level_create across dense and sparse book shapes.

### Performance

- Discovery latency is flat 76-86 ns mean across all measured book shapes
  (64-120 active levels). Zero measured allocations and unchanged logical
  digest `64321af91735b704`. See `docs/PERFORMANCE.md`.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

## v0.4.0 - 2026-08-16

### Changed

- Replaced linear-scan risk lookups with fixed-capacity open-addressed indices
  for `AccountId -> AccountState` and `OrderId -> Reservation`. Reservations
  now live in a stable-slot free-list with deterministic collision/back-shift
  deletion. The order index maps IDs to stable slot handles. Public APIs are
  unchanged.

### Added

- Added invariant coverage for collisions, relocation, slot reuse, duplicate
  and full rejection, handle stability across unrelated churn, stale-handle
  fail-closed, and reservation-totals-equal-exposure after fill/cancel/settle.
- Added a risk occupancy benchmark harness measuring risk_check,
  reservation_lookup, fill, cancel, settle, reject, and account_lookup at
  10%, 50%, and 90% configured occupancy.

### Performance

- Up to 53x latency reduction at 90% reservation occupancy (2,892 ns to 55 ns
  for cancel), 25x for risk check (1,631 ns to 65 ns), and 2.8x for account
  lookups at 90% account occupancy. Zero measured allocations and unchanged
  logical digest `64321af91735b704`. See `docs/PERFORMANCE.md`.

### Verification

- `cargo fmt --all --check`
- `cargo check --workspace --all-targets --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`

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
