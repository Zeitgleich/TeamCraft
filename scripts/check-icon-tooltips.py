#!/usr/bin/env python3
"""Fails when an icon-only button has an `aria-label` but no visible tooltip (issue #2019).

Sighted pointer users learn what an icon button does from its tooltip: `title=` or `data-tip=`
next to the `aria-label`. The check reads every `<button ...>` tag of the UI crate that has the
`icon-btn` class and reports the ones with an `aria-label` and neither attribute.

    scripts/check-icon-tooltips.py [root]   # default: crates/teamcraft-ui/src of the repository

Mark the line of the tag with `// tooltip-ok: <reason>` when a button must not show one.
"""
import pathlib
import re
import sys


def tag_end(text: str, start: int) -> int:
    depth = 0
    index = start
    while index < len(text):
        char = text[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == ">" and depth == 0 and text[index - 1] != "=":
            return index
        index += 1
    return len(text)


def main() -> int:
    root = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else (
        pathlib.Path(__file__).resolve().parent.parent / "crates/teamcraft-ui/src"
    )
    failures = []
    for path in sorted(root.rglob("*.rs")):
        text = path.read_text()
        for match in re.finditer(r"<button\b", text):
            tag = text[match.start():tag_end(text, match.end())]
            if "icon-btn" not in tag or "aria-label" not in tag:
                continue
            if "title=" in tag or "data-tip" in tag or "tooltip-ok" in tag:
                continue
            line = text.count("\n", 0, match.start()) + 1
            failures.append(f"{path}:{line}: icon button has an aria-label but no title/data-tip")
    for failure in failures:
        print(failure)
    if failures:
        print(f"{len(failures)} icon button(s) without a tooltip (issue #2019)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
