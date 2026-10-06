# Roadmap implementation and Linux validation

The pending software integration is implemented: fixed routed journaled engines,
owned session admission, replay configuration including report bounds, immutable
checkpoint bundles and operational verification commands. This is pre-v1 work;
dedicated Linux hardware qualification and external independent review remain open.

## Changes and guarantees

| Change | Files | Contract |
| --- | --- | --- |
| Routed and session ownership | `hft-engine/src/routed.rs`, `session.rs`, `lib.rs` | Private child engines, fixed routes, per-instrument sequence domains, staged transport updates; overload consumes nothing |
| Session overflow repair | `hft-session/src/lib.rs` | Deadline arithmetic precedes sequence, state and timeout mutation |
| Replay configuration | `hft-engine/src/config.rs`, `builder.rs` | Canonical sidecar binds reports, capacity shape, instrument, account limits and format versions before replay |
| Checkpoint operations | `hft-engine/src/operations.rs`, `hft-cli` | New-path publication, completion integrity, verified backup/restore and explicit compatibility checks |
| Combined faults | `hft-soak/src/combined.rs`, runner, result and CLI | Session/routing/event/journal pressure, worker failure, configured restart and concurrent shutdown in one scenario |
| Sustained capture | `scripts/soak/run_linux.py`, soak workflow | One sustained process, retained seed repeats, actual elapsed duration and sampled memory |
| Benchmark repair and extension | `hft-bench/src/session_workloads.rs`, `service_workloads.rs` and tests | Actual outcomes, payload refill, measured allocator boundaries and four facade cells |
| Linux safety capture | `scripts/linux/run_safety.sh` | Actual-algorithm Loom, Miri, FFI ASan/UBSan and bounded parser fuzzing |

Matching, risk rules, wire bytes, journal records, snapshot v1 and golden engine
replay digests are unchanged. Business logic remains safe Rust. No queue memory
ordering was weakened. Configuration vectors and snapshot/bundle encoding allocate
only during cold setup or operations; measured admission remains allocation-free.
Historical invalid session timings stay excluded from comparisons.

## Completed Linux checks

Ubuntu 26.04.1 under WSL 2, kernel 6.6.87.2, Rust 1.96.0 and native x86_64 Linux
builds passed formatting, default/all-feature checks and strict Clippy. Default
tests passed 253 cases; all-feature tests passed 245. Rust 1.85 check and default
tests passed as well. Instrumented Loom builds use their dedicated model tests;
native CLI and queue integration tests run in the default configuration.

Miri passed 79 cases across book, risk, wire and the actual SPSC implementation,
including the book reference model. Both actual-algorithm Loom models passed.
AddressSanitizer and UndefinedBehaviorSanitizer passed the native ABI tests.
UBSan initially failed to link because the Clang runtime package was missing;
installing `libclang-rt-21-dev` resolved that environment prerequisite. Original
failure logs are retained alongside the successful follow-up. Parser fuzzing
completed 38,766,660 executions in 61 seconds without failure.

The full suite now emits 140 benchmark cells. Every one of the 137 non-recovery
cells measured zero allocations and deallocations. The four routed/session frame
and command cells produced equal checksums and final snapshots. A Linux release
checkpoint, backup, restore and compatibility workflow succeeded with next
sequence 2.

The WSL profile captures counters and call stacks for the entire benchmark,
including untimed setup. It is diagnostic evidence; validation processes ran
concurrently and timings are not a qualified latency comparison. WSL is explicitly
rejected by dedicated qualification preflight. Native CPU placement, governor,
SMT, IRQ and NUMA requirements still need a dedicated host.

## Sustained qualification

Two captures use all retained seeds with 1,000,000 and 65,536 steps per seed pass,
respectively. Both sustained processes completed successfully with every retained
seed, recurring combined faults and equal repeated state, event and counter results.

| Steps per seed pass | Actual seconds | Verified seed results | Combined rounds per verified pass | Peak sampled RSS | Initial / final quarter median RSS |
| --- | --- | --- | --- | --- | --- |
| 1,000,000 | 7,213.34 | 1,304 | 245 | 3,977,216 bytes | 3,543,040 / 3,530,752 bytes |
| 65,536 | 7,201.41 | 20,676 | 16 | 3,846,144 bytes | 3,399,680 / 3,432,448 bytes |

Each verified result represents two independently executed matching passes.
Memory remained bounded; neither capture reported divergence, missing fault
coverage or unexplained RSS growth. These WSL runs qualify the declared in-memory
fault workload, not dedicated latency or real disk fault behavior.

[Evidence archive](evidence/roadmap-2026-10-06.zip), SHA-256:
`3b9600a54f5ba1d499205cc2285f07d415ed71b0865fe6d090ed35ba45c97194`.
It retains completed host results, per-seed results, memory samples, safety and
verification logs, profiling, binary identities and validation sources. Its source
comparison records six later benchmark/test/CLI metadata edits after the first
capture; soak and production dependency sources match both captures. The lockfile
difference only adds a CLI test dependency. The second capture matches the source
archive exactly. CI [run 89](https://github.com/Ninian-Lemain/deterministic-exchange-rs/actions/runs/37532150878)
passed quality, MSRV, Miri and sanitizer jobs for implementation commit `2d0dca2`.

Raw development evidence also lives under `target/roadmap-validation` in
the Linux checkout. External independent API,
unsafe-boundary, recovery-format and operational review is still required before
a v1 release candidate. Later NIC backends, venue adapters and replication remain
separate designs with their own hardware and protocol requirements.
