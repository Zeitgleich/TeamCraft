#!/usr/bin/env python3
"""Fails when UI code calls `.track()` on a memo that nothing else in its file reads (issue #1203).

In Leptos a `Memo` is lazy and `track()` does not compute it. A memo nobody reads has no sources,
so a change of its inputs never reaches the effect that only tracked it: the effect runs once at
mount and never again (#1196: a prompt that stayed hidden until a reload). `track()` is fine on a
signal, a store field or a memo some other code reads. The fix is to read the value in the effect
(`let _ = memo.get();`), which computes it and subscribes in one go.

A memo is a `let name = Memo::new(..)` (also `new_with_compare`, `new_owning`) in the same file. If
`name.track()` is the only place that mentions it (comments do not count), the line is reported.

    scripts/check-memo-track.py [root]   # default: crates/teamcraft-ui/src of the repository

The check is textual: it cannot see a memo that arrives through a parameter or a struct field, and
a name reused for something else in the file counts as a read. Mark a line (or the line above it)
with `// memo-ok: <reason>` for a deliberate `track()`.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SCAN_DIR = "crates/teamcraft-ui/src"

MEMO_BINDING = re.compile(r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=]+)?=\s*Memo::(?:new|new_with_compare|new_owning)\b")
TRACK_CALL = re.compile(r"\b(\w+)\.track\(\)")
MARKER = "memo-ok:"

# Files with a known bad `track()` whose fix is in flight: relative path -> pull request.
# #1202 fixes this one; delete the entry once it is merged.
PENDING_FIX = {
  "crates/teamcraft-ui/src/components/link_request_prompt.rs": "#1202",
}


STRING_LITERAL = re.compile(r'"(?:[^"\\]|\\.)*"')


def code_of(line: str) -> str:
  """The line without its string literals and its `//` comment: neither reads a memo."""
  return STRING_LITERAL.sub('""', line).split("//", 1)[0]


def findings_in(text: str) -> list[tuple[int, str]]:
  """(line number, memo name) of every `track()` on a memo that has no other mention."""
  lines = text.split("\n")
  code = [code_of(line) for line in lines]
  memos = {match.group(1) for line in code for match in MEMO_BINDING.finditer(line)}
  found = []
  for number, line in enumerate(code):
    for match in TRACK_CALL.finditer(line):
      name = match.group(1)
      if name not in memos:
        continue
      if MARKER in lines[number] or (number > 0 and MARKER in lines[number - 1]):
        continue
      if not is_read_somewhere(name, code):
        found.append((number + 1, name))
  return found


def is_read_somewhere(name: str, code: list[str]) -> bool:
  """Whether a line mentions `name` other than its binding and its `track()` calls."""
  mention = re.compile(r"\b" + re.escape(name) + r"\b")
  for line in code:
    binding = MEMO_BINDING.search(line)
    is_binding_line = binding is not None and binding.group(1) == name
    for use in mention.finditer(line):
      if is_binding_line and use.start() == binding.start(1):
        continue
      if line[use.end():].startswith(".track()"):
        continue
      return True
  return False


def main() -> int:
  root = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT
  base = root if len(sys.argv) > 1 else root / SCAN_DIR
  problems = []
  for path in sorted(base.rglob("*.rs")):
    relative = path.relative_to(root).as_posix()
    if relative in PENDING_FIX:
      continue
    for number, name in findings_in(path.read_text(encoding="utf-8")):
      problems.append(
        f"{relative}:{number}: `{name}.track()` on a memo nothing else reads is never computed, "
        f"so the effect never re-runs; read it instead: `let _ = {name}.get();` (issue #1203)"
      )
  for problem in problems:
    print(problem)
  return 1 if problems else 0


if __name__ == "__main__":
  sys.exit(main())
