"""Contrast of the colour tokens in main.css, light and dark (issues #361, #362).

WCAG 2.x contrast ratios: 4.5:1 for text (1.4.3), 3:1 for control edges, icons and focus rings
(1.4.11, 2.4.11). Pairs whose foreground or background is not a plain hex token are not covered.
"""
import pathlib
import re
import unittest

STYLESHEET = pathlib.Path(__file__).resolve().parents[2] / "crates/teamcraft-ui/style/main.css"
TEXT = 4.5
NON_TEXT = 3.0
# (foreground, background, minimum, what it is)
PAIRS = [
    ("text-muted", "surface", TEXT, "secondary text"),
    ("text-muted", "surface-2", TEXT, "secondary text on a hovered row"),
    ("accent-strong", "accent-soft", TEXT, "accent text on a tint (reply count, mention)"),
    ("accent-strong", "surface-2", TEXT, "links and reply counts on a hovered row"),
    ("on-accent", "accent", TEXT, "primary button, panic overlay reload"),
    ("on-danger", "danger", TEXT, "danger button"),
    ("sb-dim", "sb-bg", TEXT, "muted channel in the sidebar"),
    ("sb-muted", "sb-bg", TEXT, "channel in the sidebar"),
    ("border-strong", "surface", NON_TEXT, "field, switch and checkbox edges"),
    ("border-strong", "bg", NON_TEXT, "field edges on the page background"),
    ("star", "surface", NON_TEXT, "favourite star when on"),
    ("focus", "surface", NON_TEXT, "focus ring"),
    ("focus", "bg", NON_TEXT, "focus ring on the page background"),
    ("focus", "surface-2", NON_TEXT, "focus ring on a hovered row"),
]
DECLARATION = re.compile(r"(--[\w-]+)\s*:\s*([^;]+);")


class ContrastTest(unittest.TestCase):
    def test_light_tokens(self):
        self.check_theme("light", themes()["light"])

    def test_dark_tokens(self):
        self.check_theme("dark", themes()["dark"])

    def test_both_dark_blocks_agree(self):
        css = STYLESHEET.read_text(encoding="utf-8")
        self.assertEqual(tokens(block(css, "@media (prefers-color-scheme: dark)")), tokens(block(css, ':root[data-theme="dark"]')))

    def check_theme(self, theme, values):
        for foreground, background, minimum, what in PAIRS:
            with self.subTest(theme=theme, pair=f"{foreground} on {background}"):
                ratio = contrast(resolve(values, foreground), resolve(values, background))
                self.assertGreaterEqual(ratio, minimum, f"{what}: {ratio:.2f}:1 in {theme}")


def themes():
    css = STYLESHEET.read_text(encoding="utf-8")
    light = tokens(block(css, ":root {"))
    return {"light": light, "dark": {**light, **tokens(block(css, ':root[data-theme="dark"]'))}}


def block(css, opening):
    """The text of the first block that starts with `opening`, nested blocks included."""
    start = css.index("{", css.index(opening)) + 1
    depth, index = 1, start
    while depth:
        depth += {"{": 1, "}": -1}.get(css[index], 0)
        index += 1
    return css[start:index - 1]


def tokens(text):
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return {name: value.strip() for name, value in DECLARATION.findall(text)}


def resolve(values, name):
    value = values[f"--{name}"]
    while value.startswith("var("):
        value = values[value[4:].split(")")[0].split(",")[0].strip()]
    return value


def contrast(first, second):
    lighter, darker = sorted((luminance(first), luminance(second)), reverse=True)
    return (lighter + 0.05) / (darker + 0.05)


def luminance(hex_colour):
    channels = [int(hex_colour[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


if __name__ == "__main__":
    unittest.main()
