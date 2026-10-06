#!/usr/bin/env bash
# Independent of latency qualification: requires already installed tools.
set -euo pipefail

usage() {
    echo "usage: $0 --output DIRECTORY [--fuzz-seconds SECONDS]" >&2
    exit 2
}

output=
fuzz_seconds=30
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output) [[ $# -ge 2 ]] || usage; output=$2; shift 2 ;;
        --fuzz-seconds) [[ $# -ge 2 ]] || usage; fuzz_seconds=$2; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$output" && "$fuzz_seconds" =~ ^[1-9][0-9]*$ ]] || usage
[[ $(uname -s) == Linux ]] || { echo 'Linux is required' >&2; exit 2; }
[[ $(uname -m) == x86_64 ]] || { echo 'x86_64 Linux is required for the native ASan target' >&2; exit 2; }
for tool in cargo rustc rustup clang git grep mktemp sha256sum; do
    command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 2; }
done

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd "$script_dir/../.." && pwd)
mkdir -p "$output"
output=$(cd "$output" && pwd)
run_dir=$(mktemp -d "$output/safety-$(date -u +%Y%m%dT%H%M%SZ)-XXXXXX")
cd "$repo_dir"
printf 'status=running\nfuzz_seconds=%s\n' "$fuzz_seconds" > "$run_dir/status.txt"
trap 'result=$?; printf "status=%s\nexit_code=%s\n" "$(if [[ $result == 0 ]]; then echo passed; else echo failed; fi)" "$result" >> "$run_dir/status.txt"; echo "$run_dir"' EXIT

run_check() {
    local name=$1
    shift
    printf '%q ' "$@" >> "$run_dir/commands.txt"
    printf '\n' >> "$run_dir/commands.txt"
    echo "Running $name"
    if "$@" > "$run_dir/$name.log" 2>&1; then
        printf '%s=passed\n' "$name" >> "$run_dir/status.txt"
    else
        local result=$?
        printf '%s=failed\n' "$name" >> "$run_dir/status.txt"
        cat "$run_dir/$name.log" >&2
        return "$result"
    fi
}

# Fail explicitly when a prerequisite is missing; do not skip coverage or install.
run_check miri_available cargo +nightly miri --version
run_check fuzz_available cargo +nightly fuzz --version
rustup component list --toolchain nightly --installed > "$run_dir/components.txt"
grep -q '^rust-src' "$run_dir/components.txt" || { echo 'nightly rust-src is required' >&2; exit 2; }
rustc -Vv > "$run_dir/rustc.txt"
rustc +nightly -Vv > "$run_dir/nightly.txt"
clang --version > "$run_dir/clang.txt"
git rev-parse HEAD > "$run_dir/commit.txt"
git diff --stat > "$run_dir/worktree.txt"

run_check loom env RUSTFLAGS= CARGO_TARGET_DIR=target/safety-loom cargo test --locked -p hft-spsc --features loom loom_actual_queue
run_check miri env RUSTFLAGS= CARGO_TARGET_DIR=target/safety-miri cargo +nightly miri test --locked -p hft-wire -p hft-risk -p hft-book -p hft-spsc
run_check asan env CC=clang CFLAGS='-fsanitize=address -fno-omit-frame-pointer' RUSTFLAGS='-Zsanitizer=address' CARGO_TARGET_DIR=target/safety-asan cargo +nightly test --locked -Zbuild-std --target x86_64-unknown-linux-gnu -p hft-ffi --features vendor-sdk
run_check ubsan env CC=clang CFLAGS='-fsanitize=undefined -fno-sanitize-recover=all' RUSTFLAGS='-C linker=clang -C link-args=-fsanitize=undefined' CARGO_TARGET_DIR=target/safety-ubsan cargo +nightly test --locked -p hft-ffi --features vendor-sdk
run_check malformed cargo test --locked -p hft-wire malformed_input_smoke_never_panics
run_check fuzz env RUSTFLAGS= cargo +nightly fuzz run parse_message -- -max_total_time="$fuzz_seconds" -timeout=10

# A bounded successful fuzz run and all four safety mechanisms are required.
sha256sum "$run_dir"/*.log > "$run_dir/SHA256SUMS"
