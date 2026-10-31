# Importing from Synology Chat

Runbook for moving a team from Synology Chat to TeamCraft with its history: channels, direct
messages, "My space", files, threads, stickers, reactions, read state, favourites and mute. How it
works and what the server and the clients trust: [e2ee.md](e2ee.md) §4.10 and [admin.md](admin.md)
§3.8. Issue #402.

The import is a client: `teamcraft-import` signs in as an import account, encrypts every message and
file like the apps do and uploads them. While it imports, the server lets it through its rate limits
and upload quotas, so it runs as fast as the server and the link allow. Plan for:

| Step                          | Typical time (hundreds of channels, about a million posts, 60 GB of files)   |
|-------------------------------|------------------------------------------------------------------------------|
| Export on the NAS             | minutes for the database, longer for the files                               |
| Transfer                      | depends on the link; resumable                                               |
| Dry run                       | about a minute                                                               |
| Import                        | hours; resumable at any point                                                |
| Waiting for people to sign in | days or weeks: `teamcraft-import serve` keeps running until everybody joined |

## 1. What comes over, and what doesn't

| Synology Chat                             | TeamCraft                                                                                    |
|-------------------------------------------|----------------------------------------------------------------------------------------------|
| Channel (type 0, 1)                       | Channel in the space the mapping names, or merged into an existing channel                   |
| Direct message (type 2)                   | The DM of the two people (merged into an existing one), only when both are mapped            |
| My space (type 3)                         | The owner's notes                                                                            |
| Chatbot conversation (type 5), Office (4) | Not imported                                                                                 |
| Posts, threads, files, stickers, mentions | Imported with the original author and time; a line marks the end of the import               |
| Reactions                                 | Part of the imported message (gemoji short codes as emoji)                                   |
| Edits                                     | The last version, marked edited                                                              |
| Read position (`last_view_at`)            | Read position; imported messages are never unread and notify nobody                          |
| Favourite, mute                           | Favourite, mute                                                                              |
| Archived channel                          | Archived after the import                                                                    |
| Webhooks                                  | Listed in the report, to recreate as bridge sources (docs/bridge.md); tokens don't come over |
| Bots, webhooks as authors                 | Their name, marked as a bot; their posts in DMs and notes are left out                       |
| People the mapping doesn't name           | "Deleted user" on their channel posts; DMs with them are not imported                        |
| Profile pictures, Office documents        | Not imported: people set their own picture                                                   |

## 2. Where it runs

Wherever the export is and the server can be reached; the plaintext stays on that host.

- **A host you trust** (your workstation, a VM of your own): the server never sees the plaintext. It
  has to stay up while `serve` waits for people to sign in (section 9), which can take weeks.
- **The TeamCraft server's host** (upload the export there, section 5): the simplest for a long
  `serve`, and the upload is the same size. But the server is the part TeamCraft doesn't trust
  (ARCHITECTURE.md §2): while the export and the import's state lie there, whoever controls that
  host can read the old history and act as the import account's device. Acceptable when you trust
  the host for that time anyway; delete the export once `run` finished, and the state directory once
  `serve` is done. A separate VM next to the server, as for the bridge and the notetaker, keeps the
  two apart.

On a host without a browser, sign in through an SSH tunnel: `login` prints the local port it waits
on; `ssh -L <port>:127.0.0.1:<port> <host>` and open the printed address in your own browser.

## 3. Prerequisites

- A TeamCraft server of this version or later, with the identity provider sync configured
  (`[oidc.sync]`, docs/operations.md): it pre-provisions the mapped accounts, so people who never
  signed in still get their channels and DMs.
- **Every app on this version or later** (desktop, web, Android, iOS) before the import starts. An
  older app shows imported messages under the import account's name, and it turns down the import
  account's group of a DM its user wrote in before the other person had the app: that user then
  can't read the DM's imported history.
- An **import account** at the identity provider (for example `import`), with the TeamCraft `admin`
  client role. Don't set it up in an app: the tool's device is its first and only device.
- **Sign in with the tool** (it makes the import account's keys and device):

  ```sh
  teamcraft-import --state /data/import-state login --server https://chat.example.com
  ```

  It prints an address to open in the browser (the identity provider's page); over SSH, forward the
  port it names first. The state directory now holds the device's encrypted store and keys.
- **Workspace roster**, as an owner in the app: confirm the import account as an admin (admin.md
  §2.3), so members' apps accept what it signs.
- **Administration → Import**, as an owner: pick the import account and the date until which it may
  join conversations (default: 90 days). Without this signed policy the server refuses the import and
  the apps show imported messages as the import account's.
- The host that runs the import (section 2): `teamcraft-import` (`cargo build --release -p
  teamcraft-import`, crate `crates/teamcraft-import`), the export on a local disk, and a little disk
  space for the tool's state (IDs and keys, no content).

## 4. Export on the NAS

Copy `tools/synology-export/chat-export.sh` to the NAS and run it as root (SSH). It reads the Chat
package's database read-only and copies the files:

```sh
sh chat-export.sh /volume1/chat-export
```

The output directory holds NDJSON files (`channels`, `users`, `bots`, `posts`, `reactions`, `reads`,
`favorites`, `notifications`, `webhooks_*`), `avatars/`, `stickers/`, a dump of the whole database
(`synochat.pgdump`) and `raw/uploads/{channel}/{post}` with the attachments; `DONE` is written last.
Leave Synology Chat read-only (or offline) from now on, or export again before the final run.

**An older export** (made with an earlier script, without `bots.ndjson` or with former members in
`channels.ndjson`): restore its dump on any Postgres and export the metadata again from it; the files
of the old export stay valid:

```sh
createdb synochat_restored
pg_restore --no-owner --no-acl -d synochat_restored synochat.pgdump
PSQL="psql -d synochat_restored -At" sh chat-export.sh --metadata-only ./export-metadata
```

Then pass `--export ./export-metadata --uploads <old export>/raw/uploads` below.

## 5. Transfer

Copy the export to the host that runs the import (section 2) with a resumable tool, for example:

```sh
rsync -a --partial --info=progress2 nas:/volume1/chat-export/ /data/chat-export/
```

Start the full import only once the transfer finished (`DONE` exists and the file count matches
`file-count.txt`): a message is posted once, so a file missing then stays missing (the message names
it, and the report lists it).

The export holds everybody's messages and files in plain text: keep it readable by one account only
(`chmod 700`), and run the tool as that account. If the files belong to `root` and the tool runs as
another user, give them to that user first (`chown -R import: /data/chat-export /data/mapping`); the
tool only reads them. Its state directory (`--state`) must be writable by the same user. Delete the
export once the import is checked (section 10).

## 6. The mappings

Two CSV files with a header row; `teamcraft-import plan` writes templates of both from the export
(`--write-templates`), and the operator fills them in.

**Channels** (`channels.csv`), one row per channel of type 0, 1 and 5:

| Column                                                            | Meaning                                                                                                                 |
|-------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------|
| `channel_id`                                                      | The Synology channel ID                                                                                                 |
| `name`                                                            | The new channel's name                                                                                                  |
| `visibility`                                                      | `public` or `private` (anything else: private)                                                                          |
| `webhook`, `members`, `posts`, `last_post`, `created`, `archived` | Information only                                                                                                        |
| `space`                                                           | The TeamCraft space a new channel goes into (made if missing)                                                           |
| `teamcraft_channel`                                               | Empty: a new channel. `Space/Channel`: merge into that existing channel. `notes`: into the notes of its single member   |
| `teamcraft_name`                                                  | Optional: the new channel's name instead of `name` (also to tell two channels of one space apart); ignored when merging |
| `import_history`                                                  | `no`: the channel with its members but without posts (log and webhook channels); else the full history                  |
| `import`                                                          | `f`, `false`, `n` or `no` (any case): skip the channel; else import it                                                  |

Channels without posts are skipped. DMs (type 2) and "My space" (type 3) need no row.

**People** (`users.csv`): `synology_id` and `teamcraft_user` (the user name at the identity provider)
are read; the other columns are information. Rows without `teamcraft_user` are unmapped.

## 7. Dry run

```sh
teamcraft-import --state /data/import-state plan \
  --export /data/chat-export --channels channels.csv --users users.csv --language de
```

`--language` (`en`, `de`) is how a mention of somebody without an account reads in the imported
text ("@Deleted user"). `plan` reads everything, checks the mappings against the server (spaces, channels to merge into, user names
at the identity provider) and changes nothing. It prints and writes (`report-plan.md` in the state
directory) what each channel, DM and notes conversation would get, every skipped item with its reason,
and every problem (an unknown user name, a merge target that doesn't exist or shares history only from
the join on, two new channels with the same name in one space, a new channel named like an existing
one). Fix the mappings until it reports no problems. The same inputs go to `run`, `serve` and
`report`.

## 8. Import

```sh
teamcraft-import --state /data/import-state run \
  --export /data/chat-export --channels channels.csv --users users.csv
```

It works conversation by conversation, oldest posts first, and shows its progress. It can be stopped
(Ctrl-C) and started again at any time, also after a crash or a lost network: what is done is kept in
the state directory, and a run skips it and fills gaps (a conversation cut short, a channel added to
the mapping). A second run over a finished import changes nothing. For existing channels it joins as
a workspace admin, which needs a device of a member of that channel online (the run waits and says
so); for existing DMs, a device of one of the two people.

The state directory holds the tool's device keys (encrypted, the key in `store.key`): keep it, back it
up, and never run two imports with the same state at once.

## 9. Waiting for everybody

People who haven't set up the app yet are members already; they get their channels, history and DMs
when they set it up. Their addition waits on the import device, so leave it running:

```sh
teamcraft-import --state /data/import-state serve
```

It reconnects on its own and leaves each conversation once everybody is in. `teamcraft-import
--state /data/import-state report` shows the counts per conversation, skipped items, who is still
missing where, and the webhooks to recreate. When nobody is missing (or the remaining people won't
come), stop it, choose **End import** in Administration → Import and deactivate the import account.

## 10. Checking

- As a mapped person: the channels in their spaces with the history, the DMs, the notes, files and
  pictures, reactions, and nothing unread from the import.
- A merged channel or DM: the imported history before its first message.
- `report` after a second `run`: nothing new imported.

## 11. Problems

| Message                       | What to do                                                                                                 |
|-------------------------------|------------------------------------------------------------------------------------------------------------|
| `import_not_allowed`          | The signed import policy doesn't name this account, or its date passed (step 2)                            |
| A user name is not found      | The name in `users.csv` differs from the identity provider's, or the account lacks a role                  |
| Waiting for a member's device | Somebody of that channel or DM has to open the app once                                                    |
| `import_window_full`          | Far more posts after the merged conversation's first message than fit before it; reported per conversation |
| A file is missing             | The message names it; finish the transfer before the run next time                                         |
