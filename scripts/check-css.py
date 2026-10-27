#!/usr/bin/env python3
"""Fails on stylesheet mistakes that silently change what users see (issue #363).

The UI stylesheet is long, and two rules with the same specificity override each other without a
warning. This check fails on:

- a `var(--x)` with no definition of `--x` anywhere in the checked files (variables set at runtime
  are allow-listed in RUNTIME_VARIABLES),
- two `@keyframes` with the same name (the later one wins everywhere),
- the same selector at top level twice with different values for one property (the later one wins).
  A rule that refines a group it was part of (`.a, .b { flex: 1 }` then `.b { flex: none }`) is a
  deliberate split and passes; other deliberate splits are allow-listed in ALLOWED_OVERRIDES,
- a font size off the type scale (issue #370): `font-size` and the size in `font` are a
  `var(--text-*)` token, so the text size setting scales them. Glyphs sized to a fixed box
  (initials in avatars and tiles, emoji cells) are listed in FIXED_FONT_SIZES and never below
  11 px,
- more raw hex colours outside custom property definitions than MAX_RAW_HEX_COLOURS (a ratchet:
  the count may only go down; new colours become tokens),
- `will-change` outside WILL_CHANGE: it keeps the element on a compositing layer of its own for as
  long as it exists (GPU memory of its size, a blend every frame). Only short-lived elements that
  really move (the lightbox, the crop dialog) get it, each with the reason,
- inline styles in the UI's Rust code (issue #376): a `style=` attribute on an HTML element in
  `view!` markup or `set_attribute("style", …)`. The web CSP is `style-src 'self'`, which blocks
  them at runtime with only a console error. Dynamic styles use `style:prop=` / `style:--var=` or
  `HtmlElement::style().set_property`. Mark a real exception (or the line above it) with
  `// inline-style-ok: <reason>`.

    scripts/check-css.py [paths ...]   # default: crates/teamcraft-ui/style/*.css and crates/teamcraft-ui/src

Paths ending in `.css` are stylesheets; other files and directories are searched for `.rs` files.

The parser is small and only understands what the stylesheet uses: rules, at-rules with nested
blocks, comments and strings. It is not a validator.
"""
import argparse
import pathlib
import re
import sys

DEFAULT_DIR = pathlib.Path("crates/teamcraft-ui/style")
DEFAULT_RUST_DIR = pathlib.Path("crates/teamcraft-ui/src")
# Set on elements at runtime: from Rust (`style:--x=…`, `style.setProperty`) or loader.js.
RUNTIME_VARIABLES = re.compile(r"^--(vv-.*|indicator-.*|hue|media-.*|cols|banners-h|window-controls-.*|tok-.*|tc-loader-.*|link-menu-.*|date-jump-.*|rail-menu-.*|setup-.*)$")
# Keyframes that are known duplicates, each with the issue that removes it.
KNOWN_DUPLICATE_KEYFRAMES = {
}
# (selector, property) pairs that are set twice at top level on purpose, with the reason.
ALLOWED_OVERRIDES = {}
# Selectors whose glyph follows a box of fixed size, not the text size, with the reason.
FIXED_FONT_SIZES = {
    ".avatar-sm": "initials in a 20 px avatar",
    ".space-tile": "initials in a 20 px tile",
    ".switcher-space .space-tile": "initials in an 18 px tile",
    ".space-image-preview .space-tile": "initials in a 64 px tile",
    ".space-row .space-tile": "initials in a 36 px tile",
    ".tile-initials": "initials sized to the call tile",
    ".avatar-xl": "initials sized to the pre-join tile's avatar",
    ".sheet-reaction": "an emoji in a 44 px button",
    ".emoji-grid li": "an emoji in a 34 px cell",
    ".approval-code": "a verification code sized to its card",
}
# Selectors that may set `will-change`, with the reason: only elements that move under the user's
# finger and go away with their overlay (docs/performance.md, "Desktop on Linux: memory").
WILL_CHANGE = {
    ".lightbox-image": "zoomed and panned per pointer move while the lightbox is open",
    ".lightbox-track": "swiped between pictures while the lightbox is open",
    ".crop-image": "dragged and zoomed while the crop dialog is open",
    ":is(.popover, .emoji-picker, .modal, .toast, .sheet, .motion-ghost)": "short-lived overlays keep their layer while up, so the end of their motion doesn't snap (#514)",
    ".tab-indicator.is-live": "only while the indicator slides; the class goes when it stops (#514)",
}
# Font sizes that are not on the scale on purpose, with the reason.
ALLOWED_FONT_SIZES = {
    "inherit": "takes the parent's size",
    "0.9em": "inline code follows the heading or text it sits in",
    "max(16px, 1em)": "iOS zooms into fields with text below 16 px",
    "calc(100% * var(--text-scale))": "the root size the scale is built on",
}
TINY_FONT_PX = 11
MAX_RAW_HEX_COLOURS = 65
# A length in the `font` shorthand (`.85rem`, `12px`, `0.9em`); a bare number is the weight.
LENGTH = re.compile(r"\d*\.?\d+(?:px|rem|em|%|pt|vw|vh)")
SIZE_FUNCTIONS = ("var(--text-", "calc(", "max(", "min(", "clamp(")
HEX_COLOUR = re.compile(r"#[0-9a-fA-F]{3,8}\b")
VAR_USE = re.compile(r"var\(\s*(--[\w-]+)")
FONT_SIZE_PX = re.compile(r"(\d+(?:\.\d+)?)px\b")
BLOCK_AT_RULES = ("@media", "@supports", "@layer", "@container", "@document")
# `style=` as an attribute: not `style:prop=`, `x.style =`, `let style =` or `style ==`. Which tag
# it belongs to is found by walking back from it (see enclosing_tag).
STYLE_ATTRIBUTE = re.compile(r"(?<![\w:.-])style\s*=(?!=)")
TAG_NAME = re.compile(r"<([A-Za-z][\w-]*)")
CLOSERS = {")": "(", "]": "[", "}": "{"}
SET_STYLE_ATTRIBUTE = re.compile(r"set_attribute\(\s*\"style\"")
INLINE_STYLE_MARKER = "inline-style-ok:"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("paths", nargs="*", type=pathlib.Path)
    args = parser.parse_args(argv)
    paths = args.paths or [*sorted(DEFAULT_DIR.glob("*.css")), DEFAULT_RUST_DIR]
    stylesheets = [path for path in paths if path.suffix == ".css"]
    rust_files = [file for path in paths if path.suffix != ".css" for file in rust_files_in(path)]
    findings = check_sources({str(path): path.read_text(encoding="utf-8") for path in stylesheets})
    for path in rust_files:
        findings += inline_styles(str(path), path.read_text(encoding="utf-8"))
    for finding in findings:
        print(finding)
    if findings:
        print(f"{len(findings)} style problem(s); see scripts/check-css.py for the rules.", file=sys.stderr)
        return 1
    return 0


def check_sources(sources):
    """Returns the findings for a {path: css} map, as `path:line: message` strings."""
    parsed = {path: parse(css) for path, css in sources.items()}
    findings = []
    findings += undefined_variables(parsed)
    findings += duplicate_keyframes(parsed)
    findings += conflicting_overrides(parsed)
    findings += off_scale_font_sizes(parsed)
    findings += raw_hex_colours(parsed)
    findings += layer_hints(parsed)
    return findings


def rust_files_in(path):
    return sorted(path.rglob("*.rs")) if path.is_dir() else [path]


def inline_styles(path, source):
    """Finds `style=` attributes and `set_attribute("style", …)` in one Rust file."""
    lines = source.splitlines()
    # Strings blanked for the bracket walk; `set_attribute("style"` needs its string.
    code = blank_line_comments(blank_string_contents(source))
    with_strings = blank_line_comments(source)
    findings = []
    for match in [*STYLE_ATTRIBUTE.finditer(code), *SET_STYLE_ATTRIBUTE.finditer(with_strings)]:
        if match.re is STYLE_ATTRIBUTE:
            tag = enclosing_tag(code, match.start())
            # No tag: a plain assignment. Upper case: a component's `style` property.
            if tag is None or not tag[0].islower():
                continue
        line = line_of(code, match.end())
        if INLINE_STYLE_MARKER in lines[line - 1] or (line > 1 and INLINE_STYLE_MARKER in lines[line - 2]):
            continue
        findings.append(f"{path}:{line}: inline style, blocked by the CSP: {lines[line - 1].strip()}")
    return sorted(findings, key=lambda finding: int(finding.split(":")[1]))


def enclosing_tag(code, offset):
    """The name of the tag whose attribute list contains `offset`, or None.

    Walks back over balanced brackets, so `;` and `>` inside earlier attribute values
    (`on:click=move |_| { a; b }`, `class=move || if n > 0 { … }`) don't end the search. Leaving
    the enclosing block or meeting a `;` at the same depth means `offset` is not in a tag.
    """
    depth = []
    for index in range(offset - 1, -1, -1):
        char = code[index]
        if char in CLOSERS:
            depth.append(CLOSERS[char])
        elif char in "([{":
            if not depth:
                return None
            depth.pop()
        elif depth:
            continue
        elif char == ";":
            return None
        elif char == "<":
            match = TAG_NAME.match(code, index)
            if match:
                return match.group(1)
    return None


def blank_string_contents(code):
    """Blanks the inside of string literals, keeping the quotes, so brackets in text don't count."""
    return re.sub(r'"(?:\\.|[^"\\])*"', lambda match: '"' + re.sub(r"[^\n]", " ", match.group(0)[1:-1]) + '"', code)


def blank_line_comments(source):
    """Blanks `//` comments (the allow marker lives there; prose may mention `style=`)."""
    return re.sub(r"//[^\n]*", lambda match: " " * len(match.group(0)), source)


def undefined_variables(parsed):
    defined = {decl.name for sheet in parsed.values() for decl in sheet.declarations if decl.name.startswith("--")}
    findings = []
    for path, sheet in parsed.items():
        for decl in sheet.declarations:
            for name in VAR_USE.findall(decl.value):
                if name not in defined and not RUNTIME_VARIABLES.match(name):
                    findings.append(f"{path}:{decl.line}: var({name}) is never defined")
    return findings


def duplicate_keyframes(parsed):
    first_seen = {}
    findings = []
    for path, sheet in parsed.items():
        for name, line in sheet.keyframes:
            if name in first_seen and name not in KNOWN_DUPLICATE_KEYFRAMES:
                findings.append(f"{path}:{line}: @keyframes {name} is already defined at {first_seen[name]}")
            first_seen.setdefault(name, f"{path}:{line}")
    return findings


def conflicting_overrides(parsed):
    first_seen = {}
    findings = []
    for path, sheet in parsed.items():
        for decl in sheet.declarations:
            if not decl.is_top_level or decl.name.startswith("--"):
                continue
            for selector in decl.selectors:
                key = (selector, decl.name)
                if key in ALLOWED_OVERRIDES:
                    continue
                if key in first_seen and first_seen[key][0] != decl.value:
                    value, where, group_size = first_seen[key]
                    if len(decl.selectors) < group_size:
                        continue
                    findings.append(
                        f"{path}:{decl.line}: `{selector}` sets {decl.name}: {decl.value} "
                        f"but {where} already sets it to {value}; merge the rules"
                    )
                first_seen.setdefault(key, (decl.value, f"{path}:{decl.line}", len(decl.selectors)))
    return findings


def off_scale_font_sizes(parsed):
    findings = []
    for path, sheet in parsed.items():
        for decl in sheet.declarations:
            if decl.name not in ("font-size", "font"):
                continue
            size = font_size_of(decl)
            if size is None or size.startswith("var(--text-") or size in ALLOWED_FONT_SIZES:
                continue
            if any(selector in FIXED_FONT_SIZES for selector in decl.selectors):
                px = FONT_SIZE_PX.match(size)
                if px is None or float(px.group(1)) >= TINY_FONT_PX:
                    continue
                findings.append(f"{path}:{decl.line}: {decl.name}: {decl.value} is below {TINY_FONT_PX}px")
                continue
            findings.append(f"{path}:{decl.line}: {decl.name}: {decl.value} is off the type scale; use var(--text-*)")
    return findings


def layer_hints(parsed):
    findings = []
    for path, sheet in parsed.items():
        for decl in sheet.declarations:
            if decl.name != "will-change" or decl.value == "auto":
                continue
            unlisted = [selector for selector in decl.selectors if selector not in WILL_CHANGE]
            if unlisted:
                findings.append(
                    f"{path}:{decl.line}: will-change on {', '.join(unlisted)} keeps a GPU layer; list it in WILL_CHANGE with the reason, or drop it"
                )
    return findings


def font_size_of(decl):
    """The size in `font-size`, or in the `font` shorthand (after style and weight, before `/`)."""
    if decl.name == "font-size":
        return decl.value
    if decl.value in ("inherit", "initial", "unset"):
        return None
    for part in decl.value.split("/")[0].split():
        if LENGTH.fullmatch(part) or part.startswith(SIZE_FUNCTIONS):
            return part
    return None


def raw_hex_colours(parsed):
    raw = [
        f"{path}:{decl.line}: {decl.name}: {decl.value}"
        for path, sheet in parsed.items()
        for decl in sheet.declarations
        if not decl.name.startswith("--") and HEX_COLOUR.search(decl.value)
    ]
    if len(raw) > MAX_RAW_HEX_COLOURS:
        return [f"{len(raw)} declarations with a raw hex colour (at most {MAX_RAW_HEX_COLOURS}); add a token:"] + raw
    return []


class Declaration:
    def __init__(self, name, value, line, selectors, is_top_level):
        self.name = name
        self.value = value
        self.line = line
        self.selectors = selectors
        self.is_top_level = is_top_level


class Sheet:
    def __init__(self):
        self.declarations = []
        self.keyframes = []


def parse(css):
    """Parses `css` into its declarations (with their selectors) and keyframe names."""
    text = blank_comments(css)
    sheet = Sheet()
    # Each open block: (prelude, start offset of the block's own text)
    stack = []
    segment_start = 0
    index = 0
    while index < len(text):
        char = text[index]
        if char in "\"'":
            index = skip_string(text, index)
            continue
        if char == "{":
            prelude = text[segment_start:index].strip()
            stack.append(prelude)
            if prelude.startswith("@keyframes"):
                sheet.keyframes.append((prelude.split()[1], line_of(text, index)))
            segment_start = index + 1
        elif char == "}":
            add_declarations(sheet, text, segment_start, index, stack)
            if stack:
                stack.pop()
            segment_start = index + 1
        elif char == ";" and not is_rule_block(stack):
            # A statement at-rule (`@import …;`) outside any rule.
            segment_start = index + 1
        index += 1
    return sheet


def add_declarations(sheet, text, start, end, stack):
    """Adds the declarations between `start` and `end`, the tail of the innermost block."""
    if not is_rule_block(stack):
        return
    selectors = [normalise(selector) for selector in split_top_level(stack[-1], ",")]
    # A rule straight under the root, not inside @media, @supports, @keyframes or another rule.
    is_top_level = len(stack) == 1
    offset = start
    for chunk in split_top_level(text[start:end], ";"):
        if ":" in chunk:
            name, value = chunk.split(":", 1)
            if name.strip():
                line = line_of(text, offset + len(chunk) - len(chunk.lstrip()))
                sheet.declarations.append(Declaration(name.strip().lower(), " ".join(value.split()), line, selectors, is_top_level))
        offset += len(chunk) + 1


def is_rule_block(stack):
    return bool(stack) and not stack[-1].startswith(BLOCK_AT_RULES) and not stack[-1].startswith("@keyframes")


def split_top_level(text, separator):
    """Splits on `separator` outside parentheses and strings."""
    parts, depth, quote, current = [], 0, None, []
    for char in text:
        if quote:
            quote = None if char == quote else quote
        elif char in "\"'":
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif char == separator and depth == 0:
            parts.append("".join(current))
            current = []
            continue
        current.append(char)
    parts.append("".join(current))
    return parts


def blank_comments(css):
    """Replaces comments with spaces, keeping newlines so line numbers stay right."""
    return re.sub(r"/\*.*?\*/", lambda match: re.sub(r"[^\n]", " ", match.group(0)), css, flags=re.S)


def skip_string(text, index):
    quote = text[index]
    index += 1
    while index < len(text) and text[index] != quote:
        index += 2 if text[index] == "\\" else 1
    return index + 1


def normalise(selector):
    return " ".join(selector.split())


def line_of(text, offset):
    return text.count("\n", 0, offset) + 1


if __name__ == "__main__":
    sys.exit(main())
