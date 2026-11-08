"""Serves the effects bench (#1336; build.sh builds it into out/) on http://localhost:8737/ with
the web client's production Content-Security-Policy, and appends each run's summary the page
posts to results.jsonl. With --browser it adds the CPU time of the browser's processes during the
run (`cpu_ms_per_picture`: main thread, GPU process, encoder and worker together), which the
page's own timing can't see.

    python3 dev/effects-bench/server.py [--browser chrome] [--run webgpu,webgl2] [--repeat N]

--browser chrome starts a Chrome of its own (a profile next to this file, no background
throttling) on the page and stops once the runs are done; without it, open the page in any
browser yourself.
"""
import argparse
import http.server
import json
import os
import shutil
import subprocess
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, 'out')
RESULTS = os.path.join(HERE, 'results.jsonl')
PORT = 8737
CSP = ("default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; "
       "img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; connect-src 'self'; "
       "worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
TYPES = {'.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm',
         '.json': 'application/json', '.nnef': 'application/octet-stream'}
CHROME = ['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', 'google-chrome', 'chromium']

parser = argparse.ArgumentParser()
parser.add_argument('--browser', choices=['chrome'])
parser.add_argument('--run', default='webgpu,webgl2')
parser.add_argument('--repeat', type=int, default=1)
args = parser.parse_args()
expected = len(args.run.split(',')) * args.repeat
received = {'count': 0}
done = threading.Event()
state = {'root': None, 'cpu_at_start': None}


def parse_time(text):
    """`ps` CPU time ([dd-]hh:mm:ss.ss or mm:ss.ss) in seconds."""
    days, _, rest = text.rpartition('-')
    seconds = 0.0
    for part in rest.split(':'):
        seconds = seconds * 60 + float(part)
    return seconds + (int(days) * 86400 if days else 0)


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
    def log_message(self, *_):
        pass

    def do_GET(self):
        path = self.path.split('?')[0]
        name = 'index.html' if path == '/' else path.lstrip('/')
        file = os.path.normpath(os.path.join(OUT, name))
        if not file.startswith(OUT + os.sep) or not os.path.isfile(file):
            self.send_error(404)
            return
        body = open(file, 'rb').read()
        self.send_response(200)
        self.send_header('Content-Type', TYPES.get(os.path.splitext(file)[1], 'application/octet-stream'))
        self.send_header('Content-Security-Policy', CSP)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        if self.path == '/start':
            if state['root']:
                state['cpu_at_start'] = tree_cpu_seconds(state['root'])
            self.send_response(204)
            self.end_headers()
            return
        result = json.loads(body)
        if state['root'] and state['cpu_at_start'] is not None and 'summary' in result:
            pictures = result['summary']['bench']['frames']
            spent = tree_cpu_seconds(state['root']) - state['cpu_at_start']
            result['cpu_ms_per_picture'] = round(spent * 1000 / pictures, 2)
        with open(RESULTS, 'a') as results:
            results.write(json.dumps(result) + '\n')
        print(json.dumps(result))
        self.send_response(204)
        self.end_headers()
        received['count'] += 1
        if received['count'] >= expected:
            done.set()


server = http.server.ThreadingHTTPServer(('127.0.0.1', PORT), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f'http://localhost:{PORT}/'
if args.browser == 'chrome':
    chrome = next((c for c in CHROME if shutil.which(c) or os.path.exists(c)), None)
    if chrome is None:
        raise SystemExit('no Chrome found')
    profile = os.path.join(HERE, 'profile-chrome')
    runs = ','.join([args.run] * args.repeat)
    process = subprocess.Popen([chrome, f'--user-data-dir={profile}', '--no-first-run',
                                '--no-default-browser-check', '--disable-background-timer-throttling',
                                '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
                                f'{url}?run={runs}'])
    state['root'] = process.pid
    done.wait()
    process.terminate()
else:
    print(f'serving {url} (Ctrl-C to stop); results go to {RESULTS}')
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
