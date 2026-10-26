"""Tests of scripts/merge-checksums.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "merge-checksums.py"
SPEC = importlib.util.spec_from_file_location("merge_checksums", SCRIPT)
checksums = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checksums)

WEB = "a" * 64 + "  teamcraft-web-1.0.8.tar.gz"
DEB = "b" * 64 + "  TeamCraft_1.0.8_amd64.deb"
APK = "c" * 64 + "  teamcraft-android-1.0.8-full-release.apk"
APK_REBUILT = "d" * 64 + "  teamcraft-android-1.0.8-full-release.apk"
AAB = "e" * 64 + " *teamcraft-android-1.0.8-full-release.aab"


class MergeChecksumsTest(unittest.TestCase):
    def setUp(self):
        self.dir = pathlib.Path(tempfile.mkdtemp())

    def test_android_after_the_draft_release_keeps_the_web_line(self):
        out = self.run_script(lines(WEB, DEB), lines(APK, AAB))
        self.assertEqual(out, lines(DEB, WEB, APK, AAB, ordered=True))

    def test_rerun_of_the_draft_release_keeps_the_android_lines(self):
        out = self.run_script(lines(WEB, APK), lines(WEB, DEB))
        self.assertEqual(out, lines(WEB, DEB, APK, ordered=True))

    def test_new_line_replaces_the_old_one_of_the_same_file(self):
        out = self.run_script(lines(WEB, APK), lines(APK_REBUILT))
        self.assertEqual(out, lines(WEB, APK_REBUILT, ordered=True))

    def test_missing_existing_file_counts_as_empty(self):
        new = self.dir / "new"
        new.write_text(lines(WEB))
        result = run(self.dir / "absent", new)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, lines(WEB))

    def test_missing_new_file_fails(self):
        existing = self.dir / "existing"
        existing.write_text(lines(WEB))
        result = run(existing, self.dir / "absent")
        self.assertEqual(result.returncode, 1)
        self.assertIn("doesn't exist", result.stderr)

    def test_malformed_line_fails(self):
        with self.assertRaises(checksums.ChecksumError):
            checksums.parse("not a checksum\n", "SHA256SUMS")

    def test_truncated_hash_fails(self):
        with self.assertRaises(checksums.ChecksumError):
            checksums.parse("abc  teamcraft-web-1.0.8.tar.gz\n", "SHA256SUMS")

    def test_file_listed_twice_fails(self):
        with self.assertRaises(checksums.ChecksumError):
            checksums.parse(lines(APK, APK_REBUILT), "SHA256SUMS")

    def test_empty_lines_are_ignored(self):
        self.assertEqual(checksums.parse(f"\n{WEB}\n\n", "SHA256SUMS"), {"teamcraft-web-1.0.8.tar.gz": WEB})

    def run_script(self, existing_text, new_text):
        existing = self.dir / "existing"
        new = self.dir / "new"
        existing.write_text(existing_text)
        new.write_text(new_text)
        result = run(existing, new)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout


def lines(*entries, ordered=False):
    """sha256sum text; ordered: the entries sorted by file name, as the script writes them."""
    if ordered:
        entries = sorted(entries, key=lambda line: line[66:])
    return "".join(f"{line}\n" for line in entries)


def run(existing, new):
    return subprocess.run(
        [sys.executable, str(SCRIPT), str(existing), str(new)], capture_output=True, text=True, check=False
    )


if __name__ == "__main__":
    unittest.main()
