#!/usr/bin/env python3
"""Fails when a UI source renders a guest or "external people" marker without asking who looks (issue #911).

The markers warn members that somebody external reads along; for a guest they mean nothing. So every
place that renders one goes through `AppState::shows_guest_markers()`, which is false for a guest
viewer. A source file that mentions a marker (the `badge-guest` class, the `badge-guest`,
`badge-external` and `external-badge` strings, the `sb-external` class) must also call that helper.

- An ALLOWED entry whose file no longer mentions a marker fails too, so the list cannot rot.

    scripts/check-guest-markers.py [root]   # default: the repository root
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

SCAN_DIR = "crates/teamcraft-ui/src"

MARKER = re.compile(r"badge-guest|badge-external|external-badge|sb-external")
HELPER = "shows_guest_markers"

# Files that mention a marker without rendering it for people: path (relative to the root) -> why.
ALLOWED = {
  "crates/teamcraft-ui/src/components/admin_integrations.rs": "reuses the badge class for a revoked source, not for people",
}


def main() -> int:
  root = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ROOT
  problems = []
  seen_allowed = set()
  for path in sorted((root / SCAN_DIR).rglob("*.rs")):
    relative = path.relative_to(root).as_posix()
    text = path.read_text(encoding="utf-8")
    if not MARKER.search(text):
      continue
    if relative in ALLOWED:
      seen_allowed.add(relative)
      continue
    if HELPER not in text:
      problems.append(f"{relative}: renders a guest or external marker without {HELPER}()")
  for relative in sorted(set(ALLOWED) - seen_allowed):
    problems.append(f"{relative}: in ALLOWED but no longer mentions a marker; remove the entry")
  for problem in problems:
    print(problem)
  return 1 if problems else 0


if __name__ == "__main__":
  sys.exit(main())
