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

    def test_a_timer_callback_is_checked_as_a_whole(self):
        source = "set_timeout(\n  move || value.set(1),\n  DELAY,\n);\n"
        self.assertEqual(findings(source), ["x.rs:2: move || value.set(1),"])

    def test_typed_js_closures_and_animation_frames_are_checked(self):
        source = (
            "let f = Closure::<dyn FnMut()>::new(move || flag.set(true));\n"
            "request_animation_frame(move || list.update(|l| l.clear()));\n"
        )
        self.assertEqual(len(findings(source)), 2)

    def test_blur_and_focusout_handlers_are_checked_as_a_whole(self):
        source = (
            "<div\n  on:blur=move |_| {\n    if let Some(e) = editor.get_untracked() {\n"
            "      saved.set_value(1);\n    }\n  }\n"
            "  on:focusout=move |_| hidden.set(true)\n"
            "  on:click=move |_| clicked.set(true)\n>\n"
        )
        self.assertEqual(
            findings(source),
            [
                "x.rs:3: if let Some(e) = editor.get_untracked() {",
                "x.rs:4: saved.set_value(1);",
                "x.rs:7: on:focusout=move |_| hidden.set(true)",
            ],
        )

    def test_guarded_blur_handlers_pass(self):
        source = "on:blur=move |_| {\n  if let Some(e) = editor.try_get_untracked().flatten() {\n    saved.try_set_value(1);\n  }\n}\n"
        self.assertEqual(findings(source), [])

    def test_async_fn_bodies_are_checked_after_their_first_await(self):
        source = "async fn load(&self) {\n  self.busy.set(true);\n  fetch().await;\n  self.data.set(1);\n}\n"
        self.assertEqual(findings(source), ["x.rs:4: self.data.set(1);"])

    def test_a_closure_inside_a_task_is_reported_once(self):
        # Issue #344: the access lies in both bodies, the task's and the closure's.
        source = "spawn_local(async move {\n  load().await;\n  let f = Closure::new(move || flag.set(true));\n});\n"
        self.assertEqual(findings(source), ["x.rs:3: let f = Closure::new(move || flag.set(true));"])


if __name__ == "__main__":
    unittest.main()
