# Importing from Slack

Runbook for moving a Slack workspace to TeamCraft with its history: public and private channels,
direct messages, group DMs, threads, files, reactions and edits. Issue #209.

The import is a client, as for Synology Chat: the plaintext stays on the host that runs it, and every
message is encrypted like the apps do (e2ee.md §4.10, admin.md §3.8). Slack's export is first
**converted** into the format the import reads; from then on everything is the same as in
[import-synology.md](import-synology.md): the mappings, the dry run, the import, waiting for people
and checking. This page covers only what differs.

## 1. What comes over, and what doesn't

| Slack                                         | TeamCraft                                                                                     |
|-----------------------------------------------|-----------------------------------------------------------------------------------------------|
| Public channel (`channels.json`)              | Channel, public by default in the template                                                    |
| Private channel (`groups.json`)               | Channel, private                                                                              |
| Group DM (`mpims.json`)                       | Private channel named after its members; in the template like any channel                     |
| DM (`dms.json`)                               | The DM of the two people, only when both are mapped                                           |
| Messages, threads, `/me`, replies to channel  | Imported with the original author and time                                                    |
| `*bold*`, `~strike~`, `<url\|label>`          | Markdown (`**bold**`, `~~strike~~`, links)                                                    |
| Mentions `<@U…>`, `<#C…>`, `@here`            | Mentions of mapped people; channels as `#name`; `@here`, `@channel` as text                   |
| Reactions                                     | Part of the imported message; skin tones are dropped, custom emoji stay as `:name:`           |
| Edits                                         | The last version, marked edited                                                               |
| Files                                         | Only when downloaded beforehand (section 3); else the message names the missing file          |
| Several files on one message                  | The first on the message, each further one a message of its own a millisecond later           |
| Bots and apps                                 | Their name, marked as a bot; an app's attachment text stands in for an empty message           |
| Joins, leaves, topic and name changes, pins   | Not imported (counted in `convert-report.md`)                                                 |
| Deleted messages, external and limited files  | Not imported (counted)                                                                        |
| Archived channel                              | Archived after the import                                                                     |
| Canvases, lists, huddles, custom emoji images | Not imported                                                                                  |

## 2. Export from Slack

As a workspace owner or admin: **Settings & administration → Workspace settings → Import/Export
data → Export**. Which conversations are in it depends on the plan: on every plan the public
channels; private channels and DMs only with an approved full export (Business+ and Enterprise
Grid) or an export tool that reads them with a user token. Unpack the ZIP on the host that runs the
import:

```sh
unzip slack-export.zip -d /data/slack-export
```

The folder holds `users.json`, `channels.json` (and, if exported, `groups.json`, `mpims.json`,
`dms.json`) and a folder per conversation with one JSON file per day. The conversion reads exactly
this layout; it doesn't read the ZIP itself, so a damaged or oversized archive fails at `unzip`, not
in the tool.

## 3. Files

Slack's export has **no files**, only links to them that need a token. `teamcraft-import` never
downloads anything: it looks for each file at

```
/data/slack-export/__uploads/<file ID>/<file name>
```

which is the layout export tools that download files write (for example `slackdump`'s Slack export
format). Download them that way before converting if you want them; a file that isn't there is named
on its message, and the report lists it. External files (Google Drive, Dropbox links) and files hidden
by a free plan's limit are not in any export.

## 4. Convert

```sh
teamcraft-import convert --from slack --source /data/slack-export --out /data/slack-converted
```

It writes into `--out` (new, empty, or an earlier conversion's output, which is replaced):

| File                                                                  | What                                                                     |
|-----------------------------------------------------------------------|--------------------------------------------------------------------------|
| `channels.ndjson`, `users.ndjson`, `bots.ndjson`, `posts.ndjson`, ... | The export in the format `plan`, `run`, `serve` and `report` read        |
| `channels.csv`                                                        | The channel mapping, filled in but for `space` (section 5)               |
| `users.csv`                                                           | The user mapping with Slack's user ID, name and e-mail; `teamcraft_user` |
| `convert-report.md`                                                   | Counts: channels, people, messages, files, missing files, what was left  |

Converting the same export again gives the same IDs, so a conversion can be repeated (a newer
export, a fixed file) and a resumed import still knows what it did. The converted export holds
everything in plain text, like the Slack export: same care (`chmod 700`, delete after checking).

A file of the export that isn't valid JSON stops the conversion and names the file. A conversation
whose folder name would lead out of the export (`..`, a slash) is kept without messages and counted;
so are file names like that. Day files larger than 512 MiB are refused.

## 5. Mappings

Fill in the two CSV files in `--out` as in [import-synology.md §6](import-synology.md#6-the-mappings):

- `channels.csv`: set `space` for every channel to import (Slack has one workspace, so it is empty);
  `visibility` comes from Slack (public or private). Group DMs are rows too: give them a space, or
  `import=no`.
- `users.csv`: put the identity provider's user name in `teamcraft_user`; `email` helps to find it.
  The column `source_id` (instead of Synology's `synology_id`) is the converted ID: leave it as it is.
  People Slack marks as deleted, and people who wrote but are not in `users.json`, are listed with
  `deleted=yes`; leave them empty to show their messages as "Deleted user".

## 6. Dry run, import, waiting, checking

As [import-synology.md §7–§10](import-synology.md#7-dry-run), with the converted folder as the
export and the two templates as the mappings:

```sh
teamcraft-import --state /data/import-state plan \
  --export /data/slack-converted \
  --channels /data/slack-converted/channels.csv --users /data/slack-converted/users.csv
```

`--uploads` is not needed: the converted export names each file's place itself.

## Microsoft Teams

Not supported yet (issue #209): Teams has no export of a team's history that an admin can simply
download; its history comes from Microsoft Graph with an application permission, or from a Purview
eDiscovery export. That needs a design of its own.
