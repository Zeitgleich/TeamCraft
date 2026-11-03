#!/usr/bin/env python3
"""Splits the files every feature appended to into one file per area (issue #614).

The UI stylesheet, the UI's Fluent files and the Android app's main strings file collected nearly
every feature's additions at their end, so parallel branches conflicted there. This script moves
their content into area files; after it, new work goes into the area files directly.

- crates/teamcraft-ui/style/main.css becomes style/main/NNN-<area>.css, cut at the comments listed
  in CSS_AREAS. The build bundles the files in name order (crates/teamcraft-ui/scripts/bundle-css.sh),
  so the bundle is byte for byte the old main.css: the number in front of a name is its place in the
  cascade.
- crates/teamcraft-ui/i18n/<language>.ftl becomes i18n/<language>/<area>.ftl. A message goes to the
  area of the longest prefix of its id in FTL_AREAS; the comments that headed it come along.
- apps/android/app/src/main/res/values*/strings.xml moves into the strings_<area>.xml files beside
  it, by the name prefixes in ANDROID_AREAS (an existing area file gets the strings appended).

It doesn't guess: a section comment, message id or string name without an area stops it with a
list, so the tables get extended instead. Before writing it checks that nothing was lost or changed
(the CSS pieces join to the old file; every message and string is in exactly one new file, as it
was). Files that are already split are left alone, so running it again changes nothing. To redo the
split on a newer base, check out the base's versions of the old files and run it again.

    scripts/split-hotspots.py           # split what is still in one file
    scripts/split-hotspots.py --check   # fail if one of the old files is back (CI)
"""
import argparse
import pathlib
import re
import sys
import xml.etree.ElementTree as ElementTree

ROOT = pathlib.Path(__file__).resolve().parent.parent
UI = ROOT / "crates/teamcraft-ui"
CSS_SOURCE = UI / "style/main.css"
CSS_DIR = UI / "style/main"
FTL_DIR = UI / "i18n"
ANDROID_RES = ROOT / "apps/android/app/src/main/res"

# Where each part of main.css starts (the first line of its top-level comment; None: the file's
# start) and the file it goes to. In file order; the names sort in the same order. Every
# `/* ---- … ---- */` section comment must be listed.
CSS_AREAS = [
    (None, "000-tokens.css"),
    ("/* ---- Base ---- */", "010-base.css"),
    ("/* ---- Shell layout ---- */", "020-shell.css"),
    ("/* The global search (#724)", "030-top-bar-search.css"),
    ("/* ---- Generic controls ---- */", "040-controls.css"),
    ("/* Underlined tabs of a view", "050-view-tabs.css"),
    ("/* Preview of a channel the user hasn't joined.", "060-channel-preview.css"),
    ("/* ---- Avatars and presence ---- */", "070-avatars.css"),
    ("/* ---- Space rail ---- */", "080-space-rail.css"),
    ("/* ---- Sidebar ---- */", "090-sidebar.css"),
    ("/* ---- Custom title bar", "100-title-bar.css"),
    ("/* ---- Channel view ---- */", "110-channel.css"),
    ("/* ---- Message list ---- */", "120-messages.css"),
    ("/* ---- The bottom sheet of the phone layout", "130-sheet.css"),
    ("/* The log reads like a terminal (#702)", "140-log.css"),
    ("/* A slim bar between the feed and the composer", "150-jump-latest.css"),
    ("/* ---- Composer ---- */", "160-composer.css"),
    ("/* ---- Side panels ---- */", "170-panels.css"),
    ("/* ---- Popover (status menu) ---- */", "180-popover.css"),
    ("/* ---- Modals ---- */", "190-modals.css"),
    ("/* ---- Quick switcher", "200-switcher.css"),
    ("/* ---- Narrow windows ---- */", "210-narrow.css"),
    ("/* ---- Calls ---- */", "220-calls.css"),
    ("/* Incoming call */", "230-incoming-call.css"),
    ("/* ---- Administration ---- */", "240-admin.css"),
    ("/* Organization call backgrounds", "250-org-backgrounds.css"),
    ("/* ---- Device approval, linking, key change ---- */", "260-devices.css"),
    ("/* The admin console's users left in encryption groups", "270-admin-encryption-groups.css"),
    ("/* ---- Sign-in ----", "280-sign-in.css"),
    ("/* Another tab runs the app (#744", "290-tab-elsewhere.css"),
    ("/* Forms: label above a full-width field, hint below.", "300-sign-in-forms.css"),
    ("/* ---- Search ---- */", "310-search.css"),
    ("/* ---- Settings tabs ---- */", "320-settings-tabs.css"),
    ("/* Message layouts (issue #576)", "330-message-layouts.css"),
    ("/* ---- Meetings ---- */", "340-meetings.css"),
    ("/* Message actions */", "350-message-actions.css"),
    ("/* Files: pictures inline", "360-attachments.css"),
    ("/* One card for linked messages and link previews", "370-link-previews.css"),
    ("/* Lightbox: one image or video", "380-lightbox.css"),
    ("/* Workspace dialogs */", "390-workspace-dialogs.css"),
    ("/* Browse spaces and invitations */", "400-browse-spaces.css"),
    ("/* Toolbar of integration channels", "410-log-toolbar.css"),
    ("/* Profiles and directory */", "420-profiles.css"),
    ("/* Crop dialog (issue #323)", "430-crop.css"),
    ("/* ---- Profile card (profile_views.rs) ----", "440-profile-card.css"),
    ("/* Syntax highlighting (code blocks) */", "450-code.css"),
    ("/* Composer extras */", "460-composer-extras.css"),
    ("/* A failed action (issue #322)", "470-feedback.css"),
    ("/* Meeting responses and host controls */", "480-meeting-responses.css"),
    ("/* Own background pictures", "490-own-backgrounds.css"),
    ("/* Administration console: permissions", "500-admin-console.css"),
    ("/* Desktop updates (docs/updates.md", "510-updates.css"),
    ("/* Activity (#752)", "520-activity.css"),
    ("/* Grouped sidebar: one header per space", "530-grouped-sidebar.css"),
    ("/* ---- Section header with an action", "540-section-header.css"),
    ("/* ---- Emoji picker ---- */", "550-emoji-picker.css"),
    ("/* The connection state (#718)", "560-connection.css"),
    ("/* Long messages start collapsed", "570-long-messages.css"),
    ("/* Placeholders for content that is still loading", "580-skeletons.css"),
    ("/* The day of the top message while scrolled up", "590-message-meta.css"),
    ("/* Card over a person's name while the pointer rests on it.", "600-hover-card.css"),
    ("/* A message opened from search, a link or the activity list", "610-highlight.css"),
    ("/* Forms in the workspace dialogs", "620-workspace-forms.css"),
    ("/* ---- Getting started (empty workspace or space) ---- */", "630-getting-started.css"),
    ("/* An input with a button beside it", "640-passphrase-field.css"),
    ("/* Runtime settings of the admin console", "650-admin-runtime-settings.css"),
    ("/* Keyboard shortcut: one <kbd> per key cap", "660-kbd.css"),
    ("/* ---- Direct messages page", "670-direct-messages.css"),
    ("/* ---- Motion (issue #273) ----", "680-motion.css"),
    ("/* ---- Compact layout (phones, issue #307) ----", "690-compact.css"),
    ("/* ---- Increase contrast and forced colours", "700-contrast.css"),
    ("/* ======", "710-settings-dialog.css"),
    ("/* Parts of the app that load when first used", "720-lazy-areas.css"),
    ("/* ---- People picker (#540)", "730-people-picker.css"),
    ("/* The top of a sidebar on a phone (#703)", "740-sidebar-phone.css"),
    ("/* ---- Home pages: Calls, Meetings, People, Saved", "750-home-pages.css"),
    ("/* The share link dialog", "760-share-links.css"),
    ("/* ---- Rail menus: create and focus mode", "770-rail-menus.css"),
    ("/* ---- Files view (issue #301) ---- */", "780-files-view.css"),
    ("/* ---- Setup checklist on Chats (issue #301) ---- */", "790-setup-checklist.css"),
    ("/* Admins' requests to join a private channel", "800-admin-join-requests.css"),
]
CSS_SECTION = "/* ---- "

# The Fluent area files: name, what they hold (their heading comment), and the message id prefixes
# that go there. An id goes to its longest matching prefix (`call` matches `call` and `call-…`).
FTL_AREAS = [
    ("app.ftl", "General words, the app's frame (menus, window, tray, about, updates) and app-wide states.", [
        "skip-to-content", "cancel", "save", "close", "back", "create", "confirm", "undo", "menubar", "window",
        "tray", "about", "bundle", "release", "update", "webview", "panic", "area", "route", "storage", "data",
        "connection", "offline", "catching", "error", "language", "weekday", "tab", "badge",
    ]),
    ("loader.ftl", "The loading screen (index.html, before the app runs; scripts/finish-loader.sh copies these into init.js).", [
        "loader",
    ]),
    ("callback.ftl", "The desktop app's sign-in callback page (apps/desktop/src/auth/callback_page.rs reads this file).", [
        "callback",
    ]),
    ("navigation.ftl", "The rail, sidebar and bottom navigation, the account menu, home pages, activity and getting started.", [
        "rail", "sidebar", "bottom", "home", "chats", "account", "menu", "brand", "focus", "favorite", "dm",
        "activity", "saved", "notes", "start", "setup", "page",
    ]),
    ("sign-in.ftl", "Choosing a server and signing in.", [
        "login", "server",
    ]),
    ("devices.ftl", "Setting up this device: keys, recovery, linking and approving devices, key changes.", [
        "enroll", "recovery", "passphrase", "approve", "approval", "link", "device", "devices", "waiting",
        "never", "group", "left", "welcome", "contact", "key-changed", "key-verify", "key-accept", "key-new",
        "key-old", "key-mark-verified", "error-link",
    ]),
    ("channels.ftl", "Spaces and channels: settings, members, joining, browsing, integrations and their log.", [
        "channel", "space", "spaces", "conversation", "members", "join", "invite", "browse", "bookmarks", "ws",
        "system", "history", "removal", "leave", "retention", "log", "role", "external", "error-admin-join",
    ]),
    ("messages.ftl", "Messages, threads and the composer.", [
        "message", "messages", "thread", "composer", "action", "format", "slash", "emoji", "reaction", "quote",
        "forward", "forwarded", "linked", "permalink", "pins", "priority", "ack", "jump", "new", "mention",
        "reminder", "date", "history-title", "history-current", "history-replaced", "link-menu",
        "link-open-new", "typing",
    ]),
    ("files.ftl", "Files, pictures and videos, previews, the lightbox and share links.", [
        "file", "files", "attachment", "drop", "paste", "image", "media", "preview", "lightbox", "video", "crop",
        "links", "shared", "share", "text-preview",
    ]),
    ("calls.ftl", "Calls: the call screen and pre-join, incoming calls, huddles, screen sharing and effects.", [
        "call", "calls", "huddle", "prejoin", "incoming", "effect", "effects", "tile", "control", "share-start",
        "share-stop", "share-options", "share-sound", "share-motion", "share-choose", "share-banner",
        "share-screen", "error-call",
    ]),
    ("meetings.ftl", "Meetings: scheduling, responses, host controls and guests.", [
        "meetings", "meeting", "schedule", "guest", "host",
    ]),
    ("people.ftl", "People: profiles, presence and status, the directory and user groups.", [
        "profile", "presence", "status", "directory", "groups", "hover", "last", "user",
    ]),
    ("search.ftl", "Search and the quick switcher.", [
        "search", "switcher",
    ]),
    ("settings.ftl", "Settings: notifications, privacy, security, display, keyboard shortcuts.", [
        "settings", "notif", "notify", "privacy", "security", "keyword", "shortcuts", "shortcut", "key",
    ]),
    ("admin.ftl", "The admin console: people, permissions, policies, the roster, the instance, audit, announcements.", [
        "admin", "permission", "permissions", "policy", "instance", "audit", "announcement", "roster",
    ]),
]
FTL_MESSAGE = re.compile(r"^([a-zA-Z][a-zA-Z0-9_-]*)\s*=")
# Lines that continue a message: indented, or the syntax a select expression may start a line with.
FTL_CONTINUATION = (" ", "\t", "}", "[", "*", ".")

# The Android area files and the string names (or name prefixes before `_`) that move into them.
ANDROID_AREAS = [
    ("strings_app.xml", [
        "app_name", "back", "cancel", "retry", "loading", "ok", "more", "you", "user_deleted_name", "error",
        "settings_title",
    ]),
    ("strings_signin.xml", ["sign_in", "sign_in_hint", "sign_in_failed", "sign_out"]),
    ("strings_firstrun.xml", [
        "enroll", "passphrase", "passphrase_repeat", "passphrase_too_short", "passphrases_differ",
        "recover_button", "link_instead", "device_name", "org_recovery",
    ]),
    ("strings_link.xml", ["link", "approve"]),
    ("strings_security.xml", ["fingerprint", "key_changes", "backup", "change_passphrase"]),
    ("strings_push.xml", ["push", "notification"]),
    ("strings_calls.xml", ["call"]),
    ("strings_channels.xml", [
        "channels", "channel", "browse_title", "people", "external_people", "leave_channel", "files", "search",
        "search_hint", "search_empty", "event",
    ]),
    ("strings_dm.xml", ["chats_title", "favorites", "direct_messages", "no_conversations", "favorite"]),
    ("strings_messages.xml", [
        "composer_hint", "send", "send_options", "attach", "thread", "threads", "reply_in_thread", "pending",
        "edited", "read_only_channel", "replies", "action", "edit_history", "message_deleted", "forwarded_from",
        "replying_to", "new_messages", "file", "priority", "option", "unread_count", "saved", "mentions",
        "acknowledged_by", "messages_undecrypted",
    ]),
    ("strings_meetings.xml", ["meetings", "meeting"]),
    ("strings_settings.xml", [
        "presence", "custom_status", "notifications_conversation", "notify", "read_receipts",
        "auto_follow_threads",
    ]),
]
ANDROID_HEADER = '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n'
ANDROID_FOOTER = "</resources>\n"


class SplitError(Exception):
    pass


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if one of the old files exists")
    parser.add_argument("--dry-run", action="store_true", help="list what would change, write nothing")
    args = parser.parse_args(argv)
    if args.check:
        return check()
    try:
        outputs = {}
        if CSS_SOURCE.exists():
            outputs.update(split_css(CSS_SOURCE, CSS_DIR))
        for source in sorted(FTL_DIR.glob("*.ftl")):
            outputs.update(split_ftl(source, FTL_DIR / source.stem))
        for source in sorted(ANDROID_RES.glob("values*/strings.xml")):
            outputs.update(split_android(source))
    except SplitError as error:
        print(f"split-hotspots: {error}", file=sys.stderr)
        return 1
    if not outputs:
        print("split-hotspots: nothing to split")
        return 0
    if args.dry_run:
        for path, content in sorted(outputs.items()):
            change = "remove" if content is None else f"write {len(content.splitlines())} lines"
            print(f"{path.relative_to(ROOT)}: {change}")
        return 0
    for path, content in sorted(outputs.items()):
        if content is None:
            path.unlink()
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content.encode("utf-8"))
    removed = sum(content is None for content in outputs.values())
    print(f"split-hotspots: wrote {len(outputs) - removed} files, removed {removed}")
    return 0


def check():
    """The old files must not come back (a merge that keeps a branch's main.css, say)."""
    old = [CSS_SOURCE, *sorted(FTL_DIR.glob("*.ftl")), *sorted(ANDROID_RES.glob("values*/strings.xml"))]
    old = [path for path in old if path.exists()]
    for path in old:
        print(f"{path.relative_to(ROOT)}: split into area files (issue #614); move its content there")
    return 1 if old else 0


# ---- CSS


def split_css(source, out_dir):
    """The area files of `source` ({path: text}, None to delete), cut at CSS_AREAS."""
    if out_dir.exists() and any(out_dir.glob("*.css")):
        raise SplitError(f"{out_dir.relative_to(ROOT)} already has stylesheets and {source.name} exists too")
    text = source.read_bytes().decode("utf-8")
    lines = text.splitlines(keepends=True)
    starts = [0]
    for anchor, _ in CSS_AREAS[1:]:
        found = [index for index, line in enumerate(lines) if line.startswith(anchor)]
        if len(found) != 1:
            raise SplitError(f"{source.name}: the area comment {anchor!r} is there {len(found)} times, not once")
        starts.append(found[0])
    if starts != sorted(starts):
        raise SplitError(f"{source.name}: the area comments are not in the order of CSS_AREAS")
    unlisted = [
        f"  line {index + 1}: {line.strip()}"
        for index, line in enumerate(lines)
        if line.startswith(CSS_SECTION) and index not in starts
    ]
    if unlisted:
        raise SplitError(f"{source.name}: sections without an area in CSS_AREAS:\n" + "\n".join(unlisted))
    names = [name for _, name in CSS_AREAS]
    if names != sorted(names) or len(set(names)) != len(names):
        raise SplitError("CSS_AREAS: the file names must be unique and sort in the listed order")
    pieces = ["".join(lines[start:end]) for start, end in zip(starts, [*starts[1:], len(lines)])]
    if "".join(pieces) != text:
        raise SplitError(f"{source.name}: the pieces don't add up to the file")
    for name, piece in zip(names, pieces):
        if not braces_balance(piece):
            raise SplitError(f"{name}: unbalanced braces; cut {source.name} at another comment")
    outputs = {out_dir / name: piece for name, piece in zip(names, pieces)}
    outputs[source] = None
    return outputs


def braces_balance(css):
    depth = 0
    for char in re.sub(r"/\*.*?\*/", "", css, flags=re.S):
        depth += char == "{"
        depth -= char == "}"
        if depth < 0:
            return False
    return depth == 0


# ---- Fluent


def split_ftl(source, out_dir):
    """The area files of one language's Fluent file ({path: text}, None to delete)."""
    if out_dir.exists() and any(out_dir.glob("*.ftl")):
        raise SplitError(f"{out_dir.relative_to(ROOT)} already has Fluent files and {source.name} exists too")
    text = source.read_bytes().decode("utf-8")
    header, messages = parse_ftl(text, source.name)
    by_area = {name: [] for name, _, _ in FTL_AREAS}
    unmapped = []
    for context, message_id, message_lines in messages:
        area = area_of(message_id, FTL_AREAS, "-")
        if area is None:
            unmapped.append(message_id)
        else:
            by_area[area].append((context, message_lines))
    if unmapped:
        raise SplitError(f"{source.name}: message ids without an area in FTL_AREAS: {', '.join(unmapped)}")
    outputs = {}
    descriptions = {name: description for name, description, _ in FTL_AREAS}
    for name, entries in by_area.items():
        if entries:
            outputs[out_dir / name] = render_ftl(header, descriptions[name], entries)
    verify_ftl(source.name, messages, outputs.values())
    outputs[source] = None
    return outputs


def parse_ftl(text, name):
    """The leading comment (the file's title) and the messages, each with the comments heading it.

    A message is (context, id, lines): `context` is the last `##` group comment and the `#` comment
    after it, if any (they head sections, not single messages, in these files).
    """
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    index = 0
    header = []
    while index < len(lines) and lines[index].startswith("#") and not lines[index].startswith("##"):
        header.append(lines[index])
        index += 1
    group, note = (), ()
    messages = []
    while index < len(lines):
        line = lines[index]
        if not line.strip():
            index += 1
        elif line.startswith("#"):
            block = []
            while index < len(lines) and lines[index].startswith("#"):
                block.append(lines[index])
                index += 1
            if block[0].startswith("##"):
                group, note = tuple(block), ()
            else:
                note = tuple(block)
        elif FTL_MESSAGE.match(line):
            message_id = FTL_MESSAGE.match(line).group(1)
            message_lines = [line]
            index += 1
            while index < len(lines):
                ahead = index
                while ahead < len(lines) and not lines[ahead].strip():
                    ahead += 1
                if ahead < len(lines) and lines[ahead].startswith(FTL_CONTINUATION):
                    message_lines.extend(lines[index:ahead + 1])
                    index = ahead + 1
                else:
                    break
            messages.append(((group, note), message_id, message_lines))
        else:
            raise SplitError(f"{name}:{index + 1}: not a message, comment or blank line: {line!r}")
    return header, messages


def render_ftl(header, description, entries):
    out = [*header, f"# {description}"]
    last_group, last_note = None, None
    for (group, note), message_lines in entries:
        if group != last_group:
            out.append("")
            out.extend(group)
            out.extend(note)
        elif note != last_note:
            out.append("")
            out.extend(note)
        last_group, last_note = group, note
        out.extend(message_lines)
    return "\n".join(out) + "\n"


def verify_ftl(name, messages, texts):
    """Every message of the old file is in exactly one new file, unchanged."""
    before = {message_id: message_lines for _, message_id, message_lines in messages}
    if len(before) != len(messages):
        raise SplitError(f"{name}: a message id is defined twice")
    after = {}
    for text in texts:
        for _, message_id, message_lines in parse_ftl(text, name)[1]:
            if message_id in after:
                raise SplitError(f"{name}: {message_id} would be in two files")
            after[message_id] = message_lines
    if before != after:
        raise SplitError(f"{name}: the split changed messages: {sorted(set(before) ^ set(after))}")


def area_of(name, areas, separator):
    """The area whose longest prefix matches `name` (the whole name, or up to a `separator`)."""
    best = None
    for area, *rest in areas:
        for prefix in rest[-1]:
            if (name == prefix or name.startswith(prefix + separator)) and (best is None or len(prefix) > best[1]):
                best = (area, len(prefix))
    return best[0] if best else None


# ---- Android


def split_android(source):
    """Moves the strings of one `values*/strings.xml` into the area files beside it."""
    text = source.read_bytes().decode("utf-8")
    if not text.startswith(ANDROID_HEADER) or not text.endswith(ANDROID_FOOTER):
        raise SplitError(f"{source.relative_to(ROOT)}: expected the plain header and footer")
    lines = text[len(ANDROID_HEADER):-len(ANDROID_FOOTER)].splitlines(keepends=True)
    by_area = {name: [] for name, _ in ANDROID_AREAS}
    unmapped = []
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        # Its two comments only head groups of the old file ("Calls", "Chat features"); the area
        # files name their areas themselves.
        if not stripped or (stripped.startswith("<!--") and stripped.endswith("-->")):
            index += 1
            continue
        match = re.match(r"\s*<([a-z-]+) name=\"([^\"]+)\"", line)
        if not match:
            raise SplitError(f"{source.relative_to(ROOT)}: unexpected line {line!r}")
        tag, name = match.groups()
        end = index
        while f"</{tag}>" not in lines[end] and not lines[end].rstrip().endswith("/>"):
            end += 1
        element = lines[index:end + 1]
        index = end + 1
        area = area_of(name, ANDROID_AREAS, "_")
        if area is None:
            unmapped.append(name)
        else:
            by_area[area].extend(element)
    if unmapped:
        raise SplitError(f"{source.relative_to(ROOT)}: strings without an area in ANDROID_AREAS: {', '.join(unmapped)}")
    outputs = {}
    for name, added in by_area.items():
        if not added:
            continue
        target = source.parent / name
        existing = target.read_bytes().decode("utf-8") if target.exists() else ANDROID_HEADER + ANDROID_FOOTER
        if not existing.endswith(ANDROID_FOOTER):
            raise SplitError(f"{target.relative_to(ROOT)}: expected it to end with {ANDROID_FOOTER!r}")
        outputs[target] = existing[: -len(ANDROID_FOOTER)] + "".join(added) + ANDROID_FOOTER
    verify_android(source, outputs)
    outputs[source] = None
    return outputs


def verify_android(source, outputs):
    """The directory has the same resources afterwards, each defined once."""
    def resources(texts):
        found = {}
        for text in texts:
            for element in ElementTree.fromstring(text.encode("utf-8")):
                key = (element.tag, element.get("name"))
                if key in found:
                    raise SplitError(f"{source.relative_to(ROOT)}: {key} would be defined twice")
                # The whitespace after the element depends on its neighbours, not on the string.
                element.tail = None
                found[key] = ElementTree.tostring(element)
        return found

    others = [path for path in sorted(source.parent.glob("strings*.xml")) if path != source]
    before = resources([path.read_bytes().decode("utf-8") for path in [source, *others]])
    after = resources([outputs.get(path) or path.read_bytes().decode("utf-8") for path in others] + [
        text for path, text in outputs.items() if path not in others
    ])
    if before != after:
        raise SplitError(f"{source.relative_to(ROOT)}: the split changed strings: {sorted(set(before) ^ set(after))}")


if __name__ == "__main__":
    sys.exit(main())
