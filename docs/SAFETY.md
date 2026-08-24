# Safety

Safe Rust is the default. `hft-types`, `hft-wire`, `hft-io`, `hft-risk`,
`hft-book`, `hft-gateway`, `hft-replay`, and `hft-cli` forbid unsafe code, so
matching, risk, parsing, sessions, and replay expose safe public APIs only.
CI rejects `unsafe` outside the allowlisted files.

## Unsafe Inventory

Every repository-owned unsafe site carries a documented invariant and a test.

### SPSC (`crates/hft-spsc/src/lib.rs`)

`UnsafeCell<MaybeUninit<T>>` slots: the producer alone writes a slot before
Release publication; the consumer alone reads it after Acquire observation and
before Release reclamation. `split` requires exclusive access and creates
exactly one endpoint of each kind. Drop holds exclusive access and drops only
the published half-open range `[head, tail)`. `T: Send` is required for
`Sync`.

v0.12 audit, point by point:

- **UnsafeCell**: slot access is confined to four sites (producer write,
  consumer read, `Drop`, `into_inner`). Under `--features loom` the cell type
  swaps to `loom::cell::UnsafeCell`, so Loom tracks every slot access.
- **Initialization/drop**: a slot is written exactly once between reclamation
  and publication; it is read exactly once by the consumer (`assume_init_read`)
  or dropped once by `Drop`/`into_inner`, which walk only `[head, tail)`.
  `into_inner` drains by value and then forgets the wrapper so no slot is
  dropped twice.
- **Wrap**: indices are `usize` counters masked with `N - 1`; fullness uses
  wrapping subtraction of cached peer positions. Capacity one wraps every
  operation and is covered explicitly.
- **Full/empty**: empty means `head == tail`; full means `tail - head == N`.
  Cached positions are refreshed with Acquire loads, so a stale cache can only
  cause a retry, never an out-of-bounds access or lost capacity.
- **Endpoint lifetimes**: `Producer`/`Consumer` borrow the queue for `'queue`;
  `split(&mut)` guarantees one endpoint pair per borrow. The core steps are
  shared-reference functions used verbatim by the endpoints and by the Loom
  tests, so the modeled algorithm is the shipped algorithm.

Tests: FIFO order and full rejection, cross-thread transfer, invalid capacity,
drop-exactly-once property coverage, seeded lossless schedules, actual-
algorithm Loom runs under `--features loom` (CI), and Miri on the whole crate
including the cross-thread transfer test (CI). Miri demonstrably rejects a
weakened Release publication as a data race; Loom demonstrably explores the
publication/consumption interleavings of the real algorithm.

### FFI (`crates/hft-ffi/src/lib.rs`)

`VendorSession::open` is the only unsafe public constructor. The caller
guarantees C ABI validity, no unwind across the boundary, vtable validity for
`'api`, one uniquely owned non-null handle per successful `create`, and no
retention of payload pointers. The session is neither `Send` nor `Sync`
(compile-fail doc test). Drop calls `destroy` exactly once.

Tests: ownership/destroy round trip, `create` failure status, null-handle
rejection, `send` failure status with destroy-on-drop, oversized length
rejection, and the native ABI suite below.

### Counting allocator (`crates/hft-bench/src/main.rs`)

Every `GlobalAlloc` operation delegates to `System` under the identical
pointer and layout contract; only atomic counters are added. It is an isolated
executable and does not alter library allocator behavior.

Test: allocation and deallocation counters track a `Vec` round trip.

## Native Boundary Policy

Default builds are Rust-only. Optional C++ is permitted only for a real
vendor/NIC SDK behind the C ABI declared in
`crates/hft-ffi/native/hft_vendor_api.h`: opaque handles, fixed-width fields,
explicit ownership and integer error codes, and no exceptions or C++
standard-library types across the boundary. The audited Rust SPSC stays in
Rust; it is never wrapped in C++ to hide unsafe code.

Enabling `--features vendor-sdk` compiles the C test shim
(`crates/hft-ffi/native/test_shim.c`) and runs ABI tests that compare the C
and Rust layouts and drive a full session across the compiled boundary,
including error propagation. CI runs these tests under ASan and UBSan on
Linux. A real vendor shim must pass the same gates before merge. Without a C
compiler the shim is skipped with a warning and the default build is
unaffected.

## Validation Status

- CI: fmt, workspace check, Clippy with warnings denied, tests, doc tests,
  Loom SPSC model, unsafe allowlist, Rust source ratio.
- Miri (CI, nightly): `hft-wire`, `hft-risk`, `hft-book`.
- ASan and UBSan (CI, nightly Linux): `hft-ffi` with the compiled C test shim.
- Remaining: Miri does not yet cover the `hft-spsc`/`hft-ffi` unsafe paths
  (the v0.12 audit owns that); sanitizers exercise the repository test shim,
  not a proprietary SDK; dedicated-hardware validation remains on the roadmap.
