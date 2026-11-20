"""Tests of apps/desktop/packaging/macos/verify-app.sh, the keychain identity check (#1223):
python3 -m unittest discover -s scripts/tests

macOS only (codesign). They sign a tiny app ad hoc and, on a Mac whose keychain has an Apple code
signing certificate, with that; the release job runs the same check on the real app."""
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

MACOS = pathlib.Path(__file__).resolve().parent.parent.parent / "apps/desktop/packaging/macos"
VERIFY = MACOS / "verify-app.sh"
IDENTIFIER = "de.zeitgleich.teamcraft.signature-test"
INFO = f"""<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{IDENTIFIER}</string>
<key>CFBundleExecutable</key><string>app</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
"""


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("codesign"), "needs macOS codesign")
class MacosSignatureTest(unittest.TestCase):
    def setUp(self):
        self.dir = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)

    def test_two_ad_hoc_builds_are_strangers_to_each_other(self):
        # Why an update asked for the keychain password: the requirement is the build's own hash.
        first = requirement(self.app("one", "build 1"))
        second = requirement(self.app("two", "build 2"))
        self.assertIn("cdhash", first)
        self.assertIn("cdhash", second)
        self.assertNotEqual(first, second)

    def test_an_ad_hoc_build_passes_with_a_note(self):
        result = verify(self.app("one", "build 1"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("asks for the login password once after each update", result.stdout)

    def test_a_release_with_a_certificate_must_not_come_out_ad_hoc(self):
        app = self.app("one", "build 1")
        for env in ({"MACOS_SIGNING_IDENTITY": "Developer ID Application: Example (ABCDE12345)"}, {"MACOS_TEAM_ID": "ABCDE12345"}):
            result = verify(app, env)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("names no team", result.stderr)

    def test_a_team_certificate_makes_two_builds_the_same_app(self):
        # Any certificate Apple issued for code signing shows it; the release uses a Developer ID.
        identity = apple_identity()
        if identity is None:
            self.skipTest("no Apple code signing certificate in the keychain")
        first, second = self.app("one", "build 1", identity), self.app("two", "build 2", identity)
        self.assertNotIn("cdhash", requirement(first))
        self.assertEqual(requirement(first), requirement(second))
        result = verify(first, {"MACOS_SIGNING_IDENTITY": identity})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        result = verify(first, {"MACOS_SIGNING_IDENTITY": identity, "MACOS_TEAM_ID": "ABCDE12345"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("not by ABCDE12345", result.stderr)

    def app(self, name, content, identity="-"):
        app = self.dir / name / "Test.app"
        (app / "Contents/MacOS").mkdir(parents=True)
        (app / "Contents/Info.plist").write_text(INFO)
        # /usr/bin/true with different bytes appended would break its own signature; a script is an
        # executable codesign seals as it is.
        executable = app / "Contents/MacOS/app"
        executable.write_text(f"#!/bin/sh\n# {content}\n")
        executable.chmod(0o755)
        subprocess.run(["codesign", "--force", "--sign", identity, str(app)], check=True, capture_output=True, timeout=60)
        return app


def apple_identity():
    out = subprocess.run(["security", "find-identity", "-v", "-p", "codesigning"], capture_output=True, text=True)
    for line in out.stdout.splitlines():
        name = line.partition('"')[2].rstrip('"')
        if name.startswith(("Developer ID Application: ", "Apple Development: ", "Apple Distribution: ")):
            return name
    return None


def requirement(app):
    out = subprocess.run(["codesign", "--display", "--requirements", "-", str(app)], check=True, capture_output=True, text=True)
    return next(line for line in out.stdout.splitlines() if "designated => " in line)


def verify(app, env=None):
    clean = {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", **(env or {})}
    return subprocess.run([str(VERIFY), str(app)], env=clean, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
