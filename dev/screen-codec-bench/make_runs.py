"""Writes the bench's run matrices next to this file."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def write(name, runs):
    with open(os.path.join(HERE, name), 'w') as f:
        json.dump(runs, f)
    print(name, len(runs))


VARIANTS = [
    ('h264', None, None),
    ('h264', None, 'prefer-software'),
    ('vp9', 'text', None),
    ('vp9', None, None),
    ('av1', 'text', None),
    ('av1', None, None),
]


def run(codec, hint, accel, width, height, fps, kbps, scenario, frames=150, measure=True):
    r = {'codec': codec, 'width': width, 'height': height, 'fps': fps, 'bitrate': kbps * 1000,
         'frames': frames, 'scenario': scenario}
    if hint:
        r['hint'] = hint
    if accel:
        r['accel'] = accel
    if not measure:
        r['measure'] = False
    return r


# Quality at 1440p30 (today's detail mode).
q1440 = [run(c, h, a, 2560, 1440, 30, kbps, s) for s in ['typing', 'scroll'] for kbps in [1000, 2000, 4000, 6600]
         for c, h, a in [VARIANTS[0], VARIANTS[1], VARIANTS[4], VARIANTS[5]]]
write('q1440.json', q1440)

# Quality at 4K30 (the new option) and 1080p60 motion.
q4k = [run(c, h, a, 3840, 2160, 30, kbps, s) for s in ['typing', 'scroll'] for kbps in [4000, 8000, 15000]
       for c, h, a in [VARIANTS[0], VARIANTS[1], VARIANTS[4]]]
q4k += [run(c, 'motion' if h else None, a, 1920, 1080, 60, kbps, 'scroll') for kbps in [4000, 8000]
        for c, h, a in [VARIANTS[0], VARIANTS[1], VARIANTS[4]]]
write('q4k.json', q4k)

# CPU only (no decoding, no PSNR): per resolution, rate and codec, with the page's own baseline.
cpu = []
for width, height, fps, kbps in [(1920, 1080, 30, 4000), (2560, 1440, 30, 6600), (1920, 1080, 60, 8000),
                                 (3840, 2160, 30, 15000), (2560, 1440, 60, 13000)]:
    for scenario in ['typing', 'scroll']:
        cpu.append(run('none', None, None, width, height, fps, kbps, scenario, frames=300, measure=False))
        for c, h, a in [VARIANTS[0], VARIANTS[1], VARIANTS[2], VARIANTS[4]]:
            cpu.append(run(c, h, a, width, height, fps, kbps, scenario, frames=300, measure=False))
write('cpu.json', cpu)
