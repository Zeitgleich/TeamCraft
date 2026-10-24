"""Tests for scripts/check-async-signals.py (issue #322)."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "check-async-signals.py"
spec = importlib.util.spec_from_file_location("check_async_signals", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def findings(source):
    return check.check_source("x.rs", source)


class CheckAsyncSignalsTest(unittest.TestCase):
    def test_a_write_after_an_await_is_found(self):
        source = "spawn_local(async move {\n  let x = load().await;\n  value.set(x);\n});\n"
        self.assertEqual(findings(source), ["x.rs:3: value.set(x);"])

    def test_try_and_if_alive_forms_pass(self):
        source = (
            "spawn_local(async move {\n  let x = load().await;\n  value.set_if_alive(x);\n"
            "  let _ = other.try_set(1);\n  more.try_update(|v| *v += 1);\n});\n"
        )
        self.assertEqual(findings(source), [])

    def test_writes_before_the_first_await_pass(self):
        source = "spawn_local(async move {\n  busy.set(true);\n  load().await;\n});\n"
        self.assertEqual(findings(source), [])

    def test_a_write_of_an_awaited_value_is_found(self):
        source = "spawn_local(async move { status.set(load().await) });\n"
        self.assertEqual(findings(source), ["x.rs:1: spawn_local(async move { status.set(load().await) });"])

    def test_map_lookups_and_marked_lines_pass(self):
        source = (
            "spawn_local(async move {\n  load().await;\n  let v = map.get(&key);\n"
            "  // async-ok: a Cell\n  cell.set(1);\n});\n"
        )
        self.assertEqual(findings(source), [])

    def test_tasks_without_await_and_test_modules_are_skipped(self):
        source = "spawn_local(async move { value.set(1) });\n#[cfg(test)]\nspawn_local(async { x.await; v.set(1) });\n"
        self.assertEqual(findings(source), [])


if __name__ == "__main__":
    unittest.main()
