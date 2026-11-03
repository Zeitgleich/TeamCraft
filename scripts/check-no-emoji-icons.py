#!/usr/bin/env python3
"""Fails when emoji are used as UI icons, markers or labels (issue #575).

The app's own design never uses emoji (.claude/rules/design.md): icons are the SVG line icons
(desktop and web), Material Symbols (Android) and SF Symbols (iOS). Emoji are user content:
messages, reactions, the emoji picker and the emoji of a custom status. Those places are listed in
ALLOWED with the reason; every other emoji in the UI sources fails the check.

- Lines inside `#[cfg(test)]` modules are skipped.
- Arrows, keyboard symbols (⌘ ⌥ ⇧ ↵) and geometric shapes (◆) are not emoji and are not flagged.
- An ALLOWED entry whose file no longer contains an emoji fails too, so the list cannot rot.

    scripts/check-no-emoji-icons.py [root]   # default: the repository root
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

SCAN_DIRS = [
  "crates/teamcraft-ui/src",
  "crates/teamcraft-ui/i18n",
  "crates/teamcraft-ui/style",
  "apps/android/app/src/main",
  "apps/ios/TeamCraft",
  "apps/desktop/src",
]

# Pictographs (U+1F000–U+1FAFF, including regional indicators), and the symbol blocks that hold
# the emoji-capable dingbats: ⚙ ⛔ ⚪ ✅ ✕ ❤ and friends, plus ⭐ and ⭕.
EMOJI = re.compile("[\U0001F000-\U0001FAFF☀-➿⭐⭕]")

CFG_TEST = re.compile(r"#\[cfg\(test\)\]")

# Paths (relative to the repository root) where emoji are content, not UI: each with the reason.
ALLOWED = {
  "crates/teamcraft-ui/src/mock.rs": "mock backend: demo data, never compiled into shipped builds",
  "crates/teamcraft-ui/src/components/message_toolbar.rs": "default quick reactions: reactions are user content",
  "crates/teamcraft-ui/src/components/status_menu.rs": "custom status presets: the emoji becomes part of the user's status",
  "crates/teamcraft-ui/src/slash.rs": "/status parses a custom status emoji the user types",
  "apps/desktop/src/core/dev_bridge.rs": "dev-only bridge sets demo custom statuses",
  "apps/ios/TeamCraft/Model/ChatActions.swift": "doc comment example of a custom status: user content",
  "apps/ios/TeamCraft/UI/ChatPane.swift": "quick reactions: reactions are user content",
  "apps/android/app/src/main/java/de/zeitgleich/teamcraft/ui/screens/Conversation.kt": "quick reactions: reactions are user content",
  "apps/android/app/src/main/java/de/zeitgleich/teamcraft/ui/screens/MessageFeed.kt": "quick reaction constant: reactions are user content",
}


def checked_lines(path):
  """Yields (line number, text) for the lines outside `#[cfg(test)]` modules."""
  skipping = False
  depth = 0
  pending = False
  for number, line in enumerate(path.read_text(encoding="utf-8").split("\n"), 1):
    if not skipping and CFG_TEST.search(line):
      pending = True
      continue
    if pending and "{" in line:
      pending = False
      skipping = True
      depth = 0
    if skipping:
      depth += line.count("{") - line.count("}")
      if depth <= 0 and "}" in line:
        skipping = False
      continue
    yield number, line


def emoji_findings(root, scan_dirs=SCAN_DIRS, allowed=ALLOWED):
  """Returns (findings, stale) for the files under scan_dirs: findings are (path, line, emoji)."""
  findings = []
  files_with_emoji = set()
  for scan_dir in scan_dirs:
    for path in sorted((root / scan_dir).rglob("*")):
      if not path.is_file():
        continue
      try:
        lines = list(checked_lines(path))
      except UnicodeDecodeError:
        continue
      relative = path.relative_to(root).as_posix()
      for number, line in lines:
        for match in EMOJI.findall(line):
          files_with_emoji.add(relative)
          if relative not in allowed:
            findings.append((relative, number, match))
  stale = sorted(path for path in allowed if path not in files_with_emoji)
  return findings, stale


def main(argv):
  root = pathlib.Path(argv[1]).resolve() if len(argv) > 1 else ROOT
  findings, stale = emoji_findings(root)
  for path, number, emoji in findings:
    print(f"{path}:{number}: emoji {emoji} used in the UI; use an app icon or plain text "
          "(.claude/rules/design.md), or list the file in ALLOWED with the reason")
  for path in stale:
    print(f"{path}: in ALLOWED but has no emoji any more; remove it from ALLOWED")
  if findings or stale:
    return 1
  print("no emoji icons in the UI sources")
  return 0


if __name__ == "__main__":
  sys.exit(main(sys.argv))
