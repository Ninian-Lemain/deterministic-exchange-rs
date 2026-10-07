# Release and recovery procedures

These procedures cover the current pre-v1 library and its offline reference
CLI. They are not a deployed exchange service runbook. There is no service
installer, live administrative endpoint, process supervisor, venue adapter or
automatic checkpoint-generation selector. Dedicated Linux boundary qualification
and independent review remain release gates in [Roadmap](ROADMAP.md).
WSL evidence does not satisfy dedicated Linux qualification.

## Build, install and release gates

Use a clean, identified source revision and retain the revision, toolchain,
target, build flags, binary hashes and verification output with the release.
The workspace uses Rust 1.96.0 in `rust-toolchain.toml` and declares MSRV 1.85.
From the repository root, run each command and stop on failure:

```text
cargo fmt --all --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo test --locked --doc --workspace
cargo test --locked -p hft-spsc --features loom loom_
cargo +1.85.0 check --locked --workspace --all-targets
cargo +1.85.0 test --locked --workspace
cargo build --locked --release -p hft-cli
```

The MSRV commands require that toolchain to be installed. The release binary
is `target/release/hft-cli` (`hft-cli.exe` on Windows). To install the offline
CLI into Cargo's binary directory, run:

```text
cargo install --locked --path crates/hft-cli
```

Preserve any previous binary before replacing an installation. Installation
does not start a service. Linux safety tooling and the relevant Miri, sanitizer,
fuzz, crash and soak evidence must also pass their declared workloads; consult
[Safety](SAFETY.md), [Soak](SOAK.md) and the roadmap evidence rather than treating
a local build as production qualification. Obtain independent API,
unsafe-boundary, recovery-format and operations review before a v1 candidate.

## Trusted configuration and offline rehearsal

Select expected configuration independently of the recovery bundle. A bundle's
SHA-256 marker checks integrity, not authenticity. Protect both the chosen
configuration and authoritative bundle against modification during validation
and recovery. `config-validate` checks canonical encoding and domain constraints;
it does not establish configuration provenance or prove that the CLI supports
its capacity shape.

The following is the exact configuration hardcoded by `checkpoint-demo`. For a
rehearsal, create `expected-demo.v1` independently with these UTF-8 bytes, LF line
endings and a final newline. PowerShell's default text output can produce an
incompatible encoding or CRLF, so use an editor that preserves this format.

```text
HFTCONFIG 1 2 1 1
1 2 8 4 4 4
1 100 100000 1000 8 1 1000
2 100 100000 1000 8 1 1000
```

From the repository root, use fresh destinations and run:

```text
cargo run --locked --release -p hft-cli -- config-validate expected-demo.v1
cargo run --locked --release -p hft-cli -- checkpoint-demo target/release-checkpoint-1
cargo run --locked --release -p hft-cli -- health target/release-checkpoint-1 expected-demo.v1
cargo run --locked --release -p hft-cli -- backup target/release-checkpoint-1 target/release-backup-1 expected-demo.v1
cargo run --locked --release -p hft-cli -- restore-check target/release-backup-1 expected-demo.v1
cargo run --locked --release -p hft-cli -- compatibility-check expected-demo.v1 target/release-backup-1/config.v1
```

The checkpoint and backup directories must not exist. The checkpoint's sibling
`target/release-checkpoint-1.journal` must also not exist, and its parent must
exist. `checkpoint-demo` applies one buy order to instrument 1, closes admission,
flushes the journal, drains events and publishes a bundle. Successful `health`
and `restore-check` report `next_sequence=2` and explicitly state that no service
is running. They validate a selected bundle and construct a fresh engine before
exiting. They do not probe a live process, storage device, network or venue.

The CLI restore and backup commands support capacities `2,8,4,4,4` only. Library
applications must use their actual generic capacity shape. The separate
`hft-engine` lifecycle example uses different capacities and emits a snapshot
and journal rather than a configured bundle; do not feed those files into this
CLI workflow. See [Engine](ENGINE.md) and [Configuration](CONFIGURATION.md).

## Application admission drain, shutdown and checkpoint

There is no command to stop a running service. An application embedding the
engine must implement the following lifecycle using the existing library APIs:

1. Close upstream admission and account for queued inputs by draining or
   refusing them. Retry backpressured commands unchanged only while admission
   remains open; business rejection has already consumed the sequence.
2. After the final command, call `stop_admission` on the engine/facade to close
   journal producers. Retain engine state for checkpointing.
3. Complete each persistence worker's `shutdown`. Require successful drain and
   flush, no poisoned status, and written and durable next-sequence watermarks
   equal to the gateway's next sequence. A dequeue is not a durability signal.
4. Drain event consumers before retiring storage. Document downstream handling
   of unconfirmed events: events have no durable acknowledgement or outbox.
5. Call `snapshot` only after successful persistence shutdown. For each
   instrument, create a `RecoveryBundle::checkpoint` with its configuration and
   snapshot, then `publish_new` to a new generation directory.
6. Retain the selected committed bundle, trusted configuration and binary
   identity. Restart using configured restore and fresh queue storage. Verify
   recovered instrument and next sequence before reopening admission.

The demo performs this lifecycle on one instrument with an empty journal tail.
An application must coordinate all routes, select authoritative contiguous tails
and implement generation retention. Session snapshots do not preserve venue
sessions; restart needs explicit logon and an authoritative transport resume
sequence. Restoring does not republish historical events.

## Backup, restore, upgrade and rollback

Back up an explicitly selected committed bundle with `backup` as above. It checks
recovery against the trusted expected configuration before publishing a new
bundle. Verify the backup with `restore-check` before relying on it. Retain all
four members: `config.v1`, `snapshot.v1`, `tail.v1` and `COMMITTED`. Application
backup policy must separately cover authoritative journals, session resume
records, binary identity and evidence outside the bundle.

Never overwrite an existing destination or select a directory solely because it
has the newest name. A publication failure can leave an incomplete directory.
Files are synchronized; Unix also synchronizes directories. Windows has no
directory durability promise from this API. Treat publication failure as an
incident and choose a fresh destination after preserving evidence.

For an upgrade, build and verify the candidate, then run its `config-validate`,
`compatibility-check CURRENT_CONFIG CANDIDATE_CONFIG` and
`restore-check SELECTED_BUNDLE TRUSTED_EXPECTED_CONFIG`. Compatibility currently
requires equal instrument, all capacities including reports, account risk
definitions and supported formats. It does not prove identical application
semantics across binaries; retain deterministic replay/soak evidence for the
candidate. Any replay-semantic or configuration change requires a designed
migration. Do not reinterpret old state silently.

Drain and checkpoint the application, record the cutover sequence per
instrument, then have the application's supervisor switch binaries and restore
the selected state with fresh storage. Verify sequence and session resume before
admission. No CLI command performs this switch.

For rollback, use the retained previous binary to validate compatibility and
restore the selected authoritative cutover state before reopening. If commands
were admitted after upgrade, first close admission and preserve their journals
and events. Verify that the previous binary can recover the full authoritative
state; reverting to an older snapshot alone would discard admitted commands.
If compatibility or replay verification fails, keep admission closed and design
a migration or reconciliation. There is no automatic rollback mechanism.

## Incident response

| Trigger | Immediate action and recovery gate |
| --- | --- |
| Event/journal queue full | Retain the unchanged input and restore consumer progress. No command was consumed. Monitor pressure in the embedding application. |
| Persistence failure, poisoned/abandoned worker, internal invariant failure or sequence exhaustion | Stop upstream admission and close producers. Preserve failed state and storage evidence. Do not retry a potentially applied command or resume the failed engine. |
| Missing/altered `COMMITTED`, corrupt snapshot/tail, gap or configuration mismatch | Keep admission closed. Preserve the rejected generation and error. Select another authoritative committed bundle with its contiguous tail and independently trusted expected configuration. |
| Checkpoint/backup publication failure | Preserve any visible partial output and source. Do not overwrite or treat it as committed. Validate the source and publish to a fresh destination after resolving the storage cause. |
| Application or host crash | Preserve journals and selected bundles. Determine the durable contiguous prefix and reconcile uncertain commands/events with upstream evidence before configured restore. |

Record binary/revision identity, configuration identity, per-instrument applied,
written and durable watermarks where available, errors, selected bundle/tail,
queue pressure and timestamps. `health` is an offline recovery check only;
live monitoring and evidence collection are application responsibilities.
Reopen admission only after verified recovery with fresh engine storage,
authoritative session logon and reconciliation of external event effects.
Replication, standby promotion, durable event redelivery and certified venue
protocols are unsupported; see [Operations](OPERATIONS.md) for the full boundary.
