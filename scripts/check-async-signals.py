#!/usr/bin/env python3
"""Fails if UI code touches a signal without `try_` after an `.await` in a `spawn_local` task.

Issue #322: a task that outlives its component (the core answered after a dialog was closed) and
then reads or writes the component's signals panics with "Tried to access a reactive value that
has already been disposed", which kills the wasm app. After the first `.await`, signals are only
touched with `try_get_untracked`, `try_set`, `try_update`, `try_with`, ... (or through helpers such
as `feedback::PendingAction`, which do).

    scripts/check-async-signals.py [paths ...]   # default: crates/teamcraft-ui/src

The check is textual: `.get()` / `.set(..)` etc. on a `Cell` or a `StoredValue` the task owns look
the same as on a signal. Mark such a line (or the line above it) with `// async-ok: <reason>`.
"""
import argparse
import pathlib
import re
import sys

SIGNAL_ACCESS = re.compile(
    r"\.(get|get_untracked|set|update|update_untracked|with|with_untracked|"
    r"get_value|set_value|update_value|with_value|read|write)\((?!\s*&)"
)
# `.get()` without arguments only: `.get(&key)` / `.get(index)` are maps and slices.
ARGUMENT_GETTERS = {"get", "get_untracked"}
MARKER = "async-ok:"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="*", type=pathlib.Path, default=[pathlib.Path("crates/teamcraft-ui/src")])
    args = parser.parse_args(argv)
    findings = []
    for root in args.paths:
        files = [root] if root.is_file() else sorted(root.rglob("*.rs"))
        for path in files:
            findings.extend(check_source(str(path), path.read_text(encoding="utf-8")))
    for finding in findings:
        print(finding)
    if findings:
        print(
            f"\n{len(findings)} signal access(es) after an .await in spawn_local without try_ (issue #322).\n"
            "Use try_get_untracked/try_set/try_update/try_with, or mark a non-signal with `// async-ok: <reason>`.",
            file=sys.stderr,
        )
        return 1
    return 0


def check_source(name, source):
    """Findings as `file:line: text` for every unguarded access after an await in a spawned task."""
    if "#[cfg(test)]" in source:
        source = source[: source.index("#[cfg(test)]")]
    lines = source.splitlines()
    findings = []
    for start, end in spawned_bodies(source):
        body = source[start:end]
        first_await = body.find(".await")
        if first_await < 0:
            continue
        for match in SIGNAL_ACCESS.finditer(source, start, end):
            method = match.group(1)
            rest = source[match.end() : match.end() + 1]
            if method in ARGUMENT_GETTERS and rest != ")":
                continue
            # Before the first await only `signal.set(future.await)` counts: it writes afterwards.
            if match.start() < start + first_await and ".await" not in source[match.end() : closing_paren(source, match.end())]:
                continue
            line_number = source.count("\n", 0, match.start())
            line = lines[line_number]
            previous = lines[line_number - 1] if line_number > 0 else ""
            if MARKER in line or MARKER in previous or is_comment(line):
                continue
            findings.append(f"{name}:{line_number + 1}: {line.strip()}")
    return sorted(set(findings), key=lambda text: int(text.split(":")[1]))


def closing_paren(source, position):
    """Index of the `)` that closes the call whose arguments start at `position`."""
    depth = 1
    while position < len(source) and depth:
        if source[position] == "(":
            depth += 1
        elif source[position] == ")":
            depth -= 1
        elif source[position] == '"':
            position = skip_string(source, position)
        position += 1
    return position - 1


def spawned_bodies(source):
    """(start, end) of the argument of every `spawn_local(` call, by matching parentheses."""
    for match in re.finditer(r"\bspawn_local\(", source):
        yield match.end(), closing_paren(source, match.end())


def skip_string(source, position):
    """Index of the closing quote of the string literal that starts at `position`."""
    position += 1
    while position < len(source) and source[position] != '"':
        if source[position] == "\\":
            position += 1
        position += 1
    return position


def is_comment(line):
    return line.lstrip().startswith("//")


if __name__ == "__main__":
    sys.exit(main())
