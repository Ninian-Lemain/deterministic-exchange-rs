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

Tests: FIFO order and full rejection, cross-thread transfer, invalid capacity,
and a Loom publication model (`--features loom`, run in CI).

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
