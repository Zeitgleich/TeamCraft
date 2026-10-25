"""Tests of scripts/play-release-notes.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "play-release-notes.py"
SPEC = importlib.util.spec_from_file_location("play_release_notes", SCRIPT)
notes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notes)

URL = "https://github.com/example/teamcraft/releases/tag/v1.0.0"


class PlainText(unittest.TestCase):
    def test_list_emphasis_code_and_links(self):
        markdown = "- **Threads** in the `sidebar`\n- See [the docs](https://example.com/docs)"
        self.assertEqual(notes.plain_text(markdown), "• Threads in the sidebar\n• See the docs")

    def test_headings_and_blank_lines(self):
        markdown = "### Calls\n\n\n\n* Faster join  \n"
        self.assertEqual(notes.plain_text(markdown), "Calls\n\n• Faster join")

    def test_images_dropped(self):
        self.assertEqual(notes.plain_text("New icon ![icon](icon.png)"), "New icon")


class PlayNotes(unittest.TestCase):
    def test_release(self):
        self.assertEqual(notes.play_notes("1.0.0", "- Threads", URL), "• Threads")

    def test_candidate_notice(self):
        text = notes.play_notes("1.0.0-rc.2", "- Threads", URL)
        self.assertEqual(text, "Release candidate 1.0.0-rc.2, for testing.\n\n• Threads")

    def test_empty_highlights(self):
        self.assertEqual(notes.play_notes("1.0.0", "", URL), "TeamCraft 1.0.0")


class Truncate(unittest.TestCase):
    def test_short_text_unchanged(self):
        self.assertEqual(notes.truncate("• Threads", URL), "• Threads")

    def test_exact_limit_unchanged(self):
        text = "x" * notes.MAX_LENGTH
        self.assertEqual(notes.truncate(text, URL), text)

    def test_long_text_cut_at_a_line_with_link(self):
        text = "\n".join(f"• Highlight number {n}" for n in range(60))
        result = notes.truncate(text, URL)
        self.assertLessEqual(notes.length(result), notes.MAX_LENGTH)
        self.assertTrue(result.endswith(f"\n… {URL}"))
        body = result.removesuffix(f"\n… {URL}")
        self.assertTrue(text.startswith(body + "\n"), "cut at a line break")

    def test_one_long_line_cut_at_a_word(self):
        text = "word " * 200
        result = notes.truncate(text, URL)
        self.assertLessEqual(notes.length(result), notes.MAX_LENGTH)
        self.assertTrue(result.removesuffix(f"\n… {URL}").endswith("word"))

    def test_no_break_cuts_hard(self):
        result = notes.truncate("x" * 1000, URL)
        self.assertEqual(notes.length(result), notes.MAX_LENGTH)

    def test_emoji_count_twice_and_are_never_split(self):
        result = notes.truncate("🎉" * 400, URL)
        self.assertLessEqual(notes.length(result), notes.MAX_LENGTH)
        self.assertNotIn("�", result.encode("utf-16-le", "surrogatepass").decode("utf-16-le"))
        self.assertTrue(result.startswith("🎉"))

    def test_overlong_link_falls_back_to_ellipsis(self):
        long_url = "https://example.com/" + "a" * 600
        result = notes.truncate("x" * 1000, long_url)
        self.assertEqual(notes.length(result), notes.MAX_LENGTH)
        self.assertTrue(result.endswith("…"))


if __name__ == "__main__":
    unittest.main()
