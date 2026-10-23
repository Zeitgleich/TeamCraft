# UX backlog (desktop and web client)

Friction points found by using the web client (demo data and the dev stack) for typical daily flows:
switching spaces and channels, reading, replying, threads, search, files, calls, settings. Each entry
says what was missing or annoying and its state. Priority is daily value: what people hit many times
a day comes first.

States: **done** (fixed on the branch named in the PR), **open** (not started), **partly** (some of it
exists).

## Reading

| Friction                                                                                          | State  |
|---------------------------------------------------------------------------------------------------|--------|
| Opening a conversation with unread messages gives no hint where the new ones start                | done   |
| The feed always opens at the bottom, even when there are more unread messages than fit            | done   |
| "Jump to latest" doesn't say how many new messages arrived while scrolled up                      | done   |
| After sending, the feed stops a few pixels short of the new message (the last line is cut off)    | done   |
| The scroll position of a conversation is lost when switching away and back                        | done   |
| Long messages and code blocks are not collapsed                                                   | done   |
| The day of the top message while scrolling up (floating date)                                     | done   |

## Navigation and keyboard

| Friction                                                                                          | State  |
|---------------------------------------------------------------------------------------------------|--------|
| Quick switcher (Ctrl/Cmd+K) only matches a continuous substring (`eng` works, `egr` doesn't)      | done   |
| Quick switcher doesn't list recently visited conversations first                                  | done   |
| No shortcut to jump to the next / previous conversation with unread messages (Alt+Shift+↑/↓)      | done   |
| No "mark all as read" for the unread conversations of a space                                     | done   |
| Shortcut overview (Ctrl/Cmd+/) is missing the newer shortcuts                                     | done   |
| Ctrl/Cmd+F to search within the open conversation                                                 | done   |
| Collapsed sidebar sections and spaces are expanded again after a reload                           | done   |

## Composer and messages

| Friction                                                                                          | State  |
|---------------------------------------------------------------------------------------------------|--------|
| The react button can only add 👍; there is no emoji picker for reactions                           | done   |
| The emoji button in the composer always inserts 🙂 instead of opening a picker                     | done   |
| No recently used emoji (quick reactions)                                                          | done   |
| ↑ in the empty composer doesn't edit the last own message                                         | done   |
| The edit box of a message doesn't take the focus when editing starts                              | done   |
| Pasting a URL over selected text replaces the text instead of making a link                       | done   |
| Drag and drop of files only works over the composer, not over the whole conversation              | done   |
| Pasted images are attached by name only, without a preview                                        | done   |
| No undo after deleting a message (it asked "Delete for everyone?" instead); now an Undo toast     | done   |
| No undo after leaving a channel                                                                   | done   |
| Messages in the outbox look sent; now "Sending…", then "Not sent yet, retrying"                   | done   |

## General and desktop

| Friction                                                                                          | State  |
|---------------------------------------------------------------------------------------------------|--------|
| No banner when the device is offline; sending silently waits                                      | done   |
| No zoom shortcuts (Ctrl/Cmd + `+` / `-` / `0`) and no text size setting                           | done   |
| The desktop window forgets its size and position                                                  | done   |
| Tray icon with unread badge, start minimised to tray                                              | open   |
| Clicking an author name does nothing; it now opens the profile card (local time, status, message) | done   |
| Hover card on names (picture, status, local time, message, profile)                               | done   |
| Favourites can be reordered (drag, or Ctrl+Shift+Up/Down); categories still can't                 | partly |
| Loading skeletons while a conversation's history loads                                            | done   |

## Bugs found on the way

| Bug                                                                                               | State  |
|---------------------------------------------------------------------------------------------------|--------|
| Below about 1100px width the page scrolls sideways and header buttons are cut off                 | done   |
| Two unclosed blocks in `style/main.css` hid every later rule; a test now checks the braces        | done   |

## Already in place (checked)

Draft per conversation and thread, Shift+Enter / configurable send key, @-mention, `:emoji:` and `/command`
autocomplete with the keyboard, formatting toolbar, Alt+↑/↓ between conversations, Esc closes panels,
unread bold and mention badges in the sidebar, muted spaces greyed, collapsible sidebar sections,
status menu with presets and expiry, copy link to a message, permalinks, image gallery, jump to date,
dark / light / system theme, compact density, `prefers-reduced-motion`, focus rings, modal focus trap.
