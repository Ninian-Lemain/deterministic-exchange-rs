# Engine

`hft-engine` owns one instrument's gateway, event producer, and journal writer.
Its public API prevents direct mutation of the gateway or writer. Persistence
and event consumption run separately. `RoutedEngine` composes a fixed array of
these engines; `SessionEngine` owns transport admission across the routes.
The original router `MatchingShard` remains a separate queue-based entry point.

## Build and process commands

`EngineBuilder` validates nonzero engine capacities, account registration, and
risk limits. `EngineStorage` provides the queues and validates their capacity.
Building requires `BATCH >= REPORTS + 2` so one event batch can hold every trade,
one terminal result, and one top-of-book update. Event ordinals also require
`REPORTS <= 65_534`.

A successful build attaches both queues at the gateway's next sequence and uses
the storage for one engine lifetime. Restart requires fresh storage.
`EngineParts` returns the engine, event consumer, and journal reader. Create the
persistence worker before accepting commands. The consumers can move to separate
threads.

Both processing methods reserve event capacity, enqueue the command, then apply
it. `process_frame` parses once and journals the original bytes.
`process_command` encodes once after admission checks. Neither method calls the
storage sink.

| Result | Caller action |
| --- | --- |
| Full event or journal queue | Retain the command and retry unchanged after capacity returns. Nothing was consumed. |
| Parse error, sequence mismatch, or unknown instrument | Correct the input. Nothing was consumed. |
| `Ok(())` | Read the event batch for acceptance or business rejection. The sequence was consumed. |
| Persistence failure, sequence exhaustion, or internal invariant/apply failure | Stop admission and recover through a new engine. |

A successful return reports application, not order acceptance or journal
durability. Observed persistence failure closes admission, but can race a command
already in flight. Dropping the journal reader before clean shutdown poisons
admission. Fatal failures have no in-place resume.

## Shutdown and restart

1. Stop upstream admission. Drain or refuse any inputs still waiting upstream.
2. Call `stop_admission` after the last command. This closes the journal producer.
   Dropping the engine also closes it, but loses access to the state for a snapshot.
3. Complete `PersistenceWorker::shutdown` to drain and flush the journal.
   `health` exposes written and durable progress separately.
4. Drain events before retiring their queue storage. Event delivery has no durable
   acknowledgment or outbox in this API.
5. Call `snapshot` after successful persistence shutdown. It requires both journal
   watermarks to equal the gateway's next sequence. Encoding allocates on this
   cold path. Publish the result with `persist_snapshot_new` at a new path.
6. Restore an authoritative snapshot and contiguous journal tail through
   `EngineBuilder::restore`, then build with fresh storage.

Restore checks the gateway capacity shape and expected instrument. Snapshot v1
does not store or check `REPORTS`, so tail replay must use the original report
bound. `restore_configured` checks a canonical `EngineConfig` sidecar against
an independently supplied expected configuration, including `REPORTS`, account
risk definitions and supported formats. These checks precede tail replay.
Legacy `restore` retains its caller-assured report-bound contract. Restore does
not republish historical events.

The lifecycle example processes one command, closes admission, flushes a journal
file, and publishes a snapshot. Run it with an output directory that does not
already exist:

```text
cargo run --release -p hft-engine --example lifecycle -- target/engine-example
```

The example runs persistence on the caller thread after admission stops. Lifecycle
tests also run admission, persistence, and event consumption on three threads and
verify that the last command is drained after producer closure.

## Compatibility and open work

The crate uses workspace version 0.19.0 and MSRV 1.85. Its Rust API is pre-v1,
with no crate-specific features or new third-party dependencies. Wire records,
64-byte journal records, and snapshot v1 bytes are unchanged. In-memory Rust
layouts are not serialization formats.

`RoutedEngine::try_new` validates each engine against its instrument route.
Application remains synchronous and per-instrument sequences are independent.
The event and persistence consumers can run on separate threads. No mutable
gateway, journal writer or child engine is exposed by the facades.

`SessionEngine` checks a caller-supplied transport sequence separately from the
command's instrument sequence. Session state, deadlines and sequence commit only
after successful journaled application. Queue pressure permits an unchanged
retry. Business rejection consumes both sequences. Any observed failed shard
closes admission across the session; persistence can still race an in-flight
command. Explicit session failure and terminal timeout also close producers.
Restart requires fresh storage and explicit logon with the authoritative
transport resume sequence; engine snapshots do not persist a venue session.

The [configuration format](CONFIGURATION.md) specifies persisted bytes and
bundle integrity. `EngineConfig` version 1 is canonical decimal UTF-8 with ascending accounts and
a final newline. Its header binds wire, journal and snapshot versions; capacity
fields include the report bound. Compatible upgrade and rollback require equal
configuration and supported formats. Wire, snapshot v1 and journal bytes are
unchanged. Rust APIs remain pre-v1 and are not a stable ABI.

`RecoveryBundle` publishes configuration, snapshot, tail and an integrity marker
at a new directory. Readers reject incomplete or altered bundles. The marker
provides SHA-256 integrity, not authentication. Use trusted expected configuration
and immutable authoritative input. Automatic generation selection and retention,
durable event delivery and independent API review remain open. The retained
[engine overhead measurements](PERFORMANCE.md#engine-boundary)
cover desktop execution. Linux production latency remains unqualified.
