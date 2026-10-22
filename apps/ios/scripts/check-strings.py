#!/usr/bin/env python3
"""Checks the String Catalogs: every key the Swift sources look up exists, has English and German
text with the same placeholders, and no key is unused. Runs anywhere (no Xcode)."""
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LANGUAGES = ("en", "de")
# SwiftUI/Foundation calls whose first string argument is a localization key.
LOOKUP = re.compile(
    r'(?:Text|Button|Label|Section|Picker|TextField|SecureField|NavigationLink|ProgressView|'
    r'navigationTitle|String\(localized:|prompt: Text|Menu)\(?\s*"([a-z][a-z0-9_]*)(?: [^"]*)?"'
)
KEY_LITERAL = re.compile(r'"([a-z][a-z0-9]*(?:_[a-z0-9]+)+)(?: [^"]*)?"')
PLACEHOLDER = re.compile(r"%(?:\d+\$)?[@dlsf]+")


def load(path):
    return json.loads(path.read_text(encoding="utf-8"))["strings"]


def texts(entry, language):
    localization = entry.get("localizations", {}).get(language)
    if localization is None:
        return None
    if "stringUnit" in localization:
        return [localization["stringUnit"]["value"]]
    plural = localization["variations"]["plural"]
    return [plural[form]["stringUnit"]["value"] for form in sorted(plural)]


def main():
    errors = []
    catalogs = {
        "TeamCraft/Resources/Localizable.xcstrings": ["TeamCraft", "Shared"],
        "NotificationService/Localizable.xcstrings": ["NotificationService"],
    }
    for catalog_path, source_dirs in catalogs.items():
        strings = load(ROOT / catalog_path)
        keys = {key.split(" ")[0] for key in strings}
        for key, entry in strings.items():
            for language in LANGUAGES:
                if texts(entry, language) is None:
                    errors.append(f"{catalog_path}: {key} has no {language} text")
            reference = [PLACEHOLDER.findall(t) for t in texts(entry, "en") or []]
            for language in LANGUAGES[1:]:
                other = [PLACEHOLDER.findall(t) for t in texts(entry, language) or []]
                if reference and other and reference[0] != other[0]:
                    errors.append(f"{catalog_path}: {key} placeholders differ in {language}")
        used = set()
        for directory in source_dirs:
            for source in (ROOT / directory).rglob("*.swift"):
                text = source.read_text(encoding="utf-8")
                for match in LOOKUP.finditer(text):
                    key = match.group(1)
                    used.add(key)
                    if key not in keys:
                        errors.append(f"{source.relative_to(ROOT)}: missing key {key}")
                used.update(m.group(1) for m in KEY_LITERAL.finditer(text))
                # A key returned from a helper (`return "favorites"`).
                used.update(literal for literal in re.findall(r'"([a-z]+)"', text) if literal in keys)
        # The app's catalog also serves keys the extension's Shared code looks up (notification actions).
        if catalog_path.startswith("TeamCraft"):
            for key in sorted(keys - used):
                errors.append(f"{catalog_path}: unused key {key}")
    for message in errors:
        print(message, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
