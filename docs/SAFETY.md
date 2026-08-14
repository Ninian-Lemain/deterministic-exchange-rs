# Safety

Safe Rust is the default. `hft-types`, `hft-wire`, `hft-io`, `hft-risk`,
`hft-book`, `hft-gateway`, `hft-replay`, and `hft-cli` forbid unsafe code.

## SPSC

`hft-spsc` uses `UnsafeCell<MaybeUninit<T>>` because producer and consumer own
opposite phases of a slot. Release publication pairs with Acquire observation;
Release reclamation pairs with Acquire capacity refresh. The producer alone
writes and the consumer alone reads. Queue drop has exclusive access and drops
only the published half-open range. `T: Send` is required for `Sync`.

## FFI

`hft-ffi` accepts a C vtable only through an unsafe constructor. The caller must
guarantee ABI validity, no exception/unwind, vtable lifetime, one uniquely owned
non-null handle, and non-retention of payload pointers. The safe session is
neither `Send` nor `Sync`; Drop calls the matching destroy exactly once.

No C++ business logic is included. The wrapper is tested with Rust test shims,
not a proprietary SDK.

## Allocation Harness

`hft-bench` delegates every global allocation operation to `System` using the
same pointer/layout contracts and only increments atomic counters. It is an
isolated executable and does not alter library allocator behavior.

## Unsupported Validation

Miri was not run because it is not installed for the available stable toolchain.
ASan/UBSan require a real compiled vendor shim and are not claimed. These gates
remain on the hardware/vendor roadmap.
