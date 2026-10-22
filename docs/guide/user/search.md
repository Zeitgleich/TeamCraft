# Search

Search is end-to-end encrypted: the server cannot search your messages. Your device builds a search
index of what it can read, and shares encrypted index segments of closed months with your other
devices (design: [docs/e2ee.md](../../e2ee.md) §10).

## Use it

Press `Ctrl/Cmd+Shift+F` or choose Search in the sidebar. `Ctrl+Enter` in the quick switcher
searches for what you typed.

| Query               | Meaning                        |
|---------------------|--------------------------------|
| `deploy plan`       | Messages containing both words |
| `"deploy plan"`     | The exact phrase               |
| `from:anna`         | Messages by a person           |
| `in:general`        | Messages in a channel          |
| `has:file`          | Messages with an attachment    |
| `has:link`          | Messages with a link           |
| `before:2026-10-31` | Older than a date              |
| `after:2026-10-01`  | Newer than a date              |
| `in-thread`         | Only messages inside threads   |

Filters appear as removable chips. Results show highlighted snippets; you can save a search and see
recent mentions.

## Limits

- You only find messages your devices can decrypt. After joining a channel with "from join" history
  you cannot search what was written earlier.
- Retention removes old messages and their index entries.
- Status: the search engine exists in the client core (desktop and Android), but the **desktop
  search page currently answers from demo data**. Treat search in the desktop app as *coming soon*.
  The browser build has no persistent index.
