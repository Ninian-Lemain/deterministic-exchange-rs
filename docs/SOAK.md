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
| Combined service | One round per 4,096 steps, rounded up, through two journaled engines owned by one routed session. |

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
The combined scenario fills event and journal queues and retries the same
transport and command sequences after capacity returns. It checks malformed
frames, unknown routes, gaps, duplicates, heartbeat recovery and reconnects;
seeded 85/15 instrument traffic passes through session, journal, matching and
events together. Two persistence workers must observe open producers before
admission closes, then drain and flush concurrently. Full-tail configured
replay must match both checkpoints, and a second restart must consume exactly
one next command. Every round injects a write failure, a flush failure and
worker abandonment; a failed instrument must stop the shared session even when
the next command targets another instrument. Fault sinks use seven-byte short
writes in memory, not disk fault injection.

## Run

Run from the repository root:

```text
cargo run --release -p hft-soak -- --profile smoke
cargo run --release -p hft-soak -- --seed 0000000000000001 --steps 1000000
cargo run --release -p hft-soak -- --profile nightly --seed-file crates/hft-soak/seeds/v1.txt
cargo run --release -p hft-soak -- --steps 1000000 --duration-seconds 7200
```

Profiles select fixed step counts: smoke uses 10,000, nightly uses 10,000,000, and
qualification uses 100,000,000. `--steps` overrides the count. Seeds contain
exactly 16 lowercase hexadecimal digits. `--seed` and `--seed-file` are mutually
exclusive.

`--duration-seconds` repeats complete passes over all selected seeds in the same
process until at least the requested elapsed time has passed. It accepts 1 to
86,400 seconds and does not change matching or session virtual time. Every seed
pass still runs twice for repeat verification. The process completes a whole
seed set before stopping and emits a result for each verified pass. Results now
include additive `combined` counters; state and event digests include the new
coverage and are not comparable with the earlier soak workload. Engine replay
digests and snapshot compatibility bytes are unchanged.

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

## Capture Linux evidence

```text
cargo build --release -p hft-soak
python3 scripts/soak/run_linux.py --output target/soak-2h --seconds 7200 --steps 1000000
```

The output directory must be new. The script copies and hashes the binary,
records source identities, runs one sustained process, samples `/proc` resident
memory once per second and validates completed results for every retained seed.
It also compares state, events and counters across repeated seed passes and
checks that all combined faults recurred. Quarter-median RSS growth above the
larger of 2 MiB or 25% fails for investigation when at least 20 nonzero samples
exist; shorter captures exercise the tooling but cannot qualify elapsed hours.
Inspect the full time series rather than relying only on this threshold.

`host-result.json` records actual elapsed seconds, completion and memory data.
An exit code, profile name, or requested duration alone is insufficient.
Windows WSL can provide fault-soak evidence; it remains unsuitable for dedicated
Linux latency qualification. The GitHub manual soak workflow uses this capture
and retains success or failure artifacts.

## Qualification status

Two sustained WSL 2 captures passed in 7,213.34 and 7,201.41 seconds. All four
retained seeds repeatedly matched state, events and fault counters, and sampled
RSS remained bounded. The declared v0.20 fault workload is qualified. Inspect the
[result table and evidence archive](ROADMAP_PROGRESS_2026-10-06.md#sustained-qualification)
for completion records and the full memory series. Dedicated Linux performance
qualification and real disk fault testing are separate requirements.
