"""Writes the run matrix of the peak measurement (issue #1005): bytes per frame, per 100 ms and per
second against the target, for H.264 (what every call sends) with `bitrateMode` unset (the
browser's default), "variable" and "constant".

    python3 make_peaks.py [output.json]
    BENCH_CHROME=/usr/bin/google-chrome python3 server.py chrome peaks.json peaks.jsonl
    python3 peaks_table.py peaks.jsonl
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
MODES = [None, 'variable', 'constant']


def run(width, height, fps, kbps, scenario, mode, frames=300, key_every=None):
    r = {'codec': 'h264', 'width': width, 'height': height, 'fps': fps, 'bitrate': kbps * 1000,
         'frames': frames, 'scenario': scenario}
    if mode:
        r['bitrateMode'] = mode
    if key_every:
        r['keyEvery'] = key_every
    return r


runs = []
# A standard share's layers (1440p30 and its half-size layer) and a 4K detail share's top layer.
for width, height, kbps, frames in [(2560, 1440, 6636, 300), (1280, 720, 1659, 300), (3840, 2160, 14930, 240)]:
    for scenario in ['typing', 'slides', 'scroll']:
        runs += [run(width, height, 30, kbps, scenario, mode, frames) for mode in MODES]
# A camera's 720p and 360p layers; `camera-cut` changes the whole scene every 3 s.
for width, height, kbps in [(1280, 720, 1500), (640, 360, 375)]:
    for scenario in ['camera', 'camera-cut']:
        runs += [run(width, height, 30, kbps, scenario, mode) for mode in MODES]
# A keyframe every 1.5 s: is it as sharp as the pictures after it under each mode?
for scenario in ['typing', 'slides']:
    runs += [run(2560, 1440, 30, 6636, scenario, mode, 300, 45) for mode in MODES]
out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, 'peaks.json')
with open(out, 'w') as f:
    json.dump(runs, f)
print(len(runs), 'runs')
