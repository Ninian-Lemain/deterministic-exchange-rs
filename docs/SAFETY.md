# Safety

Every library except `hft-spsc` and `hft-ffi` uses `#![forbid(unsafe_code)]`.
This includes the `hft-bench` library. Its executable installs the counting
allocator described below.

[CI](../.github/workflows/ci.yml) scans Rust source under `crates` for unsafe
blocks, functions, implementations, foreign declarations, `UnsafeCell`, and
mutable statics. Matching files must equal this allowlist:

- `crates/hft-spsc/src/lib.rs`
- `crates/hft-ffi/src/lib.rs`
- `crates/hft-ffi/tests/native_abi.rs`
- `crates/hft-bench/src/main.rs`

This text scan limits where unsafe code can appear. Review and tests must
still establish the invariants within those files.

## SPSC queue

The queue stores slots as `UnsafeCell<MaybeUninit<T>>`. The producer alone
writes each slot before publishing it with a Release store to `tail`. The
consumer reads it after an Acquire load observes publication, then reclaims
the slot with a Release store to `head`. The producer acquires that reclamation
before reusing the slot.

The safety review depends on these invariants:

- Slot access stays inside `Slot<T>`. With `--features loom`, the wrapper uses
  `loom::cell::UnsafeCell` so Loom tracks the algorithm's slot reads and writes.
- Each reclaimed slot is initialized once before publication. Its value moves
  out once through `assume_init_read`, or the queue drops it while still live.
  Queue destruction has exclusive access and visits only the published
  half-open range `[head, tail)`.
- `into_inner` reserves space for every live value before draining. It advances
  the stored head before each move, so unwinding cannot make queue destruction
  revisit a moved value.
- Capacity `N` is a nonzero power of two. Slot indices are wrapping `usize`
  counters masked with `N - 1`. Empty means `head == tail`. Full means
  `tail.wrapping_sub(head) == N`.
- Each endpoint owns its cursor and cached peer position. An apparent full or
  empty condition refreshes the cache with an Acquire load. A stale cache can
  delay progress but cannot permit access to an unavailable slot.
- `split(&mut self)` creates exactly one producer and one consumer. Both borrow
  the queue for `'queue`. A later split starts from the published positions,
  including any pending values. Sharing the queue across threads requires
  `T: Send` through its `Sync` implementation.

Unit and integration tests cover FIFO order, full rejection, cross-thread
transfer, invalid capacity, capacity-one slot reuse, endpoint recreation on
empty and nonempty queues, draining, and exactly-once destruction across
endpoint lifetimes. Seeded schedules compare operations with a `VecDeque`.
Loom tests exercise the same producer and consumer core functions with modeled
atomics and cells. CI also configures Miri for this crate.

## Foreign function interface

`VendorApi::new` is the FFI crate's unsafe public constructor. Its caller must
establish the callback contract:

- Callback addresses remain callable for the process lifetime. No callback
  unwinds or throws across the C ABI.
- `create` writes only to its output handle pointer and does not retain that
  pointer. Status zero with a nonnull handle transfers unique ownership to the
  session. A nonzero status transfers no resource.
- `send` reads at most the supplied length during the call. It neither writes
  through nor retains the payload pointer. Every returned status leaves the
  handle live and owned by the session.
- `destroy` accepts each live handle once, releases it before returning, and
  does not retain it.

Nullable callback fields allow a C table with a null entry to have a valid Rust
representation. `VendorSession::open` rejects missing callbacks before calling
foreign code. It also rejects a successful `create` that returns a null handle.
The session stores validated callbacks and is neither `Send` nor `Sync`. When
the session is dropped, it calls `destroy` once.

Tests cover ownership and destruction, create and send errors, null handles,
oversized lengths, and the native ABI checks below. A compile-fail doc test
checks that a session cannot be sent between threads.

## Counting allocator

The benchmark executable's `GlobalAlloc` implementation delegates to `System`
with the caller's pointer, layout, and size contracts. Atomic counters record
allocation activity without changing ownership or alignment. Linking the
benchmark library does not install this allocator.

The reduced-suite integration test launches the executable with its allocator.
It checks reported hot-path allocation counts and positive counts for recovery
workloads that allocate. This does not prove that every workload places its
counters correctly. Known gaps are recorded in [PERFORMANCE.md](PERFORMANCE.md).

## Native boundary policy

Default builds compile no repository C or C++ code and require no C or C++
compiler for the FFI crate. Beyond ABI probes, optional C++ is limited to real
vendor or NIC SDK adapters. Those adapters must use the C ABI in
[hft_vendor_api.h](../crates/hft-ffi/native/hft_vendor_api.h): opaque handles,
fixed-width fields, explicit ownership, and integer error codes.
Exceptions and C++ standard-library types must not cross the boundary. The
SPSC queue remains in Rust.

The `vendor-sdk` feature currently compiles the repository's C test shim and a
C++ header probe. It does not supply a proprietary SDK. Either probe failing
to compile fails the feature build.

`cargo test -p hft-ffi --features vendor-sdk` runs the native ABI suite. The
suite compares table size, alignment, and every field offset before invoking
table callbacks. It checks session creation, payload access, send errors,
destruction, and null-callback rejection against the compiled C shim.

## Validation scope

The CI workflow configures these checks:

| Check | Scope |
| --- | --- |
| Stable Rust | Formatting, workspace check and Clippy for all targets and features, warnings denied, workspace tests and doc tests |
| Default features | Benchmark, soak, and engine tests, parser malformed-input smoke test, release soak smoke profile, release benchmark suite |
| MSRV 1.85.0 | Locked workspace check for all targets and workspace tests |
| Loom | SPSC tests using the queue's actual algorithm |
| Nightly Miri | `hft-wire`, `hft-risk`, `hft-book`, and `hft-spsc` |
| Nightly Linux ASan | Rust FFI tests and compiled C shim with address sanitization |
| Nightly Linux UBSan | FFI tests linked to the undefined-behavior-sanitized C shim |
| Source policy | Unsafe allowlist and at least 90% Rust among counted Rust and native source lines |

This table describes CI configuration. It does not establish which checks ran
or passed locally. The Miri job excludes the FFI crate and native shim.
Sanitizer coverage applies to the repository shim, not a proprietary SDK.
Dedicated-hardware validation remains open.
