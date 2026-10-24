"""Tests of scripts/check-web-size.py: python3 -m unittest discover -s scripts/tests"""
import importlib.util
import io
import json
import os
import pathlib
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "check-web-size.py"
SPEC = importlib.util.spec_from_file_location("check_web_size", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)

APP = "teamcraft-ui-0123abcd_bg.wasm"
SERVICE_WORKER = "teamcraft-web-sw-4567ef_bg.wasm"


class BudgetTest(unittest.TestCase):
    def test_within_the_budget_passes_and_prints_the_sizes(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            code, out, _ = run(str(dist), "--budget-bytes", "1000")
        self.assertEqual(code, 0)
        self.assertIn("| app ", out)
        self.assertIn("| total ", out)
        self.assertIn("headroom", out)

    def test_over_the_budget_fails_counting_both_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=500)
            code, _, err = run(str(dist), "--budget-bytes", "1000")
        self.assertEqual(code, 1)
        self.assertIn("1,100 bytes", err)

    def test_over_the_budget_but_under_the_maximum_only_warns(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=500)
            code, out, _ = run(str(dist), "--budget-bytes", "1000", "--max-bytes", "1200")
        self.assertEqual(code, 0)
        self.assertIn("::warning::", out)

    def test_over_the_maximum_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=500)
            code, _, err = run(str(dist), "--budget-bytes", "1000", "--max-bytes", "1050")
        self.assertEqual(code, 1)
        self.assertIn("maximum of 1,050", err)

    def test_a_missing_file_is_an_error_not_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            (pathlib.Path(tmp) / APP).write_bytes(b"\0" * 10)
            code, _, err = run(tmp)
        self.assertEqual(code, 2)
        self.assertIn("teamcraft-web-sw", err)


class BaselineTest(unittest.TestCase):
    def test_the_delta_to_the_baseline_is_printed_and_the_sizes_written(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=3000, service_worker_bytes=100)
            baseline = pathlib.Path(tmp) / "main.json"
            baseline.write_text(json.dumps({
                "app": {"bytes": 1000, "gzip_bytes": 10},
                "service_worker": {"bytes": 100, "gzip_bytes": 10},
            }))
            written = pathlib.Path(tmp) / "sizes.json"
            code, out, _ = run(str(dist), "--baseline", str(baseline), "--write", str(written))
            sizes = json.loads(written.read_text())
        self.assertEqual(code, 0)
        self.assertIn("+2.0 kB", out)
        self.assertEqual(sizes["app"]["bytes"], 3000)

    def test_an_unreadable_baseline_only_drops_the_delta(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=10, service_worker_bytes=10)
            baseline = pathlib.Path(tmp) / "main.json"
            baseline.write_text("not json")
            code, out, _ = run(str(dist), "--baseline", str(baseline))
        self.assertEqual(code, 0)
        self.assertIn("–", out)


def dist_with(tmp, app_bytes, service_worker_bytes):
    dist = pathlib.Path(tmp) / "dist"
    dist.mkdir()
    (dist / APP).write_bytes(os.urandom(app_bytes))
    (dist / SERVICE_WORKER).write_bytes(os.urandom(service_worker_bytes))
    return dist


def run(*argv):
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": ""}), redirect_stdout(out), redirect_stderr(err):
        code = check.main(list(argv))
    return code, out.getvalue(), err.getvalue()


if __name__ == "__main__":
    unittest.main()
