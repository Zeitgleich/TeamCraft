"""Tests of scripts/render-release-notes.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "render-release-notes.py"
SPEC = importlib.util.spec_from_file_location("render_release_notes", SCRIPT)
notes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notes)

HIGHLIGHTS = "- Faster channel switch\n- Threads in the sidebar"
BREAKING = "- `[push] gateway_url` is now required"
MESSAGE_WITH_BREAKING = f"{HIGHLIGHTS}\n\n### Breaking\n\n{BREAKING}\n"
IDENTITY = ("-c", "user.name=Test", "-c", "user.email=test@example.com")
SIGNATURE = "-----BEGIN PGP SIGNATURE-----\n\niQEzBAABCAAdFiEE\n=abcd\n-----END PGP SIGNATURE-----\n"


class SplitBreaking(unittest.TestCase):
    def test_without_breaking(self):
        self.assertEqual(notes.split_breaking(HIGHLIGHTS + "\n"), (HIGHLIGHTS, ""))

    def test_with_breaking(self):
        self.assertEqual(notes.split_breaking(MESSAGE_WITH_BREAKING), (HIGHLIGHTS, BREAKING))

    def test_breaking_between_sections(self):
        message = f"{HIGHLIGHTS}\n\n### Breaking\n\n{BREAKING}\n\n### Fixes\n\n- Upload retry\n"
        highlights, breaking = notes.split_breaking(message)
        self.assertEqual(breaking, BREAKING)
        self.assertEqual(highlights, f"{HIGHLIGHTS}\n\n### Fixes\n\n- Upload retry")

    def test_empty(self):
        self.assertEqual(notes.split_breaking(""), ("", ""))


class ReleaseMessage(unittest.TestCase):
    def test_release_uses_its_own_message(self):
        lookup = {"v1.2.0": "release"}.get
        self.assertEqual(notes.release_message("v1.2.0", "1.2.0", lambda t: lookup(t, "")), "release")

    def test_candidate_uses_its_own_message(self):
        tags = {"v1.2.0-rc.1": "candidate", "v1.2.0": "release"}
        self.assertEqual(notes.release_message("v1.2.0-rc.1", "1.2.0", lambda t: tags.get(t, "")), "candidate")

    def test_candidate_falls_back_to_the_base_tag(self):
        tags = {"v1.2.0-rc.2": "", "v1.2.0": "release"}
        self.assertEqual(notes.release_message("v1.2.0-rc.2", "1.2.0", lambda t: tags.get(t, "")), "release")

    def test_candidate_without_any_message(self):
        self.assertEqual(notes.release_message("v1.2.0-rc.1", "1.2.0", lambda t: ""), "")

    def test_release_never_falls_back(self):
        asked = []
        notes.release_message("v1.2.0", "1.2.0", lambda t: asked.append(t) or "")
        self.assertEqual(asked, ["v1.2.0"])


class StripSignature(unittest.TestCase):
    def test_pgp_signature(self):
        self.assertEqual(notes.strip_signature(MESSAGE_WITH_BREAKING + SIGNATURE), MESSAGE_WITH_BREAKING.strip())

    def test_ssh_signature(self):
        ssh = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n"
        self.assertEqual(notes.strip_signature(HIGHLIGHTS + "\n" + ssh), HIGHLIGHTS)

    def test_unsigned(self):
        self.assertEqual(notes.strip_signature(HIGHLIGHTS + "\n"), HIGHLIGHTS)


class TagMessage(unittest.TestCase):
    """Against a throwaway repository: annotated, lightweight and missing tags."""

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.git("init", "-q")
        self.git(*IDENTITY, "-c", "commit.gpgSign=false", "commit", "-q", "--allow-empty", "-m", "commit message")
        self.original_root = notes.ROOT
        notes.ROOT = pathlib.Path(self.dir.name)
        self.addCleanup(setattr, notes, "ROOT", self.original_root)

    def test_annotated_tag_keeps_headings(self):
        self.git(*IDENTITY, "-c", "tag.gpgSign=false", "tag", "-a", "--cleanup=whitespace", "v1.0.0", "-m", MESSAGE_WITH_BREAKING)
        self.assertEqual(notes.tag_message("v1.0.0"), MESSAGE_WITH_BREAKING.strip())

    def test_lightweight_tag_is_empty(self):
        self.git("-c", "tag.gpgSign=false", "tag", "v1.0.0")
        self.assertEqual(notes.tag_message("v1.0.0"), "")

    def test_missing_tag_is_empty(self):
        self.assertEqual(notes.tag_message("v9.9.9"), "")

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.dir.name, check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
