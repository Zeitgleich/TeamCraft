"""Tests of scripts/updater-manifest.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "updater-manifest.py"
SPEC = importlib.util.spec_from_file_location("updater_manifest", SCRIPT)
manifest = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(manifest)

REPO = "Example-Org/TeamCraft"
TAG = "v1.0.1"
DATE = "2026-11-02T10:00:00Z"
RELEASE_FILES = [
    "TeamCraft_1.0.1_x64-setup.exe",
    "TeamCraft_1.0.1_arm64-setup.exe",
    "TeamCraft_1.0.1_x64_en-US.msi",
    "TeamCraft_1.0.1_arm64_en-US.msi",
    "TeamCraft_1.0.1_universal.app.tar.gz",
    "TeamCraft_1.0.1_amd64.AppImage",
    "TeamCraft_1.0.1_aarch64.AppImage",
]
# Released next to them, never in the manifest.
OTHER_FILES = [
    "TeamCraft_1.0.1_amd64.deb",
    "TeamCraft-1.0.1-1.x86_64.rpm",
    "TeamCraft_1.0.1_universal.dmg",
    "teamcraft-desktop-x86_64.flatpak",
    "SHA256SUMS",
]


class UpdaterManifestTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def release(self, signed=RELEASE_FILES, unsigned=OTHER_FILES):
        for name in signed:
            (self.dir / name).write_bytes(b"artifact")
            (self.dir / f"{name}.sig").write_text(f"sig-of-{name}\n")
        for name in unsigned:
            (self.dir / name).write_bytes(b"other")

    def build(self, version="1.0.1", require_all=True):
        return manifest.build_manifest(self.dir, version, TAG, REPO, DATE, require_all)

    def test_a_full_release_maps_every_target_with_url_and_signature(self):
        self.release()
        result = self.build()
        self.assertEqual(result["version"], "1.0.1")
        self.assertEqual(result["notes"], f"https://github.com/{REPO}/releases/tag/{TAG}")
        self.assertEqual(result["pub_date"], DATE)
        self.assertEqual(sorted(result["platforms"]), manifest.ALL_TARGETS)
        nsis = result["platforms"]["windows-x86_64-nsis"]
        self.assertEqual(
            nsis["url"], f"https://github.com/{REPO}/releases/download/{TAG}/TeamCraft_1.0.1_x64-setup.exe"
        )
        self.assertEqual(nsis["signature"], "sig-of-TeamCraft_1.0.1_x64-setup.exe")
        self.assertEqual(
            result["platforms"]["linux-aarch64-appimage"]["url"].rsplit("/", 1)[1],
            "TeamCraft_1.0.1_aarch64.AppImage",
        )

    def test_the_universal_mac_app_serves_both_architectures(self):
        self.release()
        platforms = self.build()["platforms"]
        self.assertEqual(platforms["darwin-x86_64-app"], platforms["darwin-aarch64-app"])
        self.assertTrue(platforms["darwin-aarch64-app"]["url"].endswith("_universal.app.tar.gz"))

    def test_there_are_no_bare_os_arch_keys(self):
        self.release()
        for key in self.build()["platforms"]:
            self.assertEqual(len(key.split("-")), 3, key)
            self.assertNotIn("deb", key)
            self.assertNotIn("rpm", key)

    def test_candidates_announce_the_tag_version(self):
        self.release()
        self.assertEqual(self.build(version="1.0.0-rc.2")["version"], "1.0.0-rc.2")

    def test_missing_platforms_fail_only_when_all_are_required(self):
        self.release(signed=RELEASE_FILES[:2])
        with self.assertRaisesRegex(manifest.ManifestError, "missing update artifacts for darwin"):
            self.build()
        partial = self.build(require_all=False)
        self.assertEqual(sorted(partial["platforms"]), ["windows-aarch64-nsis", "windows-x86_64-nsis"])

    def test_an_unsigned_release_has_no_manifest(self):
        self.release(signed=[], unsigned=RELEASE_FILES)
        with self.assertRaisesRegex(manifest.ManifestError, "no signed update artifacts"):
            self.build(require_all=False)

    def test_a_signed_file_no_target_uses_is_an_error(self):
        self.release(signed=["TeamCraft_1.0.1_amd64.deb"], unsigned=[])
        with self.assertRaisesRegex(manifest.ManifestError, "no updater target uses it"):
            self.build(require_all=False)

    def test_a_signature_without_its_artifact_is_an_error(self):
        (self.dir / "TeamCraft_1.0.1_amd64.AppImage.sig").write_text("sig")
        with self.assertRaisesRegex(manifest.ManifestError, "no artifact next to it"):
            self.build(require_all=False)

    def test_an_empty_signature_is_an_error(self):
        self.release(signed=["TeamCraft_1.0.1_amd64.AppImage"], unsigned=[])
        (self.dir / "TeamCraft_1.0.1_amd64.AppImage.sig").write_text("\n")
        with self.assertRaisesRegex(manifest.ManifestError, "is empty"):
            self.build(require_all=False)

    def test_two_artifacts_for_one_target_are_an_error(self):
        self.release(signed=["TeamCraft_1.0.1_amd64.AppImage", "TeamCraft_1.0.0_amd64.AppImage"], unsigned=[])
        with self.assertRaisesRegex(manifest.ManifestError, "two artifacts for linux-x86_64-appimage"):
            self.build(require_all=False)

    def test_versions_and_repositories_are_checked(self):
        self.release()
        for bad in ["1.0", "v1.0.1", "latest", "1.0.1 "]:
            with self.assertRaisesRegex(manifest.ManifestError, "not a semantic version"):
                self.build(version=bad)
        with self.assertRaisesRegex(manifest.ManifestError, "not owner/name"):
            manifest.build_manifest(self.dir, "1.0.1", TAG, "no-slash", DATE, True)

    def test_file_names_are_url_encoded(self):
        name = "TeamCraft Beta_1.0.1_amd64.AppImage"
        self.release(signed=[name], unsigned=[])
        url = self.build(require_all=False)["platforms"]["linux-x86_64-appimage"]["url"]
        self.assertTrue(url.endswith("/TeamCraft%20Beta_1.0.1_amd64.AppImage"), url)

    def test_the_command_line_prints_json_and_fails_with_a_message(self):
        self.release()
        command = [sys.executable, str(SCRIPT), "--version", "1.0.1", "--tag", TAG, "--repo", REPO]
        command += ["--dir", str(self.dir), "--pub-date", DATE, "--require-all"]
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["version"], "1.0.1")

        empty = tempfile.TemporaryDirectory()
        self.addCleanup(empty.cleanup)
        command[command.index("--dir") + 1] = empty.name
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no signed update artifacts", result.stderr)


if __name__ == "__main__":
    unittest.main()
