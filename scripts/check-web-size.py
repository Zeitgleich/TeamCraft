#!/usr/bin/env python3
"""Checks the web client's wasm against the download budget and prints its size.

The budget (.claude/rules/robustness.md, "Download size") counts the compressed wasm a signed-in
user's first visit transfers: the app (`teamcraft-ui-*_bg.wasm`), the signed-in app's chunks that
`init.js` fetches next to it (#334; a guest's meeting link skips them) and the service worker
(`teamcraft-web-sw*_bg.wasm`, installed on the first visit too). The server sends them compressed, the browser compiles while it
streams, and the service worker caches them afterwards, so the transfer size is what a user waits
for. It is measured as gzip -9, which every server and browser supports; brotli (smaller) is
printed too when the `brotli` module is installed. CI runs this after the release `trunk build`
(`.github/workflows/web-size.yml`) on every change to the web client.

Parts of the app that load when first used (the other split chunks and the call module) are listed
after the table; they are not part of the first visit and not of the budget.

`--budget-bytes` is the gzip -9 total. It warns within 5 % of it and fails above it. The
uncompressed size is printed for information only: what users wait for is the transfer.

        scripts/check-web-size.py crates/teamcraft-ui/dist [--budget-bytes N]
                [--baseline main.json] [--write sizes.json]

`--baseline` is the `--write` output of an earlier build (CI keeps main's); the delta to it is
printed. With `GITHUB_STEP_SUMMARY` set, the table also goes to the job summary.
"""
import argparse
import gzip
import json
import os
import pathlib
import re
import sys

try:
    import brotli
except ImportError:  # Optional: gzip is the budget, brotli is informational.
    brotli = None

# The gzip -9 total of app + service worker, decimal megabytes: 4.25 MB measured on main c55b2e58
# plus about 10 % (docs/performance.md, "Web bundle size"). Since the split (#334) the app counts
# only what a first visit loads; the chunks loaded when first used are listed apart.
DEFAULT_BUDGET_BYTES = 4_700_000
# Warn when the gzip total is above this share of the budget.
WARN_SHARE = 0.95
# The files of a first visit, by the name trunk gives them (with a content hash).
PARTS = {
    "app": "teamcraft-ui-*_bg.wasm",
    "service_worker": "teamcraft-web-sw*_bg.wasm",
}
# The signed-in app's chunks: the list `init.js` fetches next to the app (teamcraft-web-bundle).
SESSION_LIST = re.compile(r"for \(const href of \[([^\]]*)\]\)")
LABELS = {"app": "app", "session": "signed-in app", "service_worker": "service worker"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dist", type=pathlib.Path, help="the trunk output directory")
    parser.add_argument("--budget-bytes", type=int, default=DEFAULT_BUDGET_BYTES,
                        help="fail above this gzip -9 total, warn close to it")
    parser.add_argument("--baseline", type=pathlib.Path, help="sizes of an earlier build (JSON)")
    parser.add_argument("--write", type=pathlib.Path, help="write this build's sizes (JSON)")
    args = parser.parse_args(argv)
    try:
        sizes = measure(args.dist)
    except SessionListMissing as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    if sizes is None:
        return 2
    maybe_baseline = load_baseline(args.baseline)
    report = render(sizes, maybe_baseline, args.budget_bytes)
    report += render_lazy(measure_lazy(args.dist, session_files(args.dist)))
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as file:
            file.write(report + "\n")
    if args.write:
        args.write.parent.mkdir(parents=True, exist_ok=True)
        args.write.write_text(json.dumps(sizes, indent=2) + "\n", encoding="utf-8")
    return check_limits(sizes, args.budget_bytes)


def measure(dist):
    """{part: {"bytes", "gzip_bytes"[, "brotli_bytes"]}}; None (with an error) when a part is missing
    or ambiguous."""
    sizes = {}
    for part, pattern in PARTS.items():
        matches = sorted(dist.glob(pattern))
        if len(matches) != 1:
            print(f"error: expected one {pattern} in {dist}, found {len(matches)}", file=sys.stderr)
            return None
        sizes[part] = sizes_of([matches[0]])
    session = session_files(dist)
    if session:
        missing = [path.name for path in session if not path.is_file()]
        if missing:
            print(f"error: init.js fetches files that aren't there: {', '.join(missing)}", file=sys.stderr)
            return None
        sizes["session"] = sizes_of(session)
    return sizes


def sizes_of(paths):
    """{"bytes", "gzip_bytes"[, "brotli_bytes"]} summed over the files; each is compressed on its own,
    as it is sent."""
    total = {"bytes": 0, "gzip_bytes": 0}
    if brotli is not None:
        total["brotli_bytes"] = 0
    for path in paths:
        data = path.read_bytes()
        total["bytes"] += len(data)
        total["gzip_bytes"] += len(gzip.compress(data, compresslevel=9))
        if brotli is not None:
            total["brotli_bytes"] += len(brotli.compress(data, quality=11))
    return total


def session_files(dist):
    """The files `init.js` fetches next to the app (empty for a build without the list)."""
    init = dist / "init.js"
    if not init.is_file():
        return []
    match = SESSION_LIST.search(init.read_text(encoding="utf-8"))
    if not match:
        # Else the signed-in app would silently drop out of the budget.
        raise SessionListMissing("init.js has no list of the signed-in app's files")
    return [dist / name for name in re.findall(r"'/([^'/]+)'", match.group(1))]


class SessionListMissing(Exception):
    pass


def measure_lazy(dist, session):
    """[(name, bytes, gzip_bytes)] of every other wasm file: what loads later."""
    first_visit = {path for pattern in PARTS.values() for path in dist.glob(pattern)} | set(session)
    lazy = []
    for path in sorted(dist.glob("*.wasm")):
        if path in first_visit:
            continue
        data = path.read_bytes()
        lazy.append((path.name, len(data), len(gzip.compress(data, compresslevel=9))))
    return lazy


def render_lazy(lazy):
    if not lazy:
        return ""
    lines = ["", "", "Loaded when first used (not part of the first visit or the budget):", "",
             "| Chunk                                            | Uncompressed | gzip -9    |",
             "|--------------------------------------------------|--------------|------------|"]
    for name, size, gzip_size in lazy:
        lines.append(f"| {name:<48} | {mb(size):>12} | {mb(gzip_size):>10} |")
    return "\n".join(lines)


def check_limits(sizes, budget_bytes):
    """The exit code: 1 above the budget, 0 otherwise (with a warning close to it)."""
    gzip_total = total_bytes(sizes, "gzip_bytes")
    if gzip_total > budget_bytes:
        print(f"error: the web wasm is {gzip_total:,} bytes gzip -9, over the budget of "
              f"{budget_bytes:,}", file=sys.stderr)
        return 1
    if gzip_total > budget_bytes * WARN_SHARE:
        # A workflow annotation where CI runs it; plain text elsewhere.
        print(f"::warning::the web wasm is {gzip_total:,} bytes gzip -9, within "
              f"{100 - WARN_SHARE * 100:.0f} % of the budget of {budget_bytes:,}")
    return 0


def load_baseline(maybe_path):
    """The earlier sizes, or None when there are none (first run, unreadable file)."""
    if maybe_path is None or not maybe_path.is_file():
        return None
    try:
        return json.loads(maybe_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def total_bytes(sizes, key):
    return sum(part[key] for part in sizes.values())


def render(sizes, maybe_baseline, budget_bytes):
    """A Markdown table: each part, the total against the limits, and the gzip delta to the baseline."""
    has_brotli = all("brotli_bytes" in part for part in sizes.values())
    keys = ["bytes", "gzip_bytes"] + (["brotli_bytes"] if has_brotli else [])
    total = {key: total_bytes(sizes, key) for key in keys}
    rows = [(LABELS[part], sizes[part], part) for part in LABELS if part in sizes] + [("total", total, None)]
    brotli_header = " brotli -11 |" if has_brotli else ""
    brotli_rule = "------------|" if has_brotli else ""
    lines = [
        f"| Web wasm       | Uncompressed | gzip -9    |{brotli_header} gzip delta to main |",
        f"|----------------|--------------|------------|{brotli_rule}--------------------|",
    ]
    for label, size, maybe_key in rows:
        delta = format_delta(size["gzip_bytes"], baseline_bytes(maybe_baseline, maybe_key, "gzip_bytes"))
        brotli_cell = f" {mb(size['brotli_bytes']):>10} |" if has_brotli else ""
        lines.append(f"| {label:<14} | {mb(size['bytes']):>12} | {mb(size['gzip_bytes']):>10} |"
                     f"{brotli_cell} {delta:>18} |")
    lines.append("")
    lines.append(f"Budget {mb(budget_bytes)} gzip -9; headroom {mb(budget_bytes - total['gzip_bytes'])}.")
    if "session" in sizes:
        guest = total["gzip_bytes"] - sizes["session"]["gzip_bytes"]
        lines.append(f"A guest's first visit (meeting link, no signed-in app): {mb(guest)} gzip -9.")
    return "\n".join(lines)


def baseline_bytes(maybe_baseline, maybe_part, key):
    if maybe_baseline is None:
        return None
    try:
        if maybe_part is None:
            return total_bytes(maybe_baseline, key)
        return maybe_baseline[maybe_part][key]
    except (KeyError, TypeError):
        return None


def format_delta(current, maybe_previous):
    if maybe_previous is None:
        return "–"
    return f"{(current - maybe_previous) / 1_000:+,.1f} kB"


def mb(size):
    return f"{size / 1_000_000:.2f} MB"


if __name__ == "__main__":
    sys.exit(main())
