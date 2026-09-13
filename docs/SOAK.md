# Fault and soak runs

The `hft-soak` CLI runs each seed twice and compares state fingerprints, event
fingerprints, and scenario counters. It stops at the first failure and reports
the seed, phase, error, and replay command. Without a seed option it uses the
retained roots in [seeds/v1.txt](../crates/hft-soak/seeds/v1.txt).

## Coverage

Each pass runs the following scenarios separately:

| Scenario | Work per pass |
| --- | --- |
| Routed churn | Declared steps across four shards with 85/10/4/1 routing weights. |
| Session faults | One reconnect, gap, duplicate, timeout, and retransmit cycle per 64 steps, rounded up. |
| Recovery | Declared commands, with snapshot and tail recovery every 64 commands and at the end. |
| Journal faults | One fixture set for saturation, short writes, shutdown, crash cuts, and worker write/flush failures. |
| Capacity | One fixture set for account, order, level, report, retransmit, command queue, and event queue exhaustion. |

Routed cancels and replaces target live orders. Command and event pressure recur
every 256 commands when at least nine commands remain. Each processed command
must produce one terminal event. Checks compare event payloads with the live
order table and verify shard isolation. This path uses router `MatchingShard`,
not the journaled `hft-engine`.

Recovery installs the restored gateway at every checkpoint. Subsequent outcomes
and reports must match an uninterrupted gateway, and the full logical state must
match at each checkpoint. The retained journal tail is limited to 4,096 bytes.

Journal faults use an in-memory sink and a separate command stream. They do not
exercise a real disk. Concurrent producer closure tests live in `hft-journal`.
The soak runner does not yet cover a combined service shutdown with routed
traffic, persistence failures, sessions, and recovery.

## Run

Run from the repository root:

```text
cargo run --release -p hft-soak -- --profile smoke
cargo run --release -p hft-soak -- --seed 0000000000000001 --steps 1000000
cargo run --release -p hft-soak -- --profile nightly --seed-file crates/hft-soak/seeds/v1.txt
```

Profiles select fixed step counts: smoke uses 10,000, nightly uses 10,000,000, and
qualification uses 100,000,000. `--steps` overrides the count. Seeds contain
exactly 16 lowercase hexadecimal digits. `--seed` and `--seed-file` are mutually
exclusive.

The CLI writes one JSON result per seed after repeat verification. Scenario
failures produce an error record and exit code 1. Invalid arguments and setup
failures use exit code 2. The runner does not measure memory, so its
`peak_rss_bytes` field is null.

## Capture Windows evidence

Build the binary, then use the capture script with a new output directory:

```powershell
cargo build --release -p hft-soak
./scripts/soak/run_windows.ps1 -OutputDirectory target/soak-run -Seed 0000000000000001 -Steps 100000000
```

The directory receives the result, stderr, revision and working-tree status,
source and binary hashes, environment, elapsed time, and one-second memory
samples. The script checks the exit code and verifies that the result matches
the requested seed and step count.

Working set measures resident memory. Private bytes measure committed private
memory. Both include the harness and allocator. They do not measure hot-path
allocation counts or latency.

## Qualification status

There is no completed qualifying multi-hour soak result. A profile name and step
count do not establish elapsed hours. v0.20 remains open until multi-hour evidence
and the remaining combined fault coverage are recorded. Dedicated Linux
performance qualification is a separate requirement.
