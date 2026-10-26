#!/bin/sh
# %post of the rpm (tauri.conf.json, bundle.linux.rpm.postInstallScript; docs/updates.md §5).
#
# dnf keeps repository metadata for 48 hours by default, so a new TeamCraft version showed only after
# `dnf upgrade --refresh`. The .repo files the package site serves now say `metadata_expire=1h`; this
# brings repository files written before that up to date. It only touches a file that defines nothing
# but the TeamCraft repository and sets no `metadata_expire` of its own, so a value the user chose
# stays. It never adds a repository, and it never fails the installation.
#
# Tauri's rpm bundler can't mark files as %config(noreplace), so the package doesn't ship the .repo
# file itself: a plain file would overwrite the user's changes on every update.

for file in /etc/yum.repos.d/*.repo; do
  [ -f "$file" ] || continue
  grep -Eq '^\[teamcraft(-prerelease)?\][[:space:]]*$' "$file" || continue
  [ "$(grep -Ec '^[[:space:]]*\[' "$file")" = 1 ] || continue
  grep -Eq '^[[:space:]]*metadata_expire[[:space:]]*=' "$file" && continue
  # A file without a final newline would glue the option to its last line.
  [ -z "$(tail -c 1 "$file")" ] || echo >> "$file"
  echo 'metadata_expire=1h' >> "$file" || true
done
exit 0
