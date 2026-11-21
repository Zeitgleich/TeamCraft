#!/usr/bin/env python3
"""Fails on a new blocking call in the mobile apps' code unless it says why it is off the UI thread.

Issue #2184: the UI thread (iOS main actor, Android main thread) never waits on the store, the
network, a lock, a semaphore or a blocking core call; the watchdog killed the iOS app for it. Core
calls that can block are `async` exports, so the compiler keeps those off the UI thread. What the
apps do themselves isn't checked by anything, so every use of a blocking primitive is listed here
and must carry a marker on its line or the line above it:

    // blocking-ok: <why this never runs on the UI thread, or why it may wait there>

The guard (`UiThreadGuard`) checks at run time where it can; this check catches the new call in
review.

    scripts/check-ui-thread-blocking.py [paths ...]   # default: apps/ios, apps/android/app/src

Test code is left out: tests may wait (and ask off the main thread where it matters).
"""
import argparse
import pathlib
import re
import sys

MARKER = "blocking-ok:"
SWIFT = [
    (re.compile(r"\.sync\s*\{|\.sync\(execute"), "DispatchQueue.sync waits for the queue"),
    (re.compile(r"\bDispatchSemaphore\b|\b\w*(?:semaphore|Semaphore)\.wait\("), "a semaphore waits"),
    (re.compile(r"\b\w*(?:group|Group)\.wait\("), "DispatchGroup.wait waits"),
    (re.compile(r"\bThread\.sleep\b|\busleep\(|(?<![\w.])(?<!await )sleep\("), "sleeping blocks the thread"),
    (re.compile(r"\.acquire\(timeout:"), "ProcessLock.acquire waits for the other process"),
    (re.compile(r"\.currentAccessToken\("), "the token read waits for a refresh"),
    (re.compile(r"\bcore\.suspend\(\)|\.suspend\(\)\s*$"), "the core's suspend waits for its threads"),
]
KOTLIN = [
    (re.compile(r"\brunBlocking\b"), "runBlocking blocks the thread"),
    (re.compile(r"\bThread\.sleep\("), "sleeping blocks the thread"),
    (re.compile(r"\bCountDownLatch\b"), "a latch waits"),
    (re.compile(r"\bjava\.util\.concurrent\.Semaphore\b|\bSemaphore\(\d"), "a semaphore waits"),
]
TEST_PARTS = ("Tests", "test", "androidTest")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "paths", nargs="*", type=pathlib.Path, default=[pathlib.Path("apps/ios"), pathlib.Path("apps/android/app/src")]
    )
    args = parser.parse_args(argv)
    findings = []
    for root in args.paths:
        for path in source_files(root):
            findings.extend(check_source(str(path), path.read_text(encoding="utf-8")))
    for finding in findings:
        print(finding)
    if findings:
        print(
            f"\n{len(findings)} blocking call(s) in app code without a `// {MARKER} <reason>` marker (issue #2184).\n"
            "Move the call off the UI thread (an async core export, Task.detached, Dispatchers.IO) and\n"
            "say so in the marker, or say why it may wait.",
            file=sys.stderr,
        )
        return 1
    return 0


def source_files(root):
    """Swift and Kotlin sources under `root`, without test code."""
    if root.is_file():
        return [root]
    files = sorted(list(root.rglob("*.swift")) + list(root.rglob("*.kt")))
    return [path for path in files if not any(part in TEST_PARTS or part.endswith("Tests") for part in path.parts)]


def check_source(name, source):
    """Findings as `file:line: text (why)` for every unmarked blocking call."""
    rules = SWIFT if name.endswith(".swift") else KOTLIN
    lines = source.splitlines()
    findings = []
    for number, line in enumerate(lines):
        code = line.split("//", 1)[0]
        stripped = code.strip()
        previous = lines[number - 1] if number > 0 else ""
        if not stripped or stripped.startswith(("import ", "*", "/*")) or MARKER in line or MARKER in previous:
            continue
        for pattern, why in rules:
            if pattern.search(code):
                findings.append(f"{name}:{number + 1}: {line.strip()} ({why})")
                break
    return findings


if __name__ == "__main__":
    sys.exit(main())
