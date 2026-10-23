"""Tests of scripts/check-no-demo-data.py and of the build settings it relies on:
python3 -m unittest discover -s scripts/tests"""
import importlib.util
import io
import pathlib
import re
import tarfile
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "check-no-demo-data.py"
SPEC = importlib.util.spec_from_file_location("check_no_demo_data", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

UI = ROOT / "crates" / "teamcraft-ui"
# Everything that builds a shipped UI bundle or app.
RELEASE_BUILD_FILES = [
    *sorted((ROOT / ".github" / "workflows").glob("*.yml")),
    *sorted((ROOT / "apps" / "desktop").glob("tauri*.json")),
    *sorted((ROOT / "apps" / "desktop" / "packaging").rglob("*.sh")),
    *sorted((ROOT / "apps" / "desktop" / "packaging").rglob("*.yml")),
    UI / "index.html",
    UI / "Trunk.toml",
]


class ScanTest(unittest.TestCase):
    def test_a_clean_bundle_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = pathlib.Path(directory)
            (bundle / "teamcraft-ui_bg.wasm").write_bytes(b"\x00asm Welcome to TeamCraft")
            (bundle / "index.html").write_text("<html></html>")
            self.assertEqual(run([bundle]), 0)

    def test_a_bundle_with_demo_people_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = pathlib.Path(directory)
            (bundle / "nested").mkdir()
            (bundle / "nested" / "teamcraft-ui_bg.wasm").write_bytes(b"\x00asm\x01Riley Guest\x00Kim Novak")
            self.assertEqual(run([bundle]), 1)

    def test_an_archive_with_demo_spaces_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = pathlib.Path(directory)
            (folder / "app.wasm").write_bytes(b"\x00asm Project A")
            archive = folder / "teamcraft-web-1.0.0.tar.gz"
            with tarfile.open(archive, "w:gz") as tar:
                tar.add(folder / "app.wasm", arcname="./app.wasm")
            self.assertEqual(run([archive]), 1)

    def test_nothing_to_scan_is_an_error(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(run([pathlib.Path(directory)]), 2)
            self.assertEqual(run([pathlib.Path(directory) / "missing"]), 2)


class BuildSettingsTest(unittest.TestCase):
    def test_every_fixture_is_demo_data(self):
        # A renamed fixture would silently disable the check.
        mock = (UI / "src" / "mock.rs").read_text()
        for fixture in check.DEMO_FIXTURES:
            self.assertIn(f'"{fixture}"', mock, fixture)

    def test_the_demo_feature_is_off_by_default(self):
        manifest = (UI / "Cargo.toml").read_text()
        default = re.search(r"^default\s*=\s*\[(.*?)\]", manifest, re.MULTILINE | re.DOTALL)
        self.assertTrue(default is None or "demo" not in default.group(1))

    def test_release_builds_never_enable_the_demo_feature(self):
        enabling = re.compile(r"(--features|--all-features|cargo-features|features\s*=)[^\n]*\bdemo\b")
        for path in RELEASE_BUILD_FILES:
            for number, line in enumerate(path.read_text().splitlines(), start=1):
                self.assertIsNone(enabling.search(line), f"{path.relative_to(ROOT)}:{number}: {line.strip()}")


def run(paths):
    with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
        return check.main([str(path) for path in paths])


if __name__ == "__main__":
    unittest.main()
