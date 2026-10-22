# Messages and threads

## Write a message

Type in the composer and press **Enter** (or **Ctrl/Cmd+Enter**, a setting under Settings >
Display). Messages support a safe Markdown subset: `**bold**`, `_italic_`, `~~strike~~`,
`` `code` ``, fenced code blocks (with syntax highlighting), lists, quotes and links. No HTML is
rendered. A **formatting toolbar** and a **preview** wrap your selection in Markdown (Settings >
Writing turns them on or off); `:)` and `:tada:` become emoji when you send (also a setting).

- **@name** opens the **mention picker**: people, `@channel` / `@here` and **user groups**.
  `@channel` in a large conversation asks you to confirm first.
- **Attach files** with the paperclip or by dropping them in; see [Files](files.md).
- **Emoji** from the emoji button. Reactions: hover a message and choose the reaction button, or
  choose an existing reaction chip to add or remove yours.
- **Priority:** mark a message *Important* or *Urgent*. Recipients of urgent messages can
  **Confirm read**; in DMs the server repeats the push every two minutes (default, up to five times)
  until they do. Urgent messages never break through do-not-disturb or a mute.
- **Drafts** are kept per conversation and sync across your devices (encrypted).

## Message menu

Hover a message and open **More actions**:

| Action              | What it does                                                                               |
|---------------------|--------------------------------------------------------------------------------------------|
| Reply in thread     | Opens the thread in a side panel                                                           |
| Quote reply         | Replies with the original quoted                                                           |
| Edit                | Changes your message; **Earlier versions** shows the history with word-level changes       |
| Delete              | Deletes it for everyone (your message, or any if you moderate the channel)                 |
| Forward             | Sends it to another conversation, with an optional comment                                 |
| Pin to conversation | Shows it in the channel's pinned messages (they survive retention by default)              |
| Save for later      | Adds it to **Saved**, synced across your devices                                           |
| Copy link           | A permalink; people in the conversation see a preview, others see that they have no access |
| Mark as unread      | Moves your read position back to this message                                              |

**Jump to date** opens a conversation's history at a day.

## Slash commands

Type `/` in the composer for the list. Commands run on your device. To send text that starts with a
slash, type `//`.

| Command                                   | Does                                         |
|-------------------------------------------|----------------------------------------------|
| `/away` `/online` `/dnd` `/offline`       | Sets your presence                           |
| `/status [emoji] text` or `/status clear` | Sets or clears your custom status            |
| `/topic` `/purpose` `/rename`             | Changes the channel's topic, purpose or name |
| `/join` `/open` `/leave`                  | Joins, opens or leaves a channel             |
| `/invite @person` `/kick @person`         | Adds or removes a member                     |
| `/msg @person [text]` `/groupmsg ...`     | Opens a direct or group conversation         |
| `/mute`                                   | Mutes the conversation                       |
| `/me` `/shrug` `/code`                    | Action message, shrug, code block            |
| `/search text`                            | Opens search                                 |
| `/remind in 10m` or `at 14:30` + text     | A reminder in this conversation              |
| `/meet`                                   | Starts an instant meeting                    |
| `/settings` `/shortcuts` `/help`          | Opens settings or the lists                  |

## Bookmarks

The bar under the channel header holds links and files for the channel. Anyone who may manage
bookmarks can **Edit bookmarks**: emoji, title and link, reorder, or add a file from a message.
The bar is encrypted like messages, so every member sees the same bar.

## Threads

Reply in a thread to keep a conversation tidy.

1. Open the thread of a message (`Enter` on a focused message, or the reply button). The thread opens
   in a side panel. Your admin may also switch threads off; then a workspace is flat.
2. You **follow** a thread automatically when you reply, when it is your message, or (by default)
   when you are mentioned in it. Follow or unfollow by hand any time.
3. **Threads** (`Alt+T`) is your inbox of followed threads, newest activity first, with unread and
   mention counts. A thread reply can also be **sent to the channel**.

## Direct message extras

In DMs and group DMs the sender sees **Seen by** after you read a message. Read receipts are
reciprocal: if you turn them off in Settings > Privacy, you neither send nor see them.

## Keyboard

| Keys                  | Action                       |
|-----------------------|------------------------------|
| `Ctrl/Cmd+K`          | Quick switcher               |
| `Ctrl/Cmd+Shift+F`    | Search                       |
| `Alt+Up` / `Alt+Down` | Previous / next conversation |
| `Alt+T`               | Threads                      |
| `Ctrl/Cmd+,`          | Settings                     |
| `Ctrl/Cmd+/`          | All shortcuts                |
| `Esc`                 | Close dialog or side panel   |

## Not available yet

Scheduled messages, bot interactions (buttons, menus) and recording in calls are coming soon.
