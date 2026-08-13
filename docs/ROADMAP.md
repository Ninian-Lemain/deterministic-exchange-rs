# Roadmap

## Phase 1: Deterministic Vertical Slice - Complete

- Fixed-width domain newtypes and explicit rejects.
- Lifetime-bound parser and RAII frame lease.
- Fixed-capacity risk and price-time matching.
- Transactional gateway, reports, and stable replay digest.
- Strict session sequencing and owner-authorized cancellation.

## Phase 2: Concurrency and Safety - Complete for the Slice

- Power-of-two SPSC with separated producer/consumer positions.
- Release/Acquire publication and reclamation.
- Loom publication model and cross-thread FIFO stress test.
- Narrow C ABI wrapper with opaque ownership.

## Phase 3: Operations and Evidence - Complete for Portable CI

- Allocation assertion, timing smoke harness, source-ratio gate, Clippy, tests,
  documentation, and GitHub templates.

## Phase 4: Exchange Lifecycle - Planned

- Replace, explicit connection state machine, sequence retransmission, durable
  journal/snapshots, recovery replay, self-trade prevention, and market-data
  publication.
- Add a session-scoped fixed-capacity order-ID table if monotonic IDs cannot be
  guaranteed by the production protocol.

## Phase 5: Linux Native Networking - Hardware Dependent

- Implement AF_XDP UMEM/ring ownership, descriptor RAII, mlock/prefault,
  hugepages, CPU isolation, affinity, and NUMA placement.
- Exercise ring exhaustion, shutdown, link reset, and recovery.

## Phase 6: Vendor Integration - SDK Dependent

- Link a real vendor shim behind `vendor-sdk`.
- Add ABI layout tests and ASan/UBSan integration tests.

## Phase 7: Dedicated Performance Qualification - Hardware Dependent

- Record p50/p90/p99/p99.9/max, throughput, occupancy/backpressure, branch and
  cache misses, context switches, page faults, allocation, and reproducibility
  metadata on pinned Linux hardware.
- Establish thresholds only after a stable dedicated-runner baseline.
