# Search

Search is end-to-end encrypted: the server cannot search your messages. Your device builds a search
index of what it can read, and shares encrypted index segments of closed months with your other
devices (design: [docs/e2ee.md](../../e2ee.md) §10).

## Use it

Press `Ctrl/Cmd+Shift+F` or choose Search in the sidebar. `Ctrl+Enter` in the quick switcher
searches for what you typed, and `/search text` does the same from the composer.

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

Words are matched in English and German word forms ("meetings" finds "meeting"), and small typos
are tolerated on longer words. Filters appear as removable chips; filters your device cannot
evaluate are flagged. Results show highlighted snippets. You can **save a search** and see your
**recent mentions**. Saved searches sync across your devices.

## Limits

- You only find messages your devices can decrypt. After joining a channel with "from join" history
  you cannot search what was written earlier.
- Retention removes old messages and their index entries.
- The browser build keeps its index in memory for the session; it is rebuilt after a reload.
