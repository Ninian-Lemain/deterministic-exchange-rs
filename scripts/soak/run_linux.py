#!/usr/bin/env python3
"""Capture one sustained Linux soak process, completion, digests and RSS evidence."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def sample(pid):
    values = {}
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        name, _, value = line.partition(':')
        if name in ('VmRSS', 'VmHWM', 'VmData', 'VmSize'):
            values[name] = int(value.split()[0]) * 1024
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=7200)
    parser.add_argument('--steps', type=int, default=1_000_000)
    parser.add_argument('--binary', type=Path, default=Path('target/release/hft-soak'))
    args = parser.parse_args()
    if sys.platform != 'linux' or not 1 <= args.seconds <= 86400 or not 0 < args.steps < 2**64 - 1:
        parser.error('Linux, 1..86400 seconds and positive bounded steps are required')
    repo = Path(__file__).resolve().parents[2]
    os.chdir(repo)
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    frozen = output / 'hft-soak'
    shutil.copy2(binary, frozen)
    sources = subprocess.check_output(['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z'], text=True).split('\0')
    manifest = {
        'schema': 'hft-soak-linux-host/1',
        'revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
        'source_status': subprocess.check_output(['git', 'status', '--porcelain'], text=True),
        'source_sha256': {name: digest(repo / name) for name in sources if name and (name.startswith('crates/') or name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'))},
        'binary_sha256': digest(frozen), 'duration_seconds': args.seconds,
        'steps_per_seed_pass': args.steps,
        'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'kernel': os.uname().release,
        'classification': 'fault_soak_only_not_latency_qualification',
        'memory_measurement': 'One sustained process sampled through /proc at one-second intervals; matching allocation assertions are separate.',
    }
    write_json(output / 'manifest.json', manifest)
    started = time.monotonic()
    memory = []
    with (output / 'results.jsonl').open('w') as stdout, (output / 'stderr.log').open('w') as stderr, (output / 'memory.jsonl').open('w') as samples:
        process = subprocess.Popen([str(frozen), '--profile', 'qualification', '--steps', str(args.steps), '--duration-seconds', str(args.seconds)], stdout=stdout, stderr=stderr)
        timed_out = False
        try:
            while process.poll() is None:
                try:
                    values = sample(process.pid)
                except FileNotFoundError:
                    values = {}
                if values:
                    values['elapsed_seconds'] = time.monotonic() - started
                    samples.write(json.dumps(values) + '\n')
                    samples.flush()
                    memory.append(values)
                if time.monotonic() - started > args.seconds + 600:
                    timed_out = True
                    process.terminate()
                    break
                time.sleep(1)
            try:
                code = process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                code = process.wait()
        except BaseException:
            process.kill()
            process.wait()
            raise
    elapsed = time.monotonic() - started
    failures = []
    if code != 0 or timed_out:
        failures.append(f'exit_code={code}, timed_out={timed_out}')
    if elapsed < args.seconds:
        failures.append('process stopped before requested duration')
    expected_seeds = [line.split('#')[0].strip() for line in (repo / 'crates/hft-soak/seeds/v1.txt').read_text().splitlines() if line.split('#')[0].strip()]
    first = {}
    counts = {}
    with (output / 'results.jsonl').open() as results:
        for line in results:
            try:
                row = json.loads(line)
                seed = row['seed']
                if seed not in expected_seeds or row.get('status') != 'passed' or row.get('steps') != args.steps or row.get('completed_steps') != args.steps:
                    raise ValueError('incomplete or unexpected result')
                combined = row['scenarios']['combined']
                rounds = (args.steps + 4095) // 4096
                expected_counts = {'rounds': rounds, 'event_pressure': rounds, 'journal_pressure': rounds, 'session_refusals': rounds * 3, 'reconnects': rounds, 'heartbeat_timeouts': rounds, 'malformed_frames': rounds, 'unknown_routes': rounds, 'recovery_checks': rounds * 2, 'shutdown_races': rounds, 'write_failures': rounds, 'flush_failures': rounds, 'abandoned_workers': rounds}
                if any(combined.get(key) != value for key, value in expected_counts.items()):
                    raise ValueError('missing combined fault coverage')
                identity = [row['state_digest'], row['event_digest'], row['scenarios']]
                if seed in first and first[seed] != identity:
                    raise ValueError('same seed diverged across sustained passes')
                first[seed] = identity
                counts[seed] = counts.get(seed, 0) + 1
            except (KeyError, ValueError) as error:
                failures.append(str(error))
                break
    if set(counts) != set(expected_seeds):
        failures.append('not all retained seeds completed')
    # A just-execed process can briefly report zero before its mappings appear.
    rss = [row['VmRSS'] for row in memory if row.get('VmRSS', 0) > 0]
    width = max(1, len(rss) // 4)
    initial = statistics.median(rss[:width]) if rss else None
    final = statistics.median(rss[-width:]) if rss else None
    if not rss:
        failures.append('no resident-memory samples')
    if len(rss) >= 20 and final - initial > max(2 * 1024 * 1024, initial * 0.25):
        failures.append('RSS growth requires investigation')
    summary = {'schema': 'hft-soak-linux-result/1', 'status': 'failed' if failures else 'passed', 'exit_code': code, 'elapsed_seconds': elapsed, 'multi_hour': elapsed >= 7200 and args.seconds >= 7200, 'seed_passes': counts, 'samples': len(memory), 'peak_sampled_rss_bytes': max(rss, default=None), 'initial_quarter_median_rss_bytes': initial, 'final_quarter_median_rss_bytes': final, 'failures': failures}
    write_json(output / 'host-result.json', summary)
    (output / 'evidence.sha256').write_text(''.join(f'{digest(path)}  {path.name}\n' for path in sorted(output.iterdir()) if path.is_file() and path.name != 'evidence.sha256'))
    print(json.dumps(summary))
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
