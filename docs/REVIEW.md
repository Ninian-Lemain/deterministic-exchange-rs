# Review guide

This page identifies what to review and which evidence exists. It is not an
independent security audit or a claim that the code has no serious defects.
The project remains pre-v1.

## Review order

| Component | Questions to check |
| --- | --- |
| [Wire parser](../crates/hft-wire/src/lib.rs) | Are lengths, versions, byte order, side, and time-in-force validated before borrowed access? |
| [Risk](../crates/hft-risk/src/lib.rs) | Do checked arithmetic, limits, ownership, and reservation totals agree with live orders? |
| [Book](../crates/hft-book/src/lib.rs) | Are price-time priority, stable handles, level totals, and live/free slot separation preserved? |
| [Gateway](../crates/hft-gateway/src/lib.rs) | Does sequence consumption agree with rejection behavior? Are maker/taker fills and reservation releases accounted for once? |
| [Events](../crates/hft-events/src/lib.rs) | Is batch capacity reserved before mutation? Are event order and IDs stable across replay? |
| [Engine](../crates/hft-engine/src/lib.rs) | Can admission bypass the journal? Do queue pressure, persistence failure, shutdown, and snapshot cuts agree? |
| [Router](../crates/hft-router/src/lib.rs) | Are routes stable, endpoints correctly paired, and pending commands retained on event pressure? |
| [Journal and recovery](ENGINE.md#shutdown-and-restart) | Are persisted records ordered and snapshots canonical? Does restore reject incompatible or incomplete input? |
| [Unsafe boundaries](SAFETY.md) | Do ownership, memory ordering, initialization, drop, and FFI contracts justify each unsafe operation? |
| [Benchmarks](PERFORMANCE.md) | Does each fixture exercise its named path? Are timing and allocation boundaries actually checked? |

The router and journaled engine are separate entry points. A review must not
assume that a guarantee from one is automatically present in the other.

## Evidence to inspect

The workspace has unit, integration, reference-model, replay, and compatibility
tests. CI is configured for formatting, check, Clippy, tests, MSRV, Loom, Miri,
and native sanitizer jobs. A configured job is not proof that a particular
revision passed. Inspect the result for the revision under review.

Use [Quick Start](../QUICKSTART.md) for local commands. Inspect the
[CI workflow](../.github/workflows/ci.yml) for feature combinations and platform
coverage. Performance and soak results have separate acceptance criteria in
[PERFORMANCE.md](PERFORMANCE.md) and [SOAK.md](SOAK.md).

Previous fixes provide useful regression targets:

- Risk digest lanes use big-endian reconstruction. The replay test pins a
  complete gateway result to a golden digest.
- Index relocation updates run outside `debug_assert!`. Required mutation
  must also happen in release builds.
- Matching preflight checks report and book capacity before applying a plan.
  Gateway reservation release still runs when a new order fails book checks.
- Event admission reserves a whole command batch. Journal pressure must drop
  the token without consuming sequence or mutating the gateway.
- Recovery compares the journal sequence with the sequence in its wire payload.

## Open review work

Independent API, unsafe-boundary, recovery-format, and operational review remain
requirements for a release candidate. Specific unresolved areas include:

- Router/session integration with the journaled engine.
- A persisted configuration manifest, including the report bound used in replay.
- Snapshot selection, retention, backup, upgrade, and rollback workflows.
- Durable event delivery or an explicit application-level acknowledgment policy.
- Completed multi-hour combined fault runs and dedicated Linux measurements.
- Session benchmark outcome assertions, allocation boundaries, and sustained
  retransmission refill. See the [known gaps](PERFORMANCE.md#session-and-recovery-window).
- Real venue adapters and vendor integration.

Replace, session recovery, journaling, snapshots, and bounded events are already
implemented. Their integration and qualification limits should not be confused
with missing components.

## Reporting a finding

Record the revision, affected path, input or seed, capacity shape, expected
behavior, and observed result. Include a small reproducer when possible.
Separate a confirmed defect from a missing feature or unqualified performance
claim. Follow [SECURITY.md](../SECURITY.md) for private security reports.
