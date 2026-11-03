#!/usr/bin/env python3
"""Fails if a UI call of a desktop command passes argument names the command doesn't take.

Issue #397 (PR #396): Tauri commands take camelCase argument names unless told otherwise, while the
UI sends the Rust names (snake_case). Three commands were broken, and device linking failed,
without any test noticing. Two rules keep that from coming back:

1. Every `#[tauri::command]` in `apps/desktop/src` sets `rename_all = "snake_case"`, so the
   argument names on the wire are exactly the Rust parameter names.
2. At every call site in the UI (`call("name", &args)`, `invoke("name", args)`) the JSON keys
   (`json!({ "key": .. })`, `Reflect::set(&args, &"key".into(), ..)`) must be parameters of that
   command, and every parameter that isn't an `Option` must be passed.

    scripts/check-tauri-args.py [--desktop apps/desktop/src] [--ui crates/teamcraft-ui/src]

The call sites are read textually: the keys come from the `json!` block or the `Reflect::set`
calls in the same function above the call. A call whose arguments can't be found there (built by
a helper) is only checked for its command name.
"""
import argparse
import pathlib
import re
import sys

# Parameters Tauri injects itself; they are not part of the arguments sent by the UI. A `Channel`
# is not one of them: the UI passes it (`__CHANNEL__:<id>`) under the parameter's name.
INJECTED = re.compile(
    r"^(?:tauri::)?(?:State|AppHandle|Window|WebviewWindow|Webview|Request|ipc::Request)\b"
)
COMMAND_ATTRIBUTE = re.compile(
    r"#\[tauri::command(?:\(([^)]*)\))?\]\s*(?:(?:#\[[^\]]*\]|//[^\n]*\n)\s*)*"
    r"(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?fn\s+(\w+)\s*\("
)
CALL_SITE = re.compile(r"\b(?:call|invoke|server_call)(?:::<.*?>)?\(\s*\"(\w+)\"\s*,")
REFLECT_KEY = re.compile(r"Reflect::set\(\s*&\w+\s*,\s*&\"(\w+)\"")
FUNCTION_START = re.compile(r"^[ \t]*(?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?fn\s", re.M)
# Commands the browser build answers itself in `web_core.rs`; the desktop app has no such command.
WEB_ARM = re.compile(r'^\s*"(core_\w+)"(?:\s*\|\s*"core_\w+")*\s*=>', re.M)
SNAKE_CASE = re.compile(r'rename_all\s*=\s*"snake_case"')


class Command:
    def __init__(self, name, params, is_snake_case, location):
        self.name = name
        self.params = params  # name -> is_optional
        self.is_snake_case = is_snake_case
        self.location = location


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--desktop", default="apps/desktop/src", type=pathlib.Path)
    parser.add_argument("--ui", default="crates/teamcraft-ui/src", type=pathlib.Path)
    args = parser.parse_args(argv)

    commands = {}
    for path in sorted(args.desktop.rglob("*.rs")):
        for command in parse_commands(path.read_text(), str(path)):
            commands[command.name] = command
    web_core = args.ui / "web_core.rs"
    web_only = set(WEB_ARM.findall(web_core.read_text())) if web_core.exists() else set()
    problems = [
        f"{c.location}: `{c.name}` needs #[tauri::command(rename_all = \"snake_case\")]"
        for c in commands.values()
        if not c.is_snake_case
    ]
    for path in sorted(args.ui.rglob("*.rs")):
        problems += check_calls(path.read_text(), str(path), commands, web_only)

    if not commands:
        problems.append(f"no #[tauri::command] found under {args.desktop}")
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print(f"\n{len(problems)} problem(s) with desktop command arguments (issue #397).", file=sys.stderr)
        return 1
    return 0


def parse_commands(source, location):
    commands = []
    for match in COMMAND_ATTRIBUTE.finditer(source):
        line = source.count("\n", 0, match.start()) + 1
        # Comments between parameters are not part of the next one's name.
        signature = re.sub(r"^[ \t]*//.*$", "", balanced(source, match.end() - 1), flags=re.M)
        params = split_top_level(signature)
        parsed = {}
        for param in params:
            name, _, type_ = param.partition(":")
            type_ = type_.strip()
            if not type_ or INJECTED.match(type_):
                continue
            parsed[name.strip().removeprefix("mut ")] = type_.startswith("Option<")
        commands.append(
            Command(match.group(2), parsed, bool(SNAKE_CASE.search(match.group(1) or "")), f"{location}:{line}")
        )
    return commands


def check_calls(source, location, commands, web_only):
    problems = []
    for match in CALL_SITE.finditer(source):
        name = match.group(1)
        line = source.count("\n", 0, match.start()) + 1
        where = f"{location}:{line}"
        command = commands.get(name)
        if command is None and name in web_only:
            continue
        if command is None:
            problems.append(f"{where}: `{name}` is not a #[tauri::command] of the desktop app")
            continue
        keys = argument_keys(source, match)
        if keys is None:
            continue
        for key in sorted(keys - command.params.keys()):
            problems.append(f"{where}: `{name}` has no argument `{key}` (takes {sorted(command.params)})")
        for param, is_optional in command.params.items():
            if not is_optional and param not in keys:
                problems.append(f"{where}: `{name}` is called without its argument `{param}`")
    return problems


def argument_keys(source, call):
    """The keys passed at `call`, or None when they can't be read from the surrounding function."""
    passed = balanced(source, source.rfind("(", call.start(), call.end() - 1 - len(call.group(1)) - 2)).split(",", 1)[1].strip()
    if passed.startswith("JsValue::UNDEFINED"):
        return set()
    starts = [m.start() for m in FUNCTION_START.finditer(source, 0, call.start())]
    start = starts[-1] if starts else 0
    scope = source[start : call.start()] + passed
    keys = set(REFLECT_KEY.findall(scope))
    json_at = max(scope.rfind("json!("), scope.rfind("json!{"))
    if json_at >= 0:
        brace = scope.find("{", json_at)
        keys |= top_level_keys(balanced(scope, brace, "{", "}"))
    if keys or "json!({})" in scope or "js_sys::Object::new()" in scope:
        return keys
    return None


def top_level_keys(body):
    keys = set()
    for entry in split_top_level(body):
        match = re.match(r'\s*"(\w+)"\s*:', entry)
        if match:
            keys.add(match.group(1))
    return keys


def balanced(text, open_at, opener="(", closer=")"):
    """The text between the bracket at `open_at` and its partner, skipping string literals."""
    depth = 0
    i = open_at
    while i < len(text):
        char = text[i]
        if char == '"':
            i = skip_string(text, i)
            continue
        if char == opener:
            depth += 1
        elif char == closer:
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : i]
        i += 1
    return text[open_at + 1 :]


def skip_string(text, i):
    i += 1
    while i < len(text) and text[i] != '"':
        i += 2 if text[i] == "\\" else 1
    return i + 1


def split_top_level(text):
    """Splits at commas outside brackets, generics and strings."""
    parts, depth, current, i = [], 0, [], 0
    while i < len(text):
        char = text[i]
        if char == '"':
            end = skip_string(text, i)
            current.append(text[i:end])
            i = end
            continue
        if char in "([{<":
            depth += 1
        elif char in ")]}>" and not (char == ">" and text[i - 1 : i] == "-"):
            depth -= 1
        if char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
        i += 1
    if "".join(current).strip():
        parts.append("".join(current))
    return [part for part in parts if part.strip()]


if __name__ == "__main__":
    sys.exit(main())
