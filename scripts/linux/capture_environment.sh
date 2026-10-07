#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
    echo "environment capture requires Linux" >&2
    exit 1
fi

output=${1:-/dev/stdout}
mkdir -p "$(dirname "$output")"

read_file() {
    local path=$1
    if [[ -r "$path" ]]; then
        tr '\n' ' ' < "$path" | sed 's/[[:space:]]\+/ /g; s/ $//'
    else
        printf 'unavailable'
    fi
}

command_output() {
    if command -v "$1" >/dev/null 2>&1; then
        "$@" 2>&1 | tr '\n' ' ' | sed 's/[[:space:]]\+/ /g; s/ $//'
    else
        printf 'unavailable'
    fi
}

{
    printf 'captured_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'hostname=%s\n' "$(hostname)"
    printf 'container=%s\n' "$(if [[ -f /.dockerenv ]] || grep -qaE '(docker|containerd|kubepods|lxc)' /proc/1/cgroup; then echo yes; else echo no; fi)"
    printf 'kernel=%s\n' "$(uname -srvm)"
    printf 'command_line=%s\n' "$(read_file /proc/cmdline)"
    printf 'cpu=%s\n' "$(command_output lscpu)"
    printf 'cpu_flags=%s\n' "$(awk -F ': ' '/^(flags|Features)[[:space:]]*:/{print $2; exit}' /proc/cpuinfo)"
    printf 'microcode=%s\n' "$(awk -F ': ' '/^microcode[[:space:]]*:/{print $2; exit}' /proc/cpuinfo || true)"
    printf 'numa=%s\n' "$(command_output numactl --hardware)"
    printf 'memory=%s\n' "$(command_output free -b)"
    printf 'page_size=%s\n' "$(getconf PAGE_SIZE)"
    printf 'huge_pages=%s\n' "$(grep -E '^(HugePages|Hugepagesize|Hugetlb):' /proc/meminfo | tr '\n' ' ')"
    printf 'governors=%s\n' "$(for path in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do [[ -r "$path" ]] && printf '%s=%s ' "${path%/*}" "$(<"$path")"; done)"
    printf 'frequencies=%s\n' "$(for path in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq; do [[ -r "$path" ]] && printf '%s=%s ' "${path%/*}" "$(<"$path")"; done)"
    printf 'turbo_intel=%s\n' "$(read_file /sys/devices/system/cpu/intel_pstate/no_turbo)"
    printf 'boost=%s\n' "$(read_file /sys/devices/system/cpu/cpufreq/boost)"
    printf 'isolated_cpus=%s\n' "$(read_file /sys/devices/system/cpu/isolated)"
    printf 'nohz_full=%s\n' "$(read_file /sys/devices/system/cpu/nohz_full)"
    printf 'online_cpus=%s\n' "$(read_file /sys/devices/system/cpu/online)"
    printf 'default_irq_affinity=%s\n' "$(read_file /proc/irq/default_smp_affinity)"
    printf 'irq_affinity=%s\n' "$(for path in /proc/irq/[0-9]*/effective_affinity_list; do [[ -r "$path" ]] && printf '%s=%s ' "${path%/*}" "$(<"$path")"; done)"
    printf 'perf_event_paranoid=%s\n' "$(read_file /proc/sys/kernel/perf_event_paranoid)"
    printf 'mitigations=%s\n' "$(for path in /sys/devices/system/cpu/vulnerabilities/*; do [[ -r "$path" ]] && printf '%s=%s ' "${path##*/}" "$(<"$path")"; done)"
    printf 'rustc=%s\n' "$(command_output rustc -vV)"
    printf 'cargo=%s\n' "$(command_output cargo -V)"
    printf 'rustflags=%s\n' "${RUSTFLAGS:-unset}"
    printf 'target=%s\n' "$(rustc -vV | awk -F ': ' '/^host:/{print $2}')"
    printf 'linker=%s\n' "$(command_output sh -c 'cc -print-prog-name=ld | xargs --no-run-if-empty -- ld --version')"
    printf 'clang=%s\n' "$(command_output clang --version)"
    printf 'cc=%s\n' "$(command_output cc --version)"
    printf 'perf=%s\n' "$(command_output perf version)"
} > "$output"
