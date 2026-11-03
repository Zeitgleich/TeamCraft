"""Tests of scripts/check-no-emoji-icons.py: emoji stay out of UI icons, markers and labels (issue #575):
python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "check-no-emoji-icons.py"
SPEC = importlib.util.spec_from_file_location("check_no_emoji_icons", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

SCAN = ["ui"]
UI_FILE = "ui/button.rs"


class EmojiIconTests(unittest.TestCase):
  def scan(self, files, allowed=None):
    with tempfile.TemporaryDirectory() as tmp:
      root = pathlib.Path(tmp)
      for relative, text in files.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
      return check.emoji_findings(root, SCAN, allowed if allowed is not None else {})

  def test_flags_emoji_used_as_an_icon(self):
    findings, stale = self.scan({UI_FILE: 'const DEFAULT: &str = "💬";\n'})
    self.assertEqual(findings, [(UI_FILE, 1, "💬")])
    self.assertEqual(stale, [])

  def test_flags_emoji_in_kotlin_xml_and_swift(self):
    findings, _ = self.scan({
      "ui/Presence.kt": 'val dot = "🟢"\n',
      "ui/strings.xml": '<string name="photo">📷 Photo</string>\n',
      "ui/Row.swift": 'let marker = "⛔"\n',
    })
    self.assertEqual(sorted(path for path, _, _ in findings), ["ui/Presence.kt", "ui/Row.swift", "ui/strings.xml"])

  def test_flags_the_gear_and_check_glyphs(self):
    findings, _ = self.scan({UI_FILE: 'let a = "⚙"; let b = "✅";\n'})
    self.assertEqual([emoji for _, _, emoji in findings], ["⚙", "✅"])

  def test_arrows_keyboard_symbols_and_shapes_are_not_emoji(self):
    findings, _ = self.scan({UI_FILE: 'let keys = "⌘⇧F ⌥↑ ←→ ↵ ◆";\n'})
    self.assertEqual(findings, [])

  def test_cfg_test_modules_are_skipped(self):
    text = (
      'fn render() {}\n'
      '#[cfg(test)]\n'
      'mod tests {\n'
      '  fn reaction() -> &\'static str { "👍" }\n'
      '  #[test]\n'
      '  fn nested() { if true { let _ = "🎉"; } }\n'
      '}\n'
      'const AFTER: &str = "🔖";\n'
    )
    findings, _ = self.scan({UI_FILE: text})
    self.assertEqual(findings, [(UI_FILE, 8, "🔖")])

  def test_allowed_file_with_emoji_passes(self):
    findings, stale = self.scan({UI_FILE: 'let quick = ["👍", "🎉"];\n'}, allowed={UI_FILE: "quick reactions"})
    self.assertEqual(findings, [])
    self.assertEqual(stale, [])

  def test_allowed_file_without_emoji_is_stale(self):
    findings, stale = self.scan({UI_FILE: "fn plain() {}\n"}, allowed={UI_FILE: "old reason"})
    self.assertEqual(findings, [])
    self.assertEqual(stale, [UI_FILE])

  def test_repository_has_no_emoji_icons(self):
    findings, stale = check.emoji_findings(ROOT)
    self.assertEqual(findings, [])
    self.assertEqual(stale, [])


if __name__ == "__main__":
  unittest.main()
