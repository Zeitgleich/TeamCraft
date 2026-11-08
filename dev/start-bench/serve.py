#!/usr/bin/env python3
"""Serves a release build of the web client for timing its start (docs/performance.md, "Web start
on phones"), like teamcraft-server's web.rs does: files named with a hash are cached for good,
everything else revalidated, unknown paths get index.html, gzip by Accept-Encoding. The bench page
(index.html and the seed, dev/start-bench) is under /__bench/. There is no server behind it:
/api/ requests are refused (`--api=drop`), or answered late with a 503 (`--api=delay:<ms>`, a slow
link). POST /__timing appends the body to a file, for Safari in the iOS Simulator, whose console
nothing can read.

    serve.py <dist> <bench_dir> <port> <results_file> [--api=drop|delay:<ms>|503]
"""
import gzip
import http.server
import json
import os
import re
import socketserver
import sys
import threading
import time

DIST, BENCH, PORT, RESULTS = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
API_MODE = 'drop'
for arg in sys.argv[5:]:
    if arg.startswith('--api='):
        API_MODE = arg.split('=', 1)[1]

TYPES = {
    '.wasm': 'application/wasm', '.js': 'text/javascript', '.mjs': 'text/javascript',
    '.css': 'text/css', '.html': 'text/html; charset=utf-8', '.svg': 'image/svg+xml',
    '.json': 'application/json', '.ftl': 'text/plain; charset=utf-8', '.png': 'image/png',
    '.ico': 'image/x-icon', '.webmanifest': 'application/manifest+json', '.jpg': 'image/jpeg',
    '.webp': 'image/webp', '.txt': 'text/plain',
}
HASHED = re.compile(r'[0-9a-f]{12,}')
COMPRESSIBLE = {'.wasm', '.js', '.mjs', '.css', '.html', '.svg', '.json', '.ftl'}
GZ_CACHE = {}
LOCK = threading.Lock()
T0 = time.time()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, fmt, *args):
        sys.stderr.write('%8.3f %s\n' % (time.time() - T0, fmt % args))

    def do_POST(self):
        length = int(self.headers.get('content-length') or 0)
        body = self.rfile.read(length)
        if self.path.startswith('/__timing'):
            with LOCK, open(RESULTS, 'a') as out:
                out.write(body.decode('utf-8', 'replace').strip() + '\n')
            self.send_response(204)
            self.send_header('content-length', '0')
            self.end_headers()
            return
        self.api_or_404()

    def api_or_404(self):
        if API_MODE.startswith('delay:'):
            # A slow link: every API request answers late, with 503.
            time.sleep(int(API_MODE.split(':', 1)[1]) / 1000)
            if self.path.startswith('/api/v1/brand/theme.css'):
                body = b':root{--bench-brand:1}'
                self.send_response(200)
                self.send_header('content-type', 'text/css; charset=utf-8')
                self.send_header('content-length', str(len(body)))
                # The server's five minutes have long passed when a phone starts the app again.
                self.send_header('cache-control', 'no-cache')
                self.end_headers()
                self.wfile.write(body)
                return
            self.send_response(503)
            self.send_header('content-length', '0')
            self.end_headers()
            return
        if API_MODE == 'drop':
            self.close_connection = True
            try:
                self.connection.shutdown(2)
            except OSError:
                pass
            return
        self.send_response(503)
        self.send_header('content-length', '0')
        self.end_headers()

    def do_GET(self):
        path = self.path.split('?', 1)[0].split('#', 1)[0]
        if path.startswith('/api/'):
            return self.api_or_404()
        if path.startswith('/__bench/'):
            root, rel = BENCH, path[len('/__bench/'):] or 'index.html'
        else:
            root, rel = DIST, path.lstrip('/') or 'index.html'
        file = os.path.normpath(os.path.join(root, rel))
        if not file.startswith(os.path.normpath(root)) or not os.path.isfile(file):
            if root == DIST and '.' not in os.path.basename(rel):
                file = os.path.join(DIST, 'index.html')
            else:
                self.send_response(404)
                self.send_header('content-length', '0')
                self.end_headers()
                return
        ext = os.path.splitext(file)[1]
        with open(file, 'rb') as handle:
            data = handle.read()
        encoding = None
        if ext in COMPRESSIBLE and 'gzip' in (self.headers.get('accept-encoding') or ''):
            key = (file, os.path.getmtime(file))
            with LOCK:
                if key not in GZ_CACHE:
                    GZ_CACHE[key] = gzip.compress(data, 6)
                data = GZ_CACHE[key]
            encoding = 'gzip'
        self.send_response(200)
        self.send_header('content-type', TYPES.get(ext, 'application/octet-stream'))
        self.send_header('content-length', str(len(data)))
        if encoding:
            self.send_header('content-encoding', encoding)
            self.send_header('vary', 'Accept-Encoding')
        name = os.path.basename(file)
        if root == DIST and HASHED.search(name):
            self.send_header('cache-control', 'public, max-age=31536000, immutable')
        else:
            self.send_header('cache-control', 'no-cache')
        self.end_headers()
        self.wfile.write(data)


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


print('serving %s on 127.0.0.1:%d (api: %s)' % (DIST, PORT, API_MODE), flush=True)
Server(('127.0.0.1', PORT), Handler).serve_forever()
