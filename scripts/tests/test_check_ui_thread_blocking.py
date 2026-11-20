"""Tests for scripts/check-ui-thread-blocking.py (issue #2184)."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "check-ui-thread-blocking.py"
spec = importlib.util.spec_from_file_location("check_ui_thread_blocking", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def swift(source):
    return check.check_source("x.swift", source)


def kotlin(source):
    return check.check_source("x.kt", source)


class CheckUiThreadBlockingTest(unittest.TestCase):
    def test_swift_waits_are_found(self):
        for line in [
            "queue.sync { load() }",
            "let semaphore = DispatchSemaphore(value: 0)",
            "_ = semaphore.wait(timeout: .now() + 5)",
            "group.wait()",
            "Thread.sleep(forTimeInterval: 0.05)",
            "_ = lock.acquire(timeout: 2)",
            "let token = vault.currentAccessToken()",
            "let suspended = core.suspend()",
        ]:
            with self.subTest(line=line):
                self.assertEqual(len(swift(line + "\n")), 1)

    def test_kotlin_waits_are_found(self):
        for line in ["val x = runBlocking { load() }", "Thread.sleep(50)", "val latch = CountDownLatch(1)"]:
            with self.subTest(line=line):
                self.assertEqual(len(kotlin(line + "\n")), 1)

    def test_a_marker_on_the_line_or_above_passes(self):
        self.assertEqual(swift("queue.sync { load() } // blocking-ok: the core's thread\n"), [])
        self.assertEqual(kotlin("// blocking-ok: FCM's own thread\nrunBlocking { handle() }\n"), [])

    def test_imports_comments_and_look_alikes_pass(self):
        self.assertEqual(kotlin("import kotlinx.coroutines.runBlocking\n * both use runBlocking\n"), [])
        self.assertEqual(swift("await self.sync()\nTask { await settings.sync() }\n// queue.sync { }\n"), [])
        self.assertEqual(swift("try await core.resume()\nlet x = await Task.sleep(for: .seconds(1))\n"), [])

    def test_test_code_is_left_out(self):
        files = [pathlib.Path(p) for p in ["apps/ios/TeamCraftTests/A.swift", "apps/android/app/src/test/B.kt"]]
        self.assertTrue(all(any(part in check.TEST_PARTS or part.endswith("Tests") for part in f.parts) for f in files))


if __name__ == "__main__":
    unittest.main()
