#!/usr/bin/env python3
"""Fails if UI code touches a signal without `try_` in code that runs later than its component.

Issue #322: a task that outlives its component (the core answered after a dialog was closed) and
then reads or writes the component's signals panics with "Tried to access a reactive value that
has already been disposed", which kills the wasm app. The same goes for timers, animation frames
and JS callbacks. After the first `.await` of a task, and anywhere in a deferred callback
(`set_timeout`, `request_animation_frame`, `Closure::new`/`wrap`/`once`), signals are only touched
with `try_get_untracked`, `try_set`, `try_update`, `try_with`, ... (or through helpers such
as `feedback::PendingAction`, which do). `on:blur` and `on:focusout` handlers count as deferred
too: the browser fires them when a focused element is removed with its view (#378), after the
component's owner is disposed.

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
# Calls whose argument runs later, when the component that started it may be gone.
CALLBACK_CALL = re.compile(
    r"\b(spawn_local|set_timeout|set_timeout_with_handle|request_animation_frame|"
    r"request_animation_frame_with_handle|Closure::(?:<.*?>::)?(?:new|wrap|once|once_into_js))\("
)
# Inline view handlers that also run while their element is removed (a focused element blurs).
REMOVAL_HANDLER = re.compile(r"\bon:(?:blur|focusout)=\s*(?:move\s*)?\|[^|]*\|\s*")
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
            f"\n{len(findings)} signal access(es) in deferred code without try_ (issue #322).\n"
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
    for start, end, is_deferred in callback_bodies(source):
        body = source[start:end]
        # A deferred callback runs later as a whole; a task only after its first await.
        first_await = 0 if is_deferred else body.find(".await")
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


def closing_brace(source, position):
    """Index of the `}` that closes the block whose contents start at `position`."""
    depth = 1
    while position < len(source) and depth:
        if source[position] == "{":
            depth += 1
        elif source[position] == "}":
            depth -= 1
        elif source[position] == '"':
            position = skip_string(source, position)
        position += 1
    return position - 1


def callback_bodies(source):
    """(start, end, is_deferred) of the arguments of every call that runs code later: tasks
    (`spawn_local`) and deferred callbacks (timers, animation frames, JS closures)."""
    for match in CALLBACK_CALL.finditer(source):
        is_deferred = match.group(1) != "spawn_local"
        yield match.end(), closing_paren(source, match.end()), is_deferred
    # `async fn` bodies: they are spawned by name (`spawn_local(load(state))`) as often as inline.
    for match in re.finditer(r"\basync fn\b[^{;]*\{", source):
        yield match.end(), closing_brace(source, match.end()), False
    # A block body runs to its closing brace, an expression body to the end of the line.
    for match in REMOVAL_HANDLER.finditer(source):
        if source.startswith("{", match.end()):
            yield match.end() + 1, closing_brace(source, match.end() + 1), True
        else:
            end = source.find("\n", match.end())
            yield match.end(), len(source) if end < 0 else end, True


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
