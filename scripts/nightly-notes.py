#!/usr/bin/env python3
"""Writes the tag message (highlights) of a nightly release (docs/release.md, "Nightly releases").

  scripts/nightly-notes.py --since v1.0.8 [--until HEAD] > highlights.md

The pull requests squash-merged since the tag (subject ends in "(#N)") are looked up with `gh`.
Every pull request becomes a bullet with its title, except those labelled `skip-release-notes` and
Dependabot's; `security` and `breaking` ones come first and `breaking` ones are repeated under
"### Breaking" (which operators must act on). At most MAX_BULLETS are listed, the rest is counted.
A "(#N)" at the end of a line of a commit message counts, so a squashed integration branch lists
its pull requests. Commits without one are not listed. The grouped full list is GitHub's
generated "What's changed" in the release notes.
"""
import argparse
import json
import re
import subprocess
import sys

MAX_BULLETS = 15
PR_NUMBER = re.compile(r"\(#(\d+)\)\s*$", re.M)
TRAILING_NUMBER = re.compile(r"\s*\(#\d+\)\s*$")  # the title may already end in its number
MAINTENANCE = "- Fixes and improvements; see *What's changed* below."


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--since", required=True, help="previous tag")
    parser.add_argument("--until", default="HEAD")
    args = parser.parse_args()

    # Subjects and bodies: a squash commit that collects several pull requests (an integration
    # branch) lists them in its body, one "(#N)" per line.
    messages = run(["git", "log", "--format=%B", f"{args.since}..{args.until}"])
    numbers = PR_NUMBER.findall(messages)

    entries: list[tuple[int, str]] = []  # (sort rank, bullet)
    breaking: list[str] = []
    for number in dict.fromkeys(numbers):
        pr = pull_request(number)
        if pr is None:
            continue
        labels = {label["name"] for label in pr["labels"]}
        if "skip-release-notes" in labels or "dependabot" in pr["author"]["login"]:
            continue
        title = TRAILING_NUMBER.sub("", pr["title"])
        line = f"- {title} (#{number})"
        rank = 0 if labels & {"security", "breaking"} else 1
        entries.append((rank, line))
        if "breaking" in labels:
            breaking.append(line)

    # sorted() is stable: newest first within a rank, as `git log` lists them.
    bullets = [line for _, line in sorted(entries, key=lambda entry: entry[0])]
    rest = len(bullets) - MAX_BULLETS
    bullets = bullets[:MAX_BULLETS]
    if rest > 0:
        bullets.append(f"- and {rest} more changes; see *What's changed* below.")

    print("\n".join(bullets) if bullets else MAINTENANCE)
    if breaking:
        print("\n### Breaking\n")
        print("\n".join(breaking))
    return 0


def pull_request(number: str) -> dict | None:
    """The title and labels of a pull request, or None when `gh` can't find it (a plain commit)."""
    result = subprocess.run(
        ["gh", "pr", "view", number, "--json", "title,labels,author"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(f"warning: pull request #{number} not readable: {result.stderr.strip()}", file=sys.stderr)
        return None
    return json.loads(result.stdout)


def run(command: list[str]) -> str:
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout


if __name__ == "__main__":
    sys.exit(main())
