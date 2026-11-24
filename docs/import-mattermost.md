# Importing from Mattermost

Runbook for moving a Mattermost instance to TeamCraft with its history: teams, public and private
channels, direct and group messages, threads, files, reactions, edits and favourites. Issue #209.

The import is a client, as for Synology Chat: the plaintext stays on the host that runs it, and every
message is encrypted like the apps do (e2ee.md §4.10, admin.md §3.8). Mattermost's bulk export is
first **converted** into the format the import reads; from then on everything is the same as in
[import-synology.md](import-synology.md). This page covers only what differs.

## 1. What comes over, and what doesn't

| Mattermost                                 | TeamCraft                                                                                     |
|--------------------------------------------|-----------------------------------------------------------------------------------------------|
| Team                                       | The suggested space of its channels (`space` in the template)                                 |
| Public channel (`O`)                       | Channel, public                                                                               |
| Private channel (`P`)                      | Channel, private                                                                              |
| Direct message                             | The DM of the two people, only when both are mapped                                           |
| Group message (three or more)              | Private channel named after its members; in the template like any channel                     |
| Posts, replies                             | Imported with the original author and time, replies in their thread                           |
| Markdown                                   | Taken as it is; `@username` of the export's people becomes a mention of the mapped account    |
| Reactions                                  | Part of the imported message (`+1`, `white_check_mark`, ...); custom emoji stay as `:name:`   |
| Edits                                      | The last version, marked edited                                                               |
| Attachments                                | Uploaded encrypted; several on one post: the first on it, each further one a message after it |
| Favourite channels                         | Favourite                                                                                     |
| Webhook and bot posts                      | Their (override) name, marked as a bot; an app's attachment text stands in for an empty post  |
| Channel members                            | From the people's lines (`teams[].channels[]`)                                                |
| Archived channel                           | Archived after the import                                                                     |
| Deleted posts                              | Not imported (counted in `convert-report.md`)                                                 |
| Pins, flags, custom emoji, schemes, roles  | Not imported                                                                                  |

## 2. Export from Mattermost

On the Mattermost server, as a system admin, with `mmctl`:

```sh
mmctl export create --attachments
mmctl export list
mmctl export download <name>.zip
```

Unpack it on the host that runs the import:

```sh
unzip <name>.zip -d /data/mattermost-export
```

It holds one JSONL file (one JSON object per line: `version`, `team`, `channel`, `user`, `post`,
`direct_channel`, `direct_post`, ...) and, with `--attachments`, the files under `data/`. The
conversion reads the JSONL file line by line, so the export's size doesn't matter for memory; posts
are spooled per channel on disk. A line longer than 64 MiB is refused.

## 3. Convert

```sh
teamcraft-import convert --from mattermost \
  --source /data/mattermost-export/import.jsonl --out /data/mattermost-converted
```

`--source` is the JSONL file; attachment paths in it are read relative to its folder. A path that
would lead out of that folder (`..`, an absolute path, a symbolic link outside) is left out and
counted. The output is the same as for Slack ([import-slack.md §4](import-slack.md#4-convert)):
the export in the import's format, `channels.csv`, `users.csv` and `convert-report.md`.

Mattermost names everything by name: a channel is `team/name`, a person their user name. The
converted IDs are made from those names, so the same export converts to the same IDs; renaming a
channel or a person in Mattermost between two exports makes them new ones for the import.

A line that isn't valid JSON stops the conversion and names its line number; lines of types the tool
doesn't read (schemes, roles, emoji) are counted and skipped. Posts in a channel the export doesn't
list are counted and skipped.

## 4. Mappings

Fill in the two CSV files in `--out` as in [import-synology.md §6](import-synology.md#6-the-mappings):

- `channels.csv`: `space` is the team's display name; change it to the TeamCraft space each channel
  goes into. Group messages are rows too: give them a space, or `import=no`.
- `users.csv`: put the identity provider's user name in `teamcraft_user`; `source_user` is the
  Mattermost user name and `email` its address (often the same user name at the identity provider).
  Deactivated people, and people who wrote but have no line of their own (bots, removed accounts),
  are listed with `deleted=yes`.

## 5. Dry run, import, waiting, checking

As [import-synology.md §7–§10](import-synology.md#7-dry-run), with the converted folder as the
export and the two templates as the mappings:

```sh
teamcraft-import --state /data/import-state plan \
  --export /data/mattermost-converted \
  --channels /data/mattermost-converted/channels.csv --users /data/mattermost-converted/users.csv
```
