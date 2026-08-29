#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 --cpu CPU" >&2
    exit 2
}

cpu=
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cpu) [[ $# -ge 2 ]] || usage; cpu=$2; shift 2 ;;
        *) usage ;;
    esac
done
[[ "$cpu" =~ ^[0-9]+$ ]] || usage

fail() {
    echo "qualification failed: $*" >&2
    exit 1
}

cpu_in_list() {
    local needle=$1
    local list=$2
    local item start end
    IFS=',' read -ra items <<< "$list"
    for item in "${items[@]}"; do
        if [[ "$item" == *-* ]]; then
            start=${item%-*}
            end=${item#*-}
        else
            start=$item
            end=$item
        fi
        if (( needle >= start && needle <= end )); then
            return 0
        fi
    done
    return 1
}

[[ "$(uname -s)" == "Linux" ]] || fail "Linux is required"
if [[ -f /.dockerenv ]] || grep -qaE '(docker|containerd|kubepods|lxc)' /proc/1/cgroup; then
    fail "container runs are tooling checks, not dedicated qualification"
fi
[[ -d "/sys/devices/system/cpu/cpu$cpu" ]] || fail "CPU $cpu does not exist"
command -v taskset >/dev/null 2>&1 || fail "taskset is required"
command -v perf >/dev/null 2>&1 || fail "perf is required"
command -v numactl >/dev/null 2>&1 || fail "numactl is required"

node_paths=(/sys/devices/system/cpu/cpu"$cpu"/node[0-9]*)
[[ -e "${node_paths[0]}" ]] || fail "NUMA node for CPU $cpu is unavailable"
node=${node_paths[0]##*node}
numactl --physcpubind="$cpu" --membind="$node" true >/dev/null 2>&1 || fail "CPU and memory binding failed for node $node"

siblings=$(<"/sys/devices/system/cpu/cpu$cpu/topology/thread_siblings_list")
online=$(< /sys/devices/system/cpu/online)
for sibling_path in /sys/devices/system/cpu/cpu[0-9]*; do
    sibling=${sibling_path##*cpu}
    if [[ "$sibling" != "$cpu" ]] && cpu_in_list "$sibling" "$siblings" && cpu_in_list "$sibling" "$online"; then
        fail "CPU $cpu has online SMT sibling $sibling"
    fi
done

governor_path="/sys/devices/system/cpu/cpu$cpu/cpufreq/scaling_governor"
[[ -r "$governor_path" ]] || fail "CPU governor is unavailable"
governor=$(<"$governor_path")
[[ "$governor" == "performance" ]] || fail "CPU $cpu governor is $governor, expected performance"

isolated=$(< /sys/devices/system/cpu/isolated)
[[ -n "$isolated" ]] || fail "the kernel reports no isolated CPUs"
taskset -c "$isolated" true >/dev/null 2>&1 || fail "isolated CPU list is invalid: $isolated"
cpu_in_list "$cpu" "$isolated" || fail "CPU $cpu is not isolated"

cmdline=$(< /proc/cmdline)
nohz_full=
rcu_nocbs=
for argument in $cmdline; do
    case "$argument" in
        nohz_full=*) nohz_full=${argument#nohz_full=} ;;
        rcu_nocbs=*) rcu_nocbs=${argument#rcu_nocbs=} ;;
    esac
done
[[ -n "$nohz_full" ]] || fail "nohz_full is not configured"
[[ -n "$rcu_nocbs" ]] || fail "rcu_nocbs is not configured"
cpu_in_list "$cpu" "$nohz_full" || fail "CPU $cpu is not in nohz_full=$nohz_full"
cpu_in_list "$cpu" "$rcu_nocbs" || fail "CPU $cpu is not in rcu_nocbs=$rcu_nocbs"

for affinity_path in /proc/irq/[0-9]*/effective_affinity_list; do
    [[ -r "$affinity_path" ]] || continue
    affinity=$(<"$affinity_path")
    if cpu_in_list "$cpu" "$affinity"; then
        fail "${affinity_path%/*} can route interrupts to CPU $cpu"
    fi
done

if ! taskset -c "$cpu" perf stat -e cycles,instructions -- true >/dev/null 2>&1; then
    fail "hardware perf counters are unavailable for CPU $cpu"
fi

probe=$(mktemp)
trap 'rm -f "$probe"' EXIT
taskset -c "$cpu" perf stat -x, -o "$probe" -e context-switches,cpu-migrations -- sleep 1
migrations=$(awk -F, '$3 == "cpu-migrations" { print $1 }' "$probe")
switches=$(awk -F, '$3 == "context-switches" { print $1 }' "$probe")
[[ "$migrations" == "0" ]] || fail "the pinned scheduler probe recorded $migrations CPU migrations"
[[ "$switches" =~ ^[0-9]+$ && "$switches" -le 2 ]] || fail "the pinned scheduler probe recorded $switches context switches"

echo "qualification checks passed for CPU $cpu"
