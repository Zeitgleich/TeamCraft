#!/usr/bin/env python3
"""Names the functions that ran in a page's long tasks (docs/performance.md, issue #2397).

    long-task-profile.py <perf-<phase>.cpuprofile> <perf-<phase>.long-tasks.json> [--top N]
        [--match REGEX] [--from-ms MS] [--to-ms MS]

Reads a CPU profile that `tests/e2e` `perf.rs` wrote with `PERF_PROFILE=<phase>` and the page's
long tasks beside it, and counts the samples that fell into a task over 50 ms: per function, the
time it ran itself (self) and the time it was on the stack (inclusive). Wasm frames keep their Rust
names in a debug web build (`trunk build` without `--release`). `--match` keeps only the inclusive
lines whose name matches (e.g. `teamcraft_core::`). `--names` names the main module's functions from
the build's names map where the served file has none.
"""
import argparse
import gzip
import json
import re
import shutil
import subprocess
import sys
from collections import Counter

HASH = re.compile(r'::h[0-9a-f]{16}$')
UNNAMED = re.compile(r'^wasm-function\[(\d+)\]$')
CRATE_HASH = re.compile(r'\[[0-9a-f]{16}\]')


def demangled(names):
    """Rust names as source paths, through binutils' c++filt where it is installed."""
    names = list(names)
    if not shutil.which('c++filt'):
        return {name: name for name in names}
    out = subprocess.run(['c++filt'], input='\n'.join(names), capture_output=True, text=True).stdout
    return {name: CRATE_HASH.sub('', plain) for name, plain in zip(names, out.split('\n'))}


def main_module_names(path):
    """Function index -> name of the main module (`*_bg.wasm`), the only one served without names
    in a debug build: the split modules keep theirs."""
    with gzip.open(path, 'rt') as handle:
        names = json.load(handle)
    out = {}
    for module in names['modules']:
        if not module['file'].endswith('_bg.wasm'):
            continue
        first = module['imported_functions']
        for position, function in enumerate(module['functions']):
            if function.get('name'):
                out[first + position] = function['name']
    return out


def renamed(counts, pretty):
    out = Counter()
    for name, ms in counts.items():
        out[pretty[name]] += ms
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('profile')
    parser.add_argument('tasks')
    parser.add_argument('--top', type=int, default=40)
    parser.add_argument('--match')
    parser.add_argument('--names', help='the build\'s names map (teamcraft-ui-names-<id>.json.gz, --names-out)')
    parser.add_argument('--from-ms', type=float, default=0.0, help='only long tasks starting at or after (page time)')
    parser.add_argument('--to-ms', type=float, default=float('inf'), help='only long tasks starting before (page time)')
    args = parser.parse_args()
    with open(args.profile) as handle:
        profile = json.load(handle)
    with open(args.tasks) as handle:
        beside = json.load(handle)
    tasks = sorted(
        (float(start), float(duration))
        for start, duration in beside['long_tasks']
        if args.from_ms <= float(start) < args.to_ms
    )
    # The profile's clock (microseconds) against the page's (milliseconds), both read at its end.
    offset_ms = float(beside['page_now_ms_at_end']) - profile['endTime'] / 1000.0

    wasm_names = main_module_names(args.names) if args.names else {}
    nodes = {node['id']: node for node in profile['nodes']}
    parent = {}
    for node in profile['nodes']:
        for child in node.get('children', []):
            parent[child] = node['id']

    def name(node_id):
        frame = nodes[node_id]['callFrame']
        text = frame.get('functionName') or '(anonymous)'
        unnamed = UNNAMED.match(text)
        if unnamed and int(unnamed.group(1)) in wasm_names:
            text = wasm_names[int(unnamed.group(1))]
        return HASH.sub('', text)

    stacks = {}

    def stack(node_id):
        if node_id not in stacks:
            names = []
            current = node_id
            while current is not None:
                names.append(name(current))
                current = parent.get(current)
            stacks[node_id] = names
        return stacks[node_id]

    self_time = Counter()
    inclusive = Counter()
    in_tasks = 0
    total = 0
    at_us = profile['startTime']
    task_index = 0
    previous_at = at_us
    for node_id, delta in zip(profile['samples'], profile['timeDeltas']):
        at_us += delta
        weight_ms = (at_us - previous_at) / 1000.0
        previous_at = at_us
        page_ms = at_us / 1000.0 + offset_ms
        while task_index < len(tasks) and tasks[task_index][0] + tasks[task_index][1] < page_ms:
            task_index += 1
        names = stack(node_id)
        if names[0] in ('(idle)', '(program)', '(garbage collector)', '(root)'):
            if names[0] != '(garbage collector)':
                continue
        total += weight_ms
        if task_index >= len(tasks) or tasks[task_index][0] > page_ms:
            continue
        in_tasks += weight_ms
        self_time[names[0]] += weight_ms
        for seen in set(names):
            inclusive[seen] += weight_ms

    pretty = demangled(set(self_time) | set(inclusive))
    self_time = renamed(self_time, pretty)
    inclusive = renamed(inclusive, pretty)
    long_total = sum(duration for _, duration in tasks)
    print(f'long tasks: {len(tasks)}, {long_total:.0f} ms; longest {max((d for _, d in tasks), default=0):.0f} ms')
    print(f'busy samples: {total:.0f} ms, of them in long tasks: {in_tasks:.0f} ms')
    print('\n-- self, in long tasks (ms) --')
    for function, ms in self_time.most_common(args.top):
        print(f'{ms:8.0f}  {function[:170]}')
    print('\n-- inclusive, in long tasks (ms) --')
    pattern = re.compile(args.match) if args.match else None
    shown = 0
    for function, ms in inclusive.most_common():
        if pattern and not pattern.search(function):
            continue
        print(f'{ms:8.0f}  {function[:170]}')
        shown += 1
        if shown >= args.top * 2:
            break


if __name__ == '__main__':
    sys.exit(main())
