"""Tests of scripts/check-web-size.py: python3 -m unittest discover -s scripts/tests"""
import gzip
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
    """Random bytes don't compress: gzip -9 is a little above the uncompressed size."""

    def test_within_the_budget_passes_and_prints_the_sizes(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            code, out, _ = run(str(dist), "--budget-bytes", "2000")
        self.assertEqual(code, 0)
        self.assertIn("| app ", out)
        self.assertIn("| total ", out)
        self.assertIn("gzip -9; headroom", out)
        self.assertNotIn("::warning::", out)

    def test_over_the_compressed_budget_fails_counting_both_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=500)
            code, _, err = run(str(dist), "--budget-bytes", "1000")
        self.assertEqual(code, 1)
        self.assertIn("gzip -9, over the budget of 1,000", err)

    def test_lazy_chunks_are_listed_but_not_counted(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            (dist / "teamcraft-ui-split-admin-0123abcd.wasm").write_bytes(os.urandom(5000))
            code, out, _ = run(str(dist), "--budget-bytes", "2000")
        self.assertEqual(code, 0)
        self.assertIn("Loaded when first used", out)
        self.assertIn("teamcraft-ui-split-admin-0123abcd.wasm", out)

    def test_close_to_the_compressed_budget_only_warns(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=500)
            gzip_total = gzip_total_of(dist)
            code, out, _ = run(str(dist), "--budget-bytes", str(gzip_total + 10))
        self.assertEqual(code, 0)
        self.assertIn("::warning::", out)

    def test_compressible_wasm_is_judged_by_its_compressed_size(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600_000, service_worker_bytes=50_000, compressible=True)
            code, out, _ = run(str(dist), "--budget-bytes", "100000")
        self.assertEqual(code, 0)
        self.assertNotIn("::warning::", out)

    def test_the_signed_in_app_counts_and_a_guest_visit_is_printed(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            (dist / "teamcraft-ui-split-session-0123abcd.wasm").write_bytes(os.urandom(700))
            (dist / "teamcraft-ui-split-admin-0123abcd.wasm").write_bytes(os.urandom(5000))
            (dist / "init.js").write_text(
                "for (const href of ['/teamcraft-ui-split-session-0123abcd.wasm']) {}\n")
            code, out, err = run(str(dist), "--budget-bytes", "1500")
        self.assertEqual(code, 1)
        self.assertIn("| signed-in app ", out)
        self.assertIn("A guest's first visit", out)
        self.assertIn("over the budget of 1,500", err)
        lazy = out.split("Loaded when first used")[1]
        self.assertNotIn("split-session", lazy)
        self.assertIn("split-admin", lazy)

    def test_a_listed_session_file_that_is_missing_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=10, service_worker_bytes=10)
            (dist / "init.js").write_text("for (const href of ['/gone.wasm']) {}\n")
            code, _, err = run(str(dist))
        self.assertEqual(code, 2)
        self.assertIn("gone.wasm", err)

    def test_an_init_js_without_the_session_list_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=10, service_worker_bytes=10)
            (dist / "init.js").write_text("import init from '/app.js';\n")
            code, _, err = run(str(dist))
        self.assertEqual(code, 2)
        self.assertIn("signed-in app", err)

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
            gzip_total = gzip_total_of(dist)
            baseline = pathlib.Path(tmp) / "main.json"
            baseline.write_text(json.dumps({
                "app": {"bytes": 1000, "gzip_bytes": gzip_total - 2100},
                "service_worker": {"bytes": 100, "gzip_bytes": 100},
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


def dist_with(tmp, app_bytes, service_worker_bytes, compressible=False):
    dist = pathlib.Path(tmp) / "dist"
    dist.mkdir()
    content = (lambda size: b"\0" * size) if compressible else os.urandom
    (dist / APP).write_bytes(content(app_bytes))
    (dist / SERVICE_WORKER).write_bytes(content(service_worker_bytes))
    return dist


def gzip_total_of(dist):
    return sum(len(gzip.compress(path.read_bytes(), compresslevel=9)) for path in dist.iterdir())


def run(*argv):
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": ""}), redirect_stdout(out), redirect_stderr(err):
        code = check.main(list(argv))
    return code, out.getvalue(), err.getvalue()


if __name__ == "__main__":
    unittest.main()
