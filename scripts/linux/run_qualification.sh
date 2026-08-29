#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 --cpu CPU --output DIRECTORY [-- COMMAND ...]" >&2
    exit 2
}

cpu=
output=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cpu) [[ $# -ge 2 ]] || usage; cpu=$2; shift 2 ;;
        --output) [[ $# -ge 2 ]] || usage; output=$2; shift 2 ;;
        --) shift; break ;;
        *) usage ;;
    esac
done
[[ "$cpu" =~ ^[0-9]+$ && -n "$output" ]] || usage

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
"$script_dir/check_qualification.sh" --cpu "$cpu"

export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=native"

if [[ $# -eq 0 ]]; then
    cargo build --release -p hft-bench
    command=(target/release/hft-bench)
    workload=full_suite
else
    command=("$@")
    workload=custom
fi

run_dir="$output/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$run_dir"
"$script_dir/capture_environment.sh" "$run_dir/environment.txt"
node_paths=(/sys/devices/system/cpu/cpu"$cpu"/node[0-9]*)
node=${node_paths[0]##*node}
printf '%q ' "${command[@]}" > "$run_dir/command.txt"
printf '\n' >> "$run_dir/command.txt"
{
    printf 'classification=dedicated_linux_candidate\n'
    printf 'cpu=%s\n' "$cpu"
    printf 'numa_node=%s\n' "$node"
    printf 'workload=%s\n' "$workload"
    printf 'suite_config=%s\n' "$(if [[ "$workload" == full_suite ]]; then echo full; else echo caller_defined; fi)"
    printf 'release_lto=fat\n'
    printf 'release_codegen_units=1\n'
    printf 'release_panic=abort\n'
    printf 'seeds=recorded_by_benchmark_output_or_source_fixture\n'
    printf 'sample_count=recorded_by_benchmark_output\n'
    printf 'capacity=recorded_by_benchmark_output\n'
    printf 'batch_size=recorded_by_benchmark_output\n'
} > "$run_dir/run.txt"

events=cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-loads,L1-dcache-load-misses,LLC-loads,LLC-load-misses,context-switches,cpu-migrations,page-faults
numactl --physcpubind="$cpu" --membind="$node" perf stat -x, -e "$events" -o "$run_dir/perf-stat.csv" -- "${command[@]}" > "$run_dir/benchmark.jsonl" 2> "$run_dir/benchmark.stderr"
numactl --physcpubind="$cpu" --membind="$node" perf record -o "$run_dir/perf.data" -e cycles:u --call-graph dwarf -- "${command[@]}" > "$run_dir/perf-record.stdout" 2> "$run_dir/perf-record.stderr"
sha256sum "$run_dir"/* > "$run_dir/SHA256SUMS"
echo "$run_dir"
