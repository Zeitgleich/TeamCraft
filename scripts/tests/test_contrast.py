"""Contrast of the colour tokens of the UI stylesheet and of the Android colour scheme, light and dark
(issues #361, #362, #2287).

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
    # #2287: the rest of the text pairs, so a new token value can't slip below AA unseen.
    ("text", "bg", TEXT, "body text on the page background"),
    ("text", "surface", TEXT, "body text"),
    ("text", "surface-3", TEXT, "body text on a pressed row"),
    ("text-muted", "bg", TEXT, "secondary text on the page background"),
    ("text-muted", "surface-3", TEXT, "secondary text on a pressed row"),
    ("accent", "surface", TEXT, "links"),
    ("danger", "surface", TEXT, "error text"),
    ("text", "accent-soft", TEXT, "text on the accent tint"),
    ("mention-text", "mention-bg", TEXT, "mention"),
    ("warn-text", "warn-soft", TEXT, "warning notice"),
    ("text", "code-bg", TEXT, "code"),
    ("on-fill", "badge-bg", TEXT, "unread badge"),
    ("on-fill", "dock-red", TEXT, "unread badge on a space tile (rail)"),
    ("on-fill", "call-danger", TEXT, "leave button of a call"),
    ("on-fill", "call-danger-hover", TEXT, "leave button of a call, hovered"),
    ("on-fill", "sb-current", TEXT, "current row of the sidebar"),
    ("chrome-text", "chrome", TEXT, "rail and top bar"),
    ("chrome-strong", "chrome", TEXT, "rail and top bar, strong"),
    ("call-text", "call-bg", TEXT, "call screen"),
    ("call-text", "call-surface", TEXT, "call tile"),
    ("call-muted", "call-bg", TEXT, "call screen, secondary"),
    ("call-muted", "call-surface", TEXT, "call tile, secondary"),
    ("call-notice-text", "call-notice-bg", TEXT, "call notice"),
    ("lightbox-text", "lightbox-bg", TEXT, "lightbox"),
    ("lvl-info", "surface", TEXT, "log level: info"),
    ("lvl-warn", "surface", TEXT, "log level: warning"),
    ("lvl-error", "surface", TEXT, "log level: error"),
    ("sb-current", "sb-bg", NON_TEXT, "current row of the sidebar against the sidebar"),
]
# The light sidebar is a tint of the brand's or theme's navigation colour (`--nav`): its text has to
# read on the tint of any colour a brand may set.
SIDEBAR_TEXT = ["sb-text", "sb-strong", "sb-muted", "sb-dim"]
NAVIGATION_EXTREMES = ["#000000", "#ffffff", "#ff0000", "#00ff00", "#0000ff", "#808080", "#123456"]
ANDROID_THEME = pathlib.Path(__file__).resolve().parents[2] / "apps/android/app/src/main/java/de/zeitgleich/teamcraft/ui/theme/Theme.kt"
# Material roles of the Android colour scheme (#2287): (foreground, background, minimum).
ANDROID_PAIRS = [
    ("onPrimary", "primary", TEXT),
    ("onPrimaryContainer", "primaryContainer", TEXT),
    ("onSecondaryContainer", "secondaryContainer", TEXT),
    ("onSecondary", "secondary", TEXT),
    ("onTertiary", "tertiary", TEXT),
    ("onTertiaryContainer", "tertiaryContainer", TEXT),
    ("onBackground", "background", TEXT),
    ("onSurface", "surface", TEXT),
    ("onSurface", "surfaceVariant", TEXT),
    ("onSurfaceVariant", "surface", TEXT),
    ("onSurfaceVariant", "background", TEXT),
    ("onSurfaceVariant", "surfaceContainerHighest", TEXT),
    ("onErrorContainer", "errorContainer", TEXT),
    ("error", "surface", TEXT),
    ("primary", "surface", TEXT),
    ("inverseOnSurface", "inverseSurface", TEXT),
    ("outline", "surface", NON_TEXT),
    ("outline", "background", NON_TEXT),
]
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

    def test_android_light_scheme(self):
        self.check_android("Light", "lightColorScheme")

    def test_android_dark_scheme(self):
        self.check_android("Dark", "darkColorScheme")

    def test_both_dark_blocks_agree(self):
        css = STYLESHEET.read_text(encoding="utf-8")
        self.assertEqual(tokens(block(css, "@media (prefers-color-scheme: dark)")), tokens(block(css, ':root[data-theme="dark"]')))

    def check_android(self, name, factory):
        source = ANDROID_THEME.read_text(encoding="utf-8")
        start = source.index(f"private val {name} = {factory}(")
        scheme = source[start:source.index("\n)\n", start)]
        colours = {key: "#" + value.lower() for key, value in re.findall(r"(\w+) = Color\(0xFF(\w{6})\)", scheme)}
        for foreground, background, minimum in ANDROID_PAIRS:
            with self.subTest(scheme=name, pair=f"{foreground} on {background}"):
                ratio = contrast(colours[foreground], colours[background])
                self.assertGreaterEqual(ratio, minimum, f"Android {name}: {ratio:.2f}:1")

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
