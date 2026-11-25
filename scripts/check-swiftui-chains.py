#!/usr/bin/env python3
"""Fails on a SwiftUI modifier chain in the iOS app that is too long.

Issue #2392: every modifier wraps the view in another generic type, and a `some View` property
used inside another view's body adds its whole type to that body's type. ChatPane's type nested so
deep that SwiftUI's instantiation of its metadata overflowed the main thread's stack, and every
chat crashed on open (iOS 1.0.44). The compiler doesn't warn, and the simulator's bigger stack
hides it; the crash showed only on devices.

This counts the modifiers of each chain (`.padding()`, `.sheet { … }`, … at one indentation, with
their arguments and closures between them) and fails when one is longer than the limit. A longer
view is cut into parts behind a type boundary: a separate `View` struct, a `ViewModifier`, or an
`AnyView` property (see ChatPane's `lifecycle`).

    scripts/check-swiftui-chains.py [--max N] [paths ...]   # default: apps/ios

Test code is left out.
"""
import argparse
import pathlib
import sys

DEFAULT_MAX = 30
TEST_PARTS = ("Tests", "UITests")
CONTINUATION = ("}", ")", "]", "//")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--max", type=int, default=DEFAULT_MAX, help="longest allowed chain")
    parser.add_argument("paths", nargs="*", default=["apps/ios"])
    args = parser.parse_args()
    failures = []
    for path in swift_files(args.paths):
        for line, count in long_chains(path.read_text(encoding="utf-8"), args.max):
            failures.append(f"{path}:{line}: a chain of {count} modifiers (at most {args.max})")
    for failure in failures:
        print(failure)
    if failures:
        print(
            "\nCut the view into parts behind a type boundary (a View struct, a ViewModifier or an AnyView"
            " property): a too deep view type overflows the main thread's stack on devices (issue #2392).",
            file=sys.stderr)
        return 1
    return 0


def swift_files(paths: list[str]):
    for root in map(pathlib.Path, paths):
        files = [root] if root.is_file() else sorted(root.rglob("*.swift"))
        for path in files:
            if not any(part.endswith(TEST_PARTS) for part in path.parts):
                yield path


def long_chains(source: str, limit: int) -> list[tuple[int, int]]:
    """The chains longer than `limit`, as (line of their first modifier, number of modifiers)."""
    found = []
    # Open chains by their indentation: (first line, count).
    open_chains: dict[int, tuple[int, int]] = {}
    for number, raw in enumerate(source.splitlines(), start=1):
        stripped = raw.strip()
        if not stripped:
            continue
        indent = len(raw) - len(raw.lstrip(" "))
        # A line ends every chain deeper than itself; at its own indentation only a modifier or the
        # close of a modifier's argument or closure continues the chain.
        for level in [level for level in open_chains if level > indent]:
            close(open_chains.pop(level), limit, found)
        if stripped.startswith(".") and not stripped.startswith(".."):
            first, count = open_chains.get(indent, (number, 0))
            open_chains[indent] = (first, count + 1)
        elif indent in open_chains and not stripped.startswith(CONTINUATION):
            close(open_chains.pop(indent), limit, found)
    for chain in open_chains.values():
        close(chain, limit, found)
    return sorted(found)


def close(chain: tuple[int, int], limit: int, found: list[tuple[int, int]]) -> None:
    if chain[1] > limit:
        found.append(chain)


if __name__ == "__main__":
    sys.exit(main())
