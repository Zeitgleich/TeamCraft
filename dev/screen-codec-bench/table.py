"""Prints a bench result file as a table."""
import json
import sys

rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
print(f"{'scenario':8} {'size':10} {'fps':>3} {'target':>7} {'codec':5} {'hint':6} {'accel':5} "
      f"{'kbit/s':>7} {'PSNR':>6} {'min':>6} {'key KB':>6} {'lat ms':>6} {'p95':>6} {'CPU ms/f':>8} {'wall s':>6}")
for r in rows:
    if 'caps' in r:
        continue
    if not r.get('supported'):
        print('unsupported', r)
        continue
    accel = {'prefer-software': 'sw', 'prefer-hardware': 'hw'}.get(r.get('accel'), '-')
    print(f"{r['scenario']:8} {str(r['width'])+'x'+str(r['height']):10} {r['fps']:>3} {r['bitrate']//1000:>7} "
          f"{r['codec']:5} {r.get('hint') or '-':6} {accel:5} {r.get('kbps', 0):>7} {r.get('psnrAvg', 0):>6} "
          f"{r.get('psnrMin', 0):>6} {r.get('firstKeyframeKB', 0):>6} {r.get('latencyAvg', 0):>6} "
          f"{r.get('latencyP95', 0):>6} {r.get('cpuMsPerFrame', 0):>8} {r.get('wallSeconds', 0):>6}"
          + (f" ERR {r.get('encodeError') or r.get('decodeError')}" if r.get('encodeError') or r.get('decodeError') else ''))
