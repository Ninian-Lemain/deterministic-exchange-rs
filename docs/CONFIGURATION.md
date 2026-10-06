# Replay configuration and checkpoint bundles

Engine snapshots remain format v1. They contain the gateway capacity shape but
not the report bound. Configured recovery therefore requires an `EngineConfig`
supplied independently of the stored bundle, and a canonical persisted copy
of the same configuration. Restore checks both before tail replay.

## Configuration v1

The encoding is UTF-8 decimal text with a final newline, one ASCII space between
fields and no other whitespace. Accounts appear in ascending ID order. For the
current formats the first line is:

```text
HFTCONFIG 1 2 1 1
```

The numbers identify configuration, wire, journal and snapshot versions. Unknown
versions fail closed. The next line contains instrument ID followed by account,
risk-order, price-level, orders-per-level and report capacities. Every capacity
must be nonzero; reports must not exceed 65,534 so event ordinals remain bounded.
The CLI's supported shape for instrument 1 is:

```text
1 2 8 4 4 4
```

Each remaining line contains one account's ID, maximum quantity, maximum
notional, maximum absolute position, maximum open orders, minimum price and
maximum price. Quantities and positions use `u64`, account IDs and open-order
counts use `u32`, prices use `i64` and notional uses `u128`, matching the domain
types. Registration rules require positive maximum quantity and open-order
count, ordered price limits, unique account IDs and account count within capacity.
Zero notional or position limits are valid and retain conservative risk refusal.

Encoding is not a Rust struct dump. Decoding re-encodes and compares exact bytes
to reject noncanonical spelling, spacing and line endings. Equal configuration
and supported formats are required for upgrade and rollback. Changing a capacity,
report bound, instrument or risk definition requires an explicitly designed
migration; this API does not silently reinterpret old state.

## Bundle v1

`RecoveryBundle::publish_new` creates a new directory with immutable members:

| Member | Contents |
| --- | --- |
| `config.v1` | Canonical configuration text |
| `snapshot.v1` | Existing canonical snapshot bytes |
| `tail.v1` | Contiguous 64-byte journal records after the snapshot, or empty |
| `COMMITTED` | ASCII `HFTBUNDLE 1` plus newline, then raw 32-byte SHA-256 digest |

The digest hashes `deterministic-exchange bundle v1` followed by a zero byte,
then configuration, snapshot and tail in that order. Each member is framed by
its byte length encoded as a 16-byte unsigned big-endian integer. The marker is
written and synchronized last. Missing, partial or altered markers are refused.
Publication failures can leave incomplete directories; recovery never treats
them as committed.

Files are synchronized before publication. Unix also synchronizes the bundle
and parent directories. Windows receives no directory durability promise from
this API. Backups verify configured recovery before publishing a new bundle.
Existing destinations are never overwritten.

SHA-256 provides integrity, not authentication. Keep independently trusted
expected configuration, immutable selected bundle input and the previous binary.
An operator explicitly selects the authoritative generation and contiguous tail;
there is no automatic newest-directory policy. Restore rebuilds indexes with
fresh queues and does not republish historical events. Venue session state and
transport resume sequences require an external authoritative logon policy.

The generic library supports application-selected shapes. The operational CLI
currently supports `2,8,4,4,4`; see [Operations](OPERATIONS.md) for commands and
the admission-close, drain, flush, backup and restore workflow.
