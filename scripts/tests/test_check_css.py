"""Tests for scripts/check-css.py (issue #363)."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / "check-css.py"
spec = importlib.util.spec_from_file_location("check_css", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


def findings(css):
    return check.check_sources({"x.css": css})


class UndefinedVariablesTest(unittest.TestCase):
    def test_an_undefined_variable_is_found_even_with_a_fallback(self):
        css = ":root { --a: red; }\n.x { color: var(--a); background: var(--b, #fff); }\n"
        self.assertEqual(findings(css), ["x.css:2: var(--b) is never defined"])

    def test_runtime_variables_pass(self):
        self.assertEqual(findings(".x { height: var(--vv-height); color: hsl(var(--hue) 40% 50%); }"), [])

    def test_a_variable_defined_in_a_media_block_counts(self):
        self.assertEqual(findings("@media (min-width: 1px) { :root { --a: 1px; } }\n.x { width: var(--a); }"), [])


class KeyframesTest(unittest.TestCase):
    def test_a_duplicate_name_is_found(self):
        css = "@keyframes pop { from { opacity: 0; } }\n@keyframes pop { to { opacity: 1; } }\n"
        self.assertEqual(findings(css), ["x.css:2: @keyframes pop is already defined at x.css:1"])

    def test_keyframe_steps_are_not_rules(self):
        css = "@keyframes a { from { opacity: 0; } }\n@keyframes b { from { opacity: 1; } }\n"
        self.assertEqual(findings(css), [])


class OverridesTest(unittest.TestCase):
    def test_the_same_selector_with_a_different_value_is_found(self):
        css = ".icon {\n  width: 56px;\n}\n.icon { width: 40px; }\n"
        self.assertEqual(
            findings(css),
            ["x.css:4: `.icon` sets width: 40px but x.css:2 already sets it to 56px; merge the rules"],
        )

    def test_a_selector_list_counts_for_each_selector(self):
        css = ".field { gap: 6px; }\n.form, .field { gap: 8px; }\n"
        self.assertEqual(len(findings(css)), 1)

    def test_refining_a_group_passes(self):
        css = ".a::before, .a::after { flex: 1; }\n.a::after { flex: 0 0 24px; }\n"
        self.assertEqual(findings(css), [])

    def test_the_same_value_and_media_overrides_pass(self):
        css = ".x { gap: 4px; }\n.x { gap: 4px; }\n@media (hover: none) { .x { gap: 8px; } }\n"
        self.assertEqual(findings(css), [])

    def test_comments_and_strings_do_not_confuse_the_parser(self):
        css = '/* .x { gap: 1px; } */\n.x::before { content: "{;}"; gap: 2px; }\n.x { gap: 3px; }\n'
        self.assertEqual(findings(css), [])


class TinyFontsTest(unittest.TestCase):
    def test_sizes_below_the_ratchet_fail(self):
        css = "".join(f".f{i} {{ font-size: 9px; }}\n" for i in range(check.MAX_TINY_FONT_SIZES + 1))
        self.assertTrue(findings(css)[0].startswith(f"{check.MAX_TINY_FONT_SIZES + 1} font sizes below 11px"))

    def test_the_font_shorthand_reads_the_size_not_the_line_height(self):
        css = "".join(f".f{i} {{ font: 12px/10px sans-serif; }}\n" for i in range(check.MAX_TINY_FONT_SIZES + 1))
        self.assertEqual(findings(css), [])


if __name__ == "__main__":
    unittest.main()
