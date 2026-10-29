"""Serves bench.html, launches a browser on it and records results with the browser's CPU time.

The screen-content codec bench of issue #565 (docs/performance.md, "Screen share: profiles and
codecs"). The page draws an editor-like screen of text, encodes it with WebCodecs per run, decodes
it again and reports the bitrate, the luma PSNR and the encode latency; this server adds the CPU
time of the browser's processes during each run.

    python3 make_runs.py                      # writes q1440.json, q4k.json and cpu.json
    python3 server.py chrome q1440.json q1440.jsonl
    python3 table.py q1440.jsonl
    python3 server.py firefox - caps.jsonl caps   # only what the browser encodes and decodes

usage: server.py <browser: chrome|chrome-gui|firefox> <runs.json|-> <results.jsonl> [mode]
The browser runs headless with a profile of its own next to this file (profile-<browser>/).
"""
import http.server
import json
import os
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
browser, runs_path, results_path = sys.argv[1], sys.argv[2], sys.argv[3]
mode = sys.argv[4] if len(sys.argv) > 4 else 'bench'
runs = json.load(open(runs_path)) if runs_path != '-' else []
profile = os.path.join(HERE, f'profile-{browser}')
os.makedirs(profile, exist_ok=True)
state = {'root': None, 'cpu_at_start': None, 'done': threading.Event()}


def parse_time(text):
    parts = text.replace('-', ':').split(':')
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + float(part)
    if text.count('-'):
        days, rest = text.split('-')
        seconds = int(days) * 86400 + parse_time(rest)
    return seconds


def tree_cpu_seconds(root):
    out = subprocess.run(['ps', '-axo', 'pid=,ppid=,time='], capture_output=True, text=True).stdout
    children, cpu = {}, {}
    for line in out.splitlines():
        pid, ppid, cputime = line.split()
        children.setdefault(int(ppid), []).append(int(pid))
        cpu[int(pid)] = parse_time(cputime)
    total, stack = 0.0, [root]
    while stack:
        pid = stack.pop()
        total += cpu.get(pid, 0.0)
        stack.extend(children.get(pid, []))
    return total


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, body, content_type='application/json'):
        data = body if isinstance(body, bytes) else body.encode()
        self.send_response(200)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.startswith('/bench.html'):
            self.reply(open(os.path.join(HERE, 'bench.html'), 'rb').read(), 'text/html')
        elif self.path == '/runs':
            self.reply(json.dumps(runs))
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])) or b'{}')
        if self.path == '/phase':
            now_cpu = tree_cpu_seconds(state['root'])
            if body['state'] == 'start':
                state['cpu_at_start'] = (now_cpu, time.time())
            else:
                cpu0, wall0 = state['cpu_at_start']
                result = body['result']
                frames = result.get('frames', 1)
                result['cpuSeconds'] = round(now_cpu - cpu0, 2)
                result['cpuMsPerFrame'] = round((now_cpu - cpu0) * 1000 / frames, 2)
                result['wallSeconds'] = round(time.time() - wall0, 2)
                with open(results_path, 'a') as f:
                    f.write(json.dumps(result) + '\n')
                print(json.dumps(result), flush=True)
        elif self.path == '/caps':
            with open(results_path, 'a') as f:
                f.write(json.dumps({'caps': body}) + '\n')
            print('caps', json.dumps(body), flush=True)
        elif self.path == '/error':
            print('error', body, flush=True)
            state['done'].set()
        elif self.path == '/done':
            state['done'].set()
        self.reply('{}')


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
port = server.server_address[1]
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f'http://127.0.0.1:{port}/bench.html?mode={mode}'
if browser == 'chrome':
    command = ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '--headless=new',
               f'--user-data-dir={profile}', '--no-first-run', '--no-default-browser-check', url]
elif browser == 'chrome-gui':
    command = ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
               f'--user-data-dir={profile}', '--no-first-run', '--no-default-browser-check', url]
else:
    command = ['/Applications/Firefox.app/Contents/MacOS/firefox', '--headless', '--no-remote',
               '--profile', profile, url]
process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
state['root'] = process.pid
finished = state['done'].wait(timeout=1800)
process.terminate()
try:
    process.wait(timeout=10)
except subprocess.TimeoutExpired:
    process.kill()
print('finished' if finished else 'timed out', flush=True)
