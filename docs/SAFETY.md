# Safety

Safe Rust is the default. Every library except `hft-spsc` and `hft-ffi`
forbids unsafe code. The `hft-bench` library also forbids it. CI rejects
unsafe blocks, functions, impls, foreign declarations, `UnsafeCell`, and
mutable statics outside the allowlisted files.

## Unsafe Inventory

Every repository-owned unsafe site carries a documented invariant and a test.

### SPSC (`crates/hft-spsc/src/lib.rs`)

`UnsafeCell<MaybeUninit<T>>` slots: the producer alone writes a slot before
Release publication; the consumer alone reads it after Acquire observation and
before Release reclamation. `split` requires exclusive access and creates
exactly one endpoint of each kind. Drop holds exclusive access and drops only
the published half-open range `[head, tail)`. `T: Send` is required for
`Sync`.

Audit details:

- **UnsafeCell**: slot access is confined to the `Slot<T>` wrapper. Under
  `--features loom` it uses `loom::cell::UnsafeCell`, so Loom tracks the same
  reads and writes as the normal build.
- **Initialization/drop**: a slot is written exactly once between reclamation
  and publication; it is read exactly once by the consumer (`assume_init_read`)
  or dropped once by `Drop`. `into_inner` reserves the full live count before
  moving values and advances the stored head before each move. Unwinding
  cannot make `Drop` visit a moved value.
- **Wrap**: indices are `usize` counters masked with `N - 1`; fullness uses
  wrapping subtraction of cached peer positions. Capacity one wraps every
  operation and is covered explicitly.
- **Full/empty**: empty means `head == tail`; full means `tail - head == N`.
  Cached positions are refreshed with Acquire loads, so a stale cache can only
  cause a retry, never an out-of-bounds access or lost capacity.
- **Endpoint lifetimes**: `Producer`/`Consumer` borrow the queue for `'queue`;
  `split(&mut)` guarantees one endpoint pair per borrow. A later split starts
  from the published head and tail, including when pending values remain. The
  core steps are shared-reference functions used by the endpoints and Loom
  tests.

Tests: FIFO order and full rejection, cross-thread transfer, invalid capacity,
endpoint recreation with empty and nonempty queues, drop-exactly-once property
coverage across endpoint epochs, seeded lossless schedules, Loom runs of the
actual algorithm under `--features loom`, and Miri on the whole crate.

### FFI (`crates/hft-ffi/src/lib.rs`)

`VendorApi::new` is the only unsafe public constructor. Its contract covers
callback lifetime, unwinding, handle ownership, error behavior, payload access,
and destruction. Nullable callback fields make a C table with a null entry a
valid Rust value. `VendorSession::open` rejects any missing callback before a
foreign call. The session stores validated callbacks and is neither `Send` nor
`Sync`. Drop calls `destroy` exactly once.

Tests: ownership/destroy round trip, `create` failure status, null-handle
rejection, null-callback rejection, `send` failure status with destroy-on-drop,
oversized length rejection, and the native ABI suite below.

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
Linux. The suite compares every table field offset before it calls a callback.
It also compiles the public header as C++. A feature build fails if either
native probe cannot compile. Default builds do not require a C or C++ compiler.

## Validation Status

- CI: fmt, workspace check, Clippy with warnings denied, tests, doc tests,
  Loom SPSC model, unsafe allowlist, Rust source ratio.
- Miri (CI, nightly): `hft-wire`, `hft-risk`, `hft-book`, `hft-spsc`.
- ASan and UBSan (CI, nightly Linux): `hft-ffi` with the compiled C test shim.
- Remaining: Miri does not execute foreign callbacks. Sanitizers exercise the
  repository test shim, not a proprietary SDK. Dedicated-hardware validation
  remains on the roadmap.
