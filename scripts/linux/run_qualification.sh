#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 --cpu CPU --output DIRECTORY [--runs COUNT (1..1000)] [-- COMMAND ...]" >&2
    exit 2
}

cpu=
output=
runs=10
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cpu) [[ $# -ge 2 ]] || usage; cpu=$2; shift 2 ;;
        --output) [[ $# -ge 2 ]] || usage; output=$2; shift 2 ;;
        --runs) [[ $# -ge 2 ]] || usage; runs=$2; shift 2 ;;
        --) shift; break ;;
        *) usage ;;
    esac
done
[[ "$cpu" =~ ^[0-9]+$ && -n "$output" ]] || usage
[[ "$runs" =~ ^[1-9][0-9]{0,3}$ ]] || usage
(( runs <= 1000 )) || usage

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
"$script_dir/check_qualification.sh" --cpu "$cpu"
for tool in git sha256sum realpath; do
    command -v "$tool" >/dev/null 2>&1 || { echo "$tool is required" >&2; exit 1; }
done
[[ -x /usr/bin/time ]] || { echo "/usr/bin/time is required for independent timing" >&2; exit 1; }
source_root=$(git rev-parse --show-toplevel)
source_revision=$(git rev-parse HEAD)
source_status=$(git status --porcelain=v1 --untracked-files=all)

export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=native"

if [[ $# -eq 0 ]]; then
    target_dir=${CARGO_TARGET_DIR:-target}
    cargo build --locked --release -p hft-bench --target-dir "$target_dir"
    command=("$target_dir/release/hft-bench")
    workload=full_suite
else
    command=("$@")
    workload=custom
fi
executable=$(type -P -- "${command[0]}") || { echo "executable not found: ${command[0]}" >&2; exit 1; }
executable=$(realpath -e -- "$executable")
[[ -f "$executable" && -x "$executable" ]] || { echo "not an executable file: $executable" >&2; exit 1; }
command[0]=$executable

run_dir="$output/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$output"
mkdir "$run_dir"
printf '%s\n' "$source_status" > "$run_dir/source-status.txt"
git diff --binary HEAD -- > "$run_dir/source.diff"
# Hash the current build inputs and workload fixtures, including untracked files.
while IFS= read -r -d '' source_file; do
    if [[ -f "$source_root/$source_file" ]]; then
        (cd "$source_root" && sha256sum -- "$source_file")
    else
        printf 'missing  %s\n' "$source_file"
    fi
done < <(git -C "$source_root" ls-files -z --cached --others --exclude-standard -- Cargo.toml Cargo.lock rust-toolchain.toml .cargo crates scripts tests) > "$run_dir/source-files.sha256"
sha256sum -- "$executable" > "$run_dir/executable.sha256"
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
    printf 'source_root=%s\n' "$source_root"
    printf 'source_revision=%s\n' "$source_revision"
    printf 'source_status=%s\n' "$(if [[ -z "$source_status" ]]; then echo clean; else echo dirty; fi)"
    printf 'source_identity=source-files.sha256_and_source.diff\n'
    printf 'workload_identity=command.txt_and_source-files.sha256\n'
    printf 'executable=%s\n' "$executable"
    printf 'executable_identity=executable.sha256\n'
    printf 'custom_command_dependencies=caller_must_record_external_inputs_and_interpreter_children\n'
    printf 'plain_runs=%s\n' "$runs"
    printf 'discarded_warmup_runs=1\n'
    printf 'plain_output=run-NNN.jsonl\n'
    printf 'timing_columns=run,wall_seconds,user_seconds,system_seconds,max_rss_kib\n'
    printf 'perf_stat_output=benchmark.jsonl_instrumented_excluded_from_plain_runs\n'
    printf 'perf_record_output=perf-record.stdout_instrumented_excluded_from_plain_runs\n'
    printf 'suite_config=%s\n' "$(if [[ "$workload" == full_suite ]]; then echo full; else echo caller_defined; fi)"
    printf 'build_provenance=%s\n' "$(if [[ "$workload" == full_suite ]]; then echo runner_cargo_release; else echo caller_supplied; fi)"
    printf 'repository_release_lto=fat\n'
    printf 'repository_release_codegen_units=1\n'
    printf 'repository_release_panic=abort\n'
    printf 'seeds=recorded_by_benchmark_output_or_source_fixture\n'
    printf 'sample_count=recorded_by_benchmark_output\n'
    printf 'capacity=recorded_by_benchmark_output\n'
    printf 'batch_size=recorded_by_benchmark_output\n'
} > "$run_dir/run.txt"

numactl --physcpubind="$cpu" --membind="$node" "${command[@]}" > "$run_dir/warmup.jsonl" 2> "$run_dir/warmup.stderr"
printf 'run,wall_seconds,user_seconds,system_seconds,max_rss_kib\n' > "$run_dir/timings.csv"
for ((run = 1; run <= runs; run++)); do
    printf -v prefix 'run-%03d' "$run"
    LC_NUMERIC=C /usr/bin/time -f '%e,%U,%S,%M' -o "$run_dir/$prefix.time" \
        numactl --physcpubind="$cpu" --membind="$node" "${command[@]}" \
        > "$run_dir/$prefix.jsonl" 2> "$run_dir/$prefix.stderr"
    printf '%s,' "$run" >> "$run_dir/timings.csv"
    cat "$run_dir/$prefix.time" >> "$run_dir/timings.csv"
done

events=cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-loads,L1-dcache-load-misses,LLC-loads,LLC-load-misses,context-switches,cpu-migrations,page-faults
numactl --physcpubind="$cpu" --membind="$node" perf stat -x, -e "$events" -o "$run_dir/perf-stat.csv" -- "${command[@]}" > "$run_dir/benchmark.jsonl" 2> "$run_dir/benchmark.stderr"
numactl --physcpubind="$cpu" --membind="$node" perf record -o "$run_dir/perf.data" -e cycles:u --call-graph dwarf -- "${command[@]}" > "$run_dir/perf-record.stdout" 2> "$run_dir/perf-record.stderr"
# Detect replacement of the executable while collecting evidence.
sha256sum --check "$run_dir/executable.sha256" >/dev/null
find "$run_dir" -maxdepth 1 -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > "$run_dir/SHA256SUMS"
echo "$run_dir"
