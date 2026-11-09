"""Tests of crates/teamcraft-ui/scripts/finish-loader.sh: python3 -m unittest discover -s scripts/tests"""
import hashlib
import json
import os
import pathlib
import re
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
UI = ROOT / "crates" / "teamcraft-ui"
SCRIPT = UI / "scripts" / "finish-loader.sh"
LANGUAGES = sorted(path.name for path in (UI / "i18n").iterdir() if path.is_dir())


class FinishLoaderTest(unittest.TestCase):
    """The web app embeds no texts (#1437): it gets each language as one file of the build."""

    def test_each_language_is_one_file_named_after_its_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            staging = staged(tmp)
            self.assertEqual(run(staging).returncode, 0)
            files = translation_files(staging)
            self.assertEqual(sorted(files), sorted(LANGUAGES + [f"{language}-admin" for language in LANGUAGES]))
            for name, url in files.items():
                match = re.fullmatch(rf"/i18n/{name}-([0-9a-f]{{16}})\.ftl", url)
                self.assertIsNotNone(match, url)
                data = (staging / url.lstrip("/")).read_bytes()
                self.assertEqual(hashlib.sha256(data).hexdigest()[:16], match.group(1))

    def test_a_languages_files_have_every_message_and_no_comment(self):
        """The console's texts are in the `-admin` file and only there (#1462)."""
        with tempfile.TemporaryDirectory() as tmp:
            staging = staged(tmp)
            run(staging)
            files = translation_files(staging)
            for language in LANGUAGES:
                texts = {name: (staging / files[name].lstrip("/")).read_text(encoding="utf-8")
                         for name in (language, f"{language}-admin")}
                for text in texts.values():
                    self.assertFalse([line for line in text.splitlines() if line.startswith("#")])
                sources = sorted((UI / "i18n" / language).glob("*.ftl"))
                expected = set()
                for source in sources:
                    if source.name != "admin.ftl":
                        expected |= message_ids(source.read_text(encoding="utf-8"))
                self.assertEqual(message_ids(texts[language]), expected, language)
                console = message_ids((UI / "i18n" / language / "admin.ftl").read_text(encoding="utf-8"))
                self.assertEqual(message_ids(texts[f"{language}-admin"]), console, language)

    def test_the_loading_screens_texts_stay_and_a_second_run_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            staging = staged(tmp)
            run(staging)
            init = (staging / "init.js").read_text(encoding="utf-8")
            self.assertTrue(init.startswith("globalThis.teamcraftLoaderTexts = "))
            self.assertIn("import init from 'app.js';", init)
            texts = json.loads(re.search(r"teamcraftLoaderTexts = (.*);\n", init).group(1))
            self.assertIn("loader-retry", texts["en"])
            again = run(staging)
            self.assertNotEqual(again.returncode, 0)
            self.assertIn("already has the texts", again.stderr)


def staged(tmp):
    staging = pathlib.Path(tmp) / "dist"
    staging.mkdir()
    (staging / "init.js").write_text("import init from 'app.js';\n", encoding="utf-8")
    return staging


def run(staging):
    return subprocess.run(["sh", str(SCRIPT)], env={**os.environ, "TRUNK_STAGING_DIR": str(staging)},
                          capture_output=True, text=True, check=False)


def translation_files(staging):
    init = (staging / "init.js").read_text(encoding="utf-8")
    return json.loads(re.search(r"teamcraftTranslationFiles = (.*);\n", init).group(1))


def message_ids(source):
    """As src/i18n.rs's tests: lines that start a message (not a term, attribute or comment)."""
    return {line.split(" =", 1)[0].strip() for line in source.splitlines()
            if line and not line.startswith((" ", "#", "-", ".")) and " =" in line}


if __name__ == "__main__":
    unittest.main()
