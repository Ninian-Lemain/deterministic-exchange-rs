# Quick Start

## Requirements

- Rust 1.85 or newer with `rustfmt` and Clippy.
- Linux x86_64 for production tuning. Other systems are for correctness tests.

## Run the Vertical Slice

```console
cargo run --release -p hft-cli -- replay-demo
```

The demo submits one resting sell and one crossing buy, then prints the frame
count, execution-report count, and deterministic final-state digest.

## Verify

```console
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features
cargo test --doc --workspace
cargo test -p hft-spsc --features loom loom_actual_queue
cargo test -p hft-wire malformed_input_smoke_never_panics
cargo run --release -p hft-bench
python scripts/source_ratio.py
```

The benchmark executable exits nonzero if allocation or deallocation occurs
between its workload counters. Repaired session fixtures and historical invalid
results are described in [Performance evidence](docs/PERFORMANCE.md#session-and-recovery-window).
Desktop timings are not production latency evidence.

## Linux safety and fault evidence

Install nightly Rust with `miri` and `rust-src`, cargo-fuzz, Clang and its
compiler-rt sanitizer runtime before running the safety capture. The capture
fails explicitly for a missing tool or a failed check:

```console
bash scripts/linux/run_safety.sh --output target/safety-evidence --fuzz-seconds 60
cargo build --release -p hft-soak
python3 scripts/soak/run_linux.py --output target/soak-2h --seconds 7200 --steps 1000000
```

See [Soak](docs/SOAK.md) for elapsed-time and memory requirements, and
[Operations](docs/OPERATIONS.md) for configuration, checkpoint, backup and
restore commands. WSL is suitable for these development checks; dedicated
Linux latency qualification requires controlled native hardware.
