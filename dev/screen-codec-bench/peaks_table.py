"""Prints a peak measurement result file (make_peaks.py) as a table. Ratios are against the target."""
import json
import sys

rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
print(f"{'scenario':10} {'size':9} {'target':>6} {'mode':8} {'kbit/s':>6} {'frame':>6} {'p95 fr':>6} {'100ms':>6} "
      f"{'1s':>5} {'first s':>7} {'delay ms':>8} {'>100ms':>6} {'stall':>5} {'lost':>4} {'PSNR':>5} {'key':>5} {'delta':>5}")
for r in rows:
    if 'caps' in r:
        continue
    if not r.get('supported'):
        print('unsupported', r['scenario'], r.get('bitrateMode'))
        continue
    failed = r.get('encodeError') or r.get('decodeError')
    if 'peakFrameRatio' not in r:
        print('failed', r['scenario'], r.get('bitrateMode'), failed)
        continue
    print(f"{r['scenario']:10} {str(r['width'])+'x'+str(r['height']):9} {r['bitrate']//1000:>6} "
          f"{r.get('bitrateMode') or 'default':8} {r.get('kbps', 0):>6} {r['peakFrameRatio']:>6} {r['p95FrameRatio']:>6} "
          f"{r['peak100msRatio']:>6} {r['peak1sRatio']:>5} {r['firstSecondRatio']:>7} {r['linkMaxDelayMs']:>8} "
          f"{r['linkFramesOver100ms']:>6} {r['linkStalls']:>5} {r['linkLostFrames']:>4} {r.get('psnrAvg', 0):>5} "
          f"{r.get('psnrKeyAvg') or '-':>5} {r.get('psnrDeltaAvg') or '-':>5}"
          + (f" ERR {failed}" if failed else ''))
