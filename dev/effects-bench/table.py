"""Prints the effects bench's results (results.jsonl, or the files given) as a Markdown table:
main-thread milliseconds per picture (mean, p95) and per step (mean), the worker's segmentation
and what the encoder took (frames given, skipped for a full queue, its first chunk's latency).

    python3 dev/effects-bench/table.py [results.jsonl ...]
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
paths = sys.argv[1:] or [os.path.join(HERE, 'results.jsonl')]


def browser(user_agent):
    for name, pattern in [('Edge', r'Edg/(\d+)'), ('Firefox', r'Firefox/(\d+)'), ('Chrome', r'Chrome/(\d+)'),
                          ('Safari', r'Version/([\d.]+) .*Safari')]:
        match = re.search(pattern, user_agent)
        if match:
            return f'{name} {match.group(1)}'
    return user_agent[:40]


print('| Browser | Compositing | Total mean / p95 | Picture | Model input | Composite | Frame | Segmentation (worker) | Encoder given / skipped | First chunk | Browser CPU |')
print('|---|---|---|---|---|---|---|---|---|---|---|')
for path in paths:
    for line in open(path):
        result = json.loads(line)
        name = browser(result.get('userAgent', ''))
        if 'summary' not in result:
            print(f"| {name} | {result.get('compositing')} | failed: {result.get('error')} | | | | | | | | |")
            continue
        bench, effects = result['summary']['bench'], result['summary']['effects']
        stages = effects['stages']
        mean = lambda stage: f"{stages[stage].get('mean', 0):.1f}"
        segmentation = effects['segmentation']
        encoder = effects['encoder']
        cpu = f"{result['cpu_ms_per_picture']:.1f} ms" if 'cpu_ms_per_picture' in result else ''
        print(f"| {name} | {effects['compositing']} | {mean('total')} / {stages['total'].get('p95', 0):.1f} ms"
              f" | {mean('picture')} | {mean('model_input')} | {mean('composite')} | {mean('frame')}"
              f" | {segmentation.get('mean', 0):.1f} ms, {segmentation['count']} of {effects['pictures']}"
              f" | {encoder['given']} / {encoder['skipped_queue_full']} | {bench.get('first_chunk_ms', -1):.0f} ms"
              f" | {cpu} |")
