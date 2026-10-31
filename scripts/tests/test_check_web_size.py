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
        self.assertIn("headroom", out)
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

    def test_the_call_module_has_a_budget_of_its_own(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            (dist / "teamcraft-web-calls-0123abcd_bg.wasm").write_bytes(os.urandom(3000))
            (dist / "teamcraft-web-segmentation-0123abcd_bg.wasm").write_bytes(os.urandom(5000))
            code, out, err = run(str(dist), "--budget-bytes", "2000", "--call-module-budget-bytes", "4000")
            self.assertEqual(code, 0, err)
            self.assertIn("teamcraft-web-segmentation-0123abcd_bg.wasm (on demand)", out)
            code, _, err = run(str(dist), "--budget-bytes", "2000", "--call-module-budget-bytes", "2000")
        self.assertEqual(code, 1)
        self.assertIn("the call module teamcraft-web-calls-0123abcd_bg.wasm is", err)

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
        self.assertIn("A guest's meeting visit", out)
        self.assertIn("over the budget of 1,500", err)
        lazy = out.split("Loaded when first used")[1]
        self.assertNotIn("split-session", lazy)
        self.assertIn("split-admin", lazy)

    def test_a_guest_visit_counts_the_call_module_but_not_the_signed_in_app(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            first_visit = gzip_total_of(dist)
            (dist / "teamcraft-web-calls-0123abcd_bg.wasm").write_bytes(os.urandom(400))
            (dist / "teamcraft-ui-split-session-0123abcd.wasm").write_bytes(os.urandom(5000))
            (dist / "init.js").write_text(
                "for (const href of ['/teamcraft-ui-split-session-0123abcd.wasm']) {}\n")
            guest = first_visit + len(gzip.compress((dist / "teamcraft-web-calls-0123abcd_bg.wasm").read_bytes(), 9))
            code, out, err = run(str(dist), "--guest-budget-bytes", str(guest), "--budget-bytes", "100000")
            self.assertEqual(code, 0, err)
            self.assertIn(f"meeting visit (lobby, meeting, call module): {guest / 1_000_000:.2f} MB", out)
            code, _, err = run(str(dist), "--guest-budget-bytes", str(guest - 1), "--budget-bytes", "100000")
        self.assertEqual(code, 1)
        self.assertIn("a guest's meeting visit is", err)

    def test_a_guest_visit_counts_the_meetings_modules_and_their_chunks_once(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            lobby = gzip_total_of(dist)
            files = {
                "teamcraft-ui-split-guest-0123abcd.wasm": 200,
                "teamcraft-ui-split-calls-0123abcd.wasm": 300,
                "teamcraft-ui-chunk-1-0123abcd.wasm": 400,
                "teamcraft-ui-split-session-0123abcd.wasm": 5000,
            }
            for name, size in files.items():
                (dist / name).write_bytes(os.urandom(size))
            (dist / "teamcraft-ui-split-0123abcd.js").write_text(split_loader({
                "guest": ["teamcraft-ui-split-guest-0123abcd.wasm", "__chunk_1"],
                "calls": ["teamcraft-ui-split-calls-0123abcd.wasm", "__chunk_1"],
                "session": ["teamcraft-ui-split-session-0123abcd.wasm"],
            }, {"__chunk_1": "teamcraft-ui-chunk-1-0123abcd.wasm"}))
            meeting = sum(len(gzip.compress((dist / name).read_bytes(), 9)) for name in list(files)[:3])
            code, out, err = run(str(dist), "--budget-bytes", "100000")
        self.assertEqual(code, 0, err)
        self.assertIn(f"meeting visit (lobby, meeting, call module): {(lobby + meeting) / 1_000_000:.2f} MB", out)

    def test_a_split_loader_without_the_guest_module_is_an_error(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=10, service_worker_bytes=10)
            (dist / "teamcraft-ui-split-calls-0123abcd.wasm").write_bytes(b"\0")
            (dist / "teamcraft-ui-split-0123abcd.js").write_text(
                split_loader({"calls": ["teamcraft-ui-split-calls-0123abcd.wasm"]}, {}))
            code, _, err = run(str(dist))
        self.assertEqual(code, 2)
        self.assertIn("no module guest", err)

    def test_a_signed_in_visit_over_its_ceiling_fails_while_the_guest_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            dist = dist_with(tmp, app_bytes=600, service_worker_bytes=300)
            (dist / "teamcraft-ui-split-session-0123abcd.wasm").write_bytes(os.urandom(5000))
            (dist / "init.js").write_text(
                "for (const href of ['/teamcraft-ui-split-session-0123abcd.wasm']) {}\n")
            code, _, err = run(str(dist), "--guest-budget-bytes", "100000", "--budget-bytes", "3000")
        self.assertEqual(code, 1)
        self.assertIn("the signed-in web wasm is", err)

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


def split_loader(modules, chunks):
    """A split loader script as teamcraft-web-bundle writes it: shared chunks, then each module's
    file and the chunks it imports."""
    parts = []
    for name, file in chunks.items():
        parts.append(f'const {name} = makeLoad(() => {{\n    const src = fetch(new URL("./{file}", '
                     f'import.meta.url));\n}}\n, []);\n')
    for module, (file, *deps) in modules.items():
        parts.append(f'export const __wasm_split_load_{module} = wrapAsyncCb(makeLoad(() => {{\n'
                     f'    const src = fetch(new URL("./{file}", import.meta.url));\n}}\n'
                     f', [{", ".join(deps)}]));\n')
    return "".join(parts)


def gzip_total_of(dist):
    return sum(len(gzip.compress(path.read_bytes(), compresslevel=9)) for path in dist.iterdir())


def run(*argv):
    out, err = io.StringIO(), io.StringIO()
    with mock.patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": ""}), redirect_stdout(out), redirect_stderr(err):
        code = check.main(list(argv))
    return code, out.getvalue(), err.getvalue()


if __name__ == "__main__":
    unittest.main()
