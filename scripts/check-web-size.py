#!/usr/bin/env python3
"""Checks the web client's wasm against the download budget and prints its size.

The budget (.claude/rules/robustness.md, "Download size") counts the uncompressed wasm a first
visit downloads: the app (`teamcraft-ui-*_bg.wasm`) and the service worker
(`teamcraft-web-sw*_bg.wasm`, installed on the first visit too). CI runs this after the release
`trunk build` (`.github/workflows/web-size.yml`) on every change to the web client.

Two limits: above `--budget-bytes` (the budget) it warns; above `--max-bytes` it fails. While the
bundle is over the budget, the maximum is a ratchet just above its current size, so it can't grow
unnoticed; it comes down with every reduction until it meets the budget (docs/performance.md).

    scripts/check-web-size.py crates/teamcraft-ui/dist [--max-bytes N] [--baseline main.json] [--write sizes.json]

`--baseline` is the `--write` output of an earlier build (CI keeps main's); the delta to it is
printed. With `GITHUB_STEP_SUMMARY` set, the table also goes to the job summary.
"""
import argparse
import gzip
import json
import os
import pathlib
import sys

# 10 MB, as the budget table says (decimal megabytes, like the sizes reported in issue #248).
DEFAULT_BUDGET_BYTES = 10_000_000
# The files of a first visit, by the name trunk gives them (with a content hash).
PARTS = {
    "app": "teamcraft-ui-*_bg.wasm",
    "service_worker": "teamcraft-web-sw*_bg.wasm",
}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("dist", type=pathlib.Path, help="the trunk output directory")
    parser.add_argument("--budget-bytes", type=int, default=DEFAULT_BUDGET_BYTES)
    parser.add_argument("--max-bytes", type=int, help="fail above this (default: the budget)")
    parser.add_argument("--baseline", type=pathlib.Path, help="sizes of an earlier build (JSON)")
    parser.add_argument("--write", type=pathlib.Path, help="write this build's sizes (JSON)")
    args = parser.parse_args(argv)
    sizes = measure(args.dist)
    if sizes is None:
        return 2
    maybe_baseline = load_baseline(args.baseline)
    report = render(sizes, maybe_baseline, args.budget_bytes)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as file:
            file.write(report + "\n")
    if args.write:
        args.write.parent.mkdir(parents=True, exist_ok=True)
        args.write.write_text(json.dumps(sizes, indent=2) + "\n", encoding="utf-8")
    total = total_bytes(sizes)
    max_bytes = args.max_bytes if args.max_bytes is not None else args.budget_bytes
    if total > max_bytes:
        print(f"error: the web wasm is {total:,} bytes, over the maximum of {max_bytes:,}",
              file=sys.stderr)
        return 1
    if total > args.budget_bytes:
        # A workflow annotation where CI runs it; plain text elsewhere.
        print(f"::warning::the web wasm is {total:,} bytes, over the budget of {args.budget_bytes:,}")
    return 0


def measure(dist):
    """{part: {"bytes", "gzip_bytes"}}; None (with an error) when a part is missing or ambiguous."""
    sizes = {}
    for part, pattern in PARTS.items():
        matches = sorted(dist.glob(pattern))
        if len(matches) != 1:
            print(f"error: expected one {pattern} in {dist}, found {len(matches)}", file=sys.stderr)
            return None
        data = matches[0].read_bytes()
        sizes[part] = {"bytes": len(data), "gzip_bytes": len(gzip.compress(data, compresslevel=9))}
    return sizes


def load_baseline(maybe_path):
    """The earlier sizes, or None when there are none (first run, unreadable file)."""
    if maybe_path is None or not maybe_path.is_file():
        return None
    try:
        return json.loads(maybe_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def total_bytes(sizes):
    return sum(part["bytes"] for part in sizes.values())


def render(sizes, maybe_baseline, budget_bytes):
    """A Markdown table: each part, the total against the budget, and the delta to the baseline."""
    rows = [("app", sizes["app"]), ("service worker", sizes["service_worker"])]
    total = {
        "bytes": total_bytes(sizes),
        "gzip_bytes": sum(part["gzip_bytes"] for part in sizes.values()),
    }
    rows.append(("total", total))
    lines = [
        "| Web wasm       | Uncompressed | gzip -9    | Delta to main |",
        "|----------------|--------------|------------|---------------|",
    ]
    for (label, size), key in zip(rows, ["app", "service_worker", None]):
        delta = format_delta(size["bytes"], baseline_bytes(maybe_baseline, key))
        lines.append(f"| {label:<14} | {mb(size['bytes']):>12} | {mb(size['gzip_bytes']):>10} | {delta:>13} |")
    headroom = budget_bytes - total["bytes"]
    lines.append("")
    lines.append(f"Budget {mb(budget_bytes)} uncompressed; headroom {mb(headroom)}.")
    return "\n".join(lines)


def baseline_bytes(maybe_baseline, maybe_key):
    if maybe_baseline is None:
        return None
    try:
        if maybe_key is None:
            return total_bytes(maybe_baseline)
        return maybe_baseline[maybe_key]["bytes"]
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
