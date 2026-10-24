#!/usr/bin/env python3
"""Checks the web client's wasm against the download budget and prints its size.

The budget (.claude/rules/robustness.md, "Download size") counts the compressed wasm a first visit
transfers: the app (`teamcraft-ui-*_bg.wasm`) and the service worker (`teamcraft-web-sw*_bg.wasm`,
installed on the first visit too). The server sends them compressed, the browser compiles while it
streams, and the service worker caches them afterwards, so the transfer size is what a user waits
for. It is measured as gzip -9, which every server and browser supports; brotli (smaller) is
printed too when the `brotli` module is installed. CI runs this after the release `trunk build`
(`.github/workflows/web-size.yml`) on every change to the web client.

Limits:

- `--budget-bytes`: the gzip -9 total. It warns within 5 % of it and fails above it.
- `--max-uncompressed-bytes`: a growth ratchet on the uncompressed total (memory and compile time
  grow with it). It sits a little above the current size so the bundle can't grow unnoticed; lower it
    after a reduction, raising it is a decision (docs/performance.md, "Web bundle size").

        scripts/check-web-size.py crates/teamcraft-ui/dist [--budget-bytes N]
                [--max-uncompressed-bytes N] [--baseline main.json] [--write sizes.json]

`--baseline` is the `--write` output of an earlier build (CI keeps main's); the delta to it is
printed. With `GITHUB_STEP_SUMMARY` set, the table also goes to the job summary.
"""
import argparse
import gzip
import json
import os
import pathlib
import sys

try:
    import brotli
except ImportError:  # Optional: gzip is the budget, brotli is informational.
    brotli = None

# The gzip -9 total of app + service worker, decimal megabytes: 4.25 MB measured on main c55b2e58
# plus about 10 % (docs/performance.md, "Web bundle size").
DEFAULT_BUDGET_BYTES = 4_700_000
# Warn when the gzip total is above this share of the budget.
WARN_SHARE = 0.95
# The files of a first visit, by the name trunk gives them (with a content hash).
PARTS = {
    "app": "teamcraft-ui-*_bg.wasm",
    "service_worker": "teamcraft-web-sw*_bg.wasm",
}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dist", type=pathlib.Path, help="the trunk output directory")
    parser.add_argument("--budget-bytes", type=int, default=DEFAULT_BUDGET_BYTES,
                        help="fail above this gzip -9 total, warn close to it")
    parser.add_argument("--max-uncompressed-bytes", type=int,
                        help="fail above this uncompressed total (the growth ratchet)")
    parser.add_argument("--baseline", type=pathlib.Path, help="sizes of an earlier build (JSON)")
    parser.add_argument("--write", type=pathlib.Path, help="write this build's sizes (JSON)")
    args = parser.parse_args(argv)
    sizes = measure(args.dist)
    if sizes is None:
        return 2
    maybe_baseline = load_baseline(args.baseline)
    report = render(sizes, maybe_baseline, args.budget_bytes, args.max_uncompressed_bytes)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as file:
            file.write(report + "\n")
    if args.write:
        args.write.parent.mkdir(parents=True, exist_ok=True)
        args.write.write_text(json.dumps(sizes, indent=2) + "\n", encoding="utf-8")
    return check_limits(sizes, args.budget_bytes, args.max_uncompressed_bytes)


def measure(dist):
    """{part: {"bytes", "gzip_bytes"[, "brotli_bytes"]}}; None (with an error) when a part is missing
    or ambiguous."""
    sizes = {}
    for part, pattern in PARTS.items():
        matches = sorted(dist.glob(pattern))
        if len(matches) != 1:
            print(f"error: expected one {pattern} in {dist}, found {len(matches)}", file=sys.stderr)
            return None
        data = matches[0].read_bytes()
        sizes[part] = {"bytes": len(data), "gzip_bytes": len(gzip.compress(data, compresslevel=9))}
        if brotli is not None:
            sizes[part]["brotli_bytes"] = len(brotli.compress(data, quality=11))
    return sizes


def check_limits(sizes, budget_bytes, maybe_max_uncompressed_bytes):
    """The exit code: 1 when a limit is exceeded, 0 otherwise (with a warning close to the budget)."""
    gzip_total = total_bytes(sizes, "gzip_bytes")
    uncompressed_total = total_bytes(sizes, "bytes")
    failed = False
    if gzip_total > budget_bytes:
        print(f"error: the web wasm is {gzip_total:,} bytes gzip -9, over the budget of "
              f"{budget_bytes:,}", file=sys.stderr)
        failed = True
    if maybe_max_uncompressed_bytes is not None and uncompressed_total > maybe_max_uncompressed_bytes:
        print(f"error: the web wasm is {uncompressed_total:,} bytes uncompressed, over the ratchet of "
              f"{maybe_max_uncompressed_bytes:,}", file=sys.stderr)
        failed = True
    if failed:
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


def render(sizes, maybe_baseline, budget_bytes, maybe_max_uncompressed_bytes):
    """A Markdown table: each part, the total against the limits, and the gzip delta to the baseline."""
    has_brotli = all("brotli_bytes" in part for part in sizes.values())
    keys = ["bytes", "gzip_bytes"] + (["brotli_bytes"] if has_brotli else [])
    total = {key: total_bytes(sizes, key) for key in keys}
    rows = [("app", sizes["app"], "app"), ("service worker", sizes["service_worker"], "service_worker"),
            ("total", total, None)]
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
    if maybe_max_uncompressed_bytes is not None:
        lines.append(f"Uncompressed ratchet {mb(maybe_max_uncompressed_bytes)}; headroom "
                     f"{mb(maybe_max_uncompressed_bytes - total['bytes'])}.")
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
