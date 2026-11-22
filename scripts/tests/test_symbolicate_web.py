"""Tests of scripts/symbolicate-web.py: python3 -m unittest discover -s scripts/tests"""
import gzip
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "symbolicate-web.py"
SPEC = importlib.util.spec_from_file_location("symbolicate_web", SCRIPT)
symbolicate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(symbolicate)

BUILD = "0123456789abcdef"
MAIN = f"teamcraft-ui-{BUILD}_bg.wasm"
ADMIN = f"teamcraft-ui-split-admin-{BUILD}.wasm"
CHUNK = f"teamcraft-ui-chunk-1-{BUILD}.wasm"
NAMES = {
    "format": 1,
    "build": BUILD,
    "modules": [
        {
            "file": MAIN,
            "imported_functions": 2,
            "functions": [
                {"start": 100, "end": 200, "name": "teamcraft_ui::app"},
                {"start": 200, "end": 300, "name": "teamcraft_ui::shell"},
                {"start": 300, "end": 400, "name": "core::panicking::panic"},
            ],
        },
        {
            "file": ADMIN,
            "imported_functions": 0,
            "functions": [{"start": 50, "end": 90, "name": "teamcraft_ui::admin::view"}],
        },
        {
            "file": CHUNK,
            "imported_functions": 0,
            "functions": [{"start": 10, "end": 40}, {"start": 40, "end": 80, "name": "teamcraft_ui::shared"}],
        },
    ],
}


class SymbolicateWebTest(unittest.TestCase):
    def test_a_chrome_frame_is_found_by_its_file_and_offset(self):
        frame = {"function": "teamcraft_ui.wasm.wasm-function[0]", "filename": ADMIN, "function_id": "0x0", "instruction_addr": "0x3c"}
        self.assertTrue(describe(frame).startswith("teamcraft_ui::admin::view  "))

    def test_an_older_report_keeps_the_position_in_the_file_name(self):
        frame = {"function": "wasm-function[3]", "filename": f"{MAIN}:wasm-function[3]:0xd2"}
        self.assertEqual(describe(frame), f"teamcraft_ui::shell  ({MAIN}, wasm-function[3] +0xd2)")

    def test_a_module_from_the_pack_is_found_by_index_and_offset(self):
        # Chrome names a module compiled from bytes by a hash: the offset picks the file.
        frame = {"function_id": "0x1", "instruction_addr": "0x30", "filename": "wasm/0a1b2c3d"}
        self.assertTrue(describe(frame).startswith(f"teamcraft_ui::shared  ({CHUNK}"))
        frame = {"function_id": "0x0", "instruction_addr": "0x14", "filename": f"teamcraft-ui-split-{BUILD}.js"}
        self.assertTrue(describe(frame).startswith(f"(unnamed)  ({CHUNK}"))

    def test_a_webkit_frame_with_a_large_index_is_the_main_files(self):
        self.assertTrue(describe({"function": "<?>.wasm-function[4]", "filename": "[wasm code]"}).startswith("core::panicking::panic"))

    def test_a_webkit_frame_with_a_small_index_lists_its_candidates(self):
        text = describe({"function": "<?>.wasm-function[0]", "filename": "[wasm code]"})
        self.assertTrue(text.startswith("one of 2: teamcraft_ui::admin::view"), text)
        self.assertIn(CHUNK, text)

    def test_an_import_or_another_build_is_said_so(self):
        self.assertIn("not in the map", describe({"function": "wasm-function[1]", "filename": MAIN}))
        self.assertIn("not in the map", describe({"function_id": "0x2", "instruction_addr": "0x999", "filename": MAIN}))

    def test_script_frames_pass_through(self):
        frame = {"function": "__wbg_adapter_12", "filename": f"teamcraft-ui-{BUILD}.js", "lineno": 3890, "colno": 14}
        self.assertEqual(describe(frame), f"__wbg_adapter_12  (teamcraft-ui-{BUILD}.js:3890:14)")
        self.assertIsNone(symbolicate.wasm_position({"function": "mywasm-function[3]"}))

    def test_the_build_is_read_from_the_frames(self):
        self.assertEqual(symbolicate.build_of([{"filename": "[wasm code]"}, {"filename": f"teamcraft-ui-{BUILD}.js"}]), BUILD)
        self.assertIsNone(symbolicate.build_of([{"filename": "[wasm code]"}]))

    def test_the_whole_report_from_a_directory_of_gzipped_maps(self):
        directory = pathlib.Path(tempfile.mkdtemp())
        with gzip.open(directory / f"teamcraft-ui-names-{BUILD}.json.gz", "wt", encoding="utf-8") as handle:
            json.dump(NAMES, handle)
        event = {
            "entries": [{"type": "exception", "data": {"values": [{"stacktrace": {"frames": [
                {"function": "<?>.wasm-function[4]", "filename": "[wasm code]"},
                {"function": "__wbg_adapter_12", "filename": f"teamcraft-ui-{BUILD}.js", "lineno": 1},
            ]}}]}}],
        }
        (directory / "event.json").write_text(json.dumps(event))
        output = subprocess.run(
            [sys.executable, str(SCRIPT), "--names", str(directory), str(directory / "event.json")],
            capture_output=True, text=True, check=True,
        ).stdout.splitlines()
        self.assertTrue(output[0].startswith("#0   __wbg_adapter_12"), output)
        self.assertTrue(output[1].startswith("#1   core::panicking::panic"), output)

    def test_a_map_of_another_format_is_refused(self):
        path = pathlib.Path(tempfile.mkdtemp()) / "map.json"
        path.write_text(json.dumps({**NAMES, "format": 2}))
        with self.assertRaises(SystemExit):
            symbolicate.load_map(path, BUILD)


def describe(frame):
    return symbolicate.describe(frame, NAMES)


if __name__ == "__main__":
    unittest.main()
