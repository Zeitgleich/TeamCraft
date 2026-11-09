"""Contrast of the colour tokens of the UI stylesheet, light and dark (issues #361, #362).

WCAG 2.x contrast ratios: 4.5:1 for text (1.4.3), 3:1 for control edges, icons and focus rings
(1.4.11, 2.4.11). Tokens are hex colours, `var()` references or `color-mix(in srgb, …)` of them;
pairs with anything else (rgba layers) are not covered.
"""
import pathlib
import re
import unittest

# The tokens and their dark theme are the first area file of the stylesheet (#614).
STYLESHEET = pathlib.Path(__file__).resolve().parents[2] / "crates/teamcraft-ui/style/main/000-tokens.css"
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
    ("sb-text", "sb-bg", TEXT, "text in the sidebar"),
    ("sb-strong", "sb-bg", TEXT, "unread channel in the sidebar"),
    ("border-strong", "surface", NON_TEXT, "field, switch and checkbox edges"),
    ("border-strong", "bg", NON_TEXT, "field edges on the page background"),
    ("star", "surface", NON_TEXT, "favourite star when on"),
    ("focus", "surface", NON_TEXT, "focus ring"),
    ("focus", "bg", NON_TEXT, "focus ring on the page background"),
    ("focus", "surface-2", NON_TEXT, "focus ring on a hovered row"),
]
# The light sidebar is a tint of the brand's or theme's navigation colour (`--nav`): its text has to
# read on the tint of any colour a brand may set.
SIDEBAR_TEXT = ["sb-text", "sb-strong", "sb-muted", "sb-dim"]
NAVIGATION_EXTREMES = ["#000000", "#ffffff", "#ff0000", "#00ff00", "#0000ff", "#808080", "#123456"]
DECLARATION = re.compile(r"(--[\w-]+)\s*:\s*([^;]+);")
COLOR_MIX = re.compile(r"color-mix\(in srgb,\s*(.+?)\s+(var\([^)]+\)|[\d.]+%)\s*,\s*(.+)\)$")


class ContrastTest(unittest.TestCase):
    def test_light_tokens(self):
        self.check_theme("light", themes()["light"])

    def test_dark_tokens(self):
        self.check_theme("dark", themes()["dark"])

    def test_the_light_sidebar_reads_on_a_tint_of_any_navigation_colour(self):
        light = themes()["light"]
        for navigation in NAVIGATION_EXTREMES:
            values = {**light, "--nav": navigation}
            for foreground in SIDEBAR_TEXT:
                with self.subTest(navigation=navigation, foreground=foreground):
                    ratio = contrast(resolve(values, foreground), resolve(values, "sb-bg"))
                    self.assertGreaterEqual(ratio, TEXT, f"{foreground}: {ratio:.2f}:1 on a tint of {navigation}")

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
    """The token as `#rrggbb`, following `var()` and mixing `color-mix(in srgb, a p%, b)`."""
    return evaluate(values, values[f"--{name}"])


def evaluate(values, value):
    value = value.strip()
    if value.startswith("var("):
        return evaluate(values, values[value[4:].split(")")[0].split(",")[0].strip()])
    mix = COLOR_MIX.match(value)
    if mix:
        first, amount, second = mix.groups()
        share = float(evaluate(values, amount).rstrip("%")) / 100
        a, b = rgb(evaluate(values, first)), rgb(evaluate(values, second))
        return "#" + "".join(f"{round(x * share + y * (1 - share)):02x}" for x, y in zip(a, b))
    return value


def rgb(hex_colour):
    return [int(hex_colour[i:i + 2], 16) for i in (1, 3, 5)]


def contrast(first, second):
    lighter, darker = sorted((luminance(first), luminance(second)), reverse=True)
    return (lighter + 0.05) / (darker + 0.05)


def luminance(hex_colour):
    channels = [int(hex_colour[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    linear = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]


if __name__ == "__main__":
    unittest.main()
