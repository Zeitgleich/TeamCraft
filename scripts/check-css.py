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
- more `font-size` values below 11 px than MAX_TINY_FONT_SIZES (a ratchet: the count may only go
  down),
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
RUNTIME_VARIABLES = re.compile(r"^--(vv-.*|hue|media-.*|cols|banners-h|window-controls-.*|tok-.*|tc-loader-.*)$")
# Keyframes that are known duplicates, each with the issue that removes it.
KNOWN_DUPLICATE_KEYFRAMES = {
}
# (selector, property) pairs that are set twice at top level on purpose, with the reason.
ALLOWED_OVERRIDES = {}
TINY_FONT_PX = 11
MAX_TINY_FONT_SIZES = 6
VAR_USE = re.compile(r"var\(\s*(--[\w-]+)")
FONT_SIZE_PX = re.compile(r"(?:^|\s)(\d+(?:\.\d+)?)px\b")
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
    findings += tiny_font_sizes(parsed)
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


def tiny_font_sizes(parsed):
    tiny = []
    for path, sheet in parsed.items():
        for decl in sheet.declarations:
            if decl.name not in ("font-size", "font"):
                continue
            # In the `font` shorthand the size is the first length; `/16px` after it is the line height.
            value = decl.value.split("/")[0]
            sizes = [float(px) for px in FONT_SIZE_PX.findall(value)]
            if sizes and sizes[0] < TINY_FONT_PX:
                tiny.append(f"{path}:{decl.line}: {decl.name}: {decl.value}")
    if len(tiny) > MAX_TINY_FONT_SIZES:
        return [f"{len(tiny)} font sizes below {TINY_FONT_PX}px (at most {MAX_TINY_FONT_SIZES}):"] + tiny
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
