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
cargo test --workspace --all-features
cargo test --doc --workspace
cargo test -p hft-spsc --features loom loom_models_release_acquire_publication
cargo test -p hft-wire malformed_input_smoke_never_panics
cargo run --release -p hft-bench
python scripts/source_ratio.py
```

The benchmark executable exits nonzero if allocation or deallocation occurs
between its post-warm-up counters. Its Windows/macOS times are not production
latency evidence.
