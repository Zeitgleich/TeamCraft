# Spaces and channels

## Concepts

| Thing          | What it is                                                                                  |
|----------------|---------------------------------------------------------------------------------------------|
| Space          | A group of channels for a project or team. Shown in the rail on the left                    |
| Channel        | A conversation in a space. **Public** (everyone in the space) or **private** (members only) |
| Direct message | A conversation with one person                                                              |
| Group message  | A conversation with a few people (at most 8 by default)                                     |
| Meeting chat   | The chat that belongs to a meeting, see [Calls and meetings](calls-and-meetings.md)         |

Message content and attachments are end-to-end encrypted. Admins can see
that a channel exists, who is in it and how it is configured, but never what is written in it.

## Find your way around

- The **sidebar** groups conversations into Unreads, Favorites, Channels, Integrations and Direct
  messages. Star a conversation to make it a favorite.
- **Quick switcher:** `Ctrl/Cmd+K`, type a name, press Enter. `Alt+Up` / `Alt+Down` moves to the
  previous or next conversation.
- **Unread badges** count unread messages; a stronger badge means you were mentioned.
- The channel header shows the topic, members and the favorite star.

## Create a channel or start a message

The server and the client core support creating channels, opening direct messages and adding
members (members may create channels by default; adding members needs the channel or space
permission). **The desktop UI has no buttons for these yet** (coming soon). Until then, ask an
admin or use the API.

If a channel's history policy is "full", people added later can read earlier messages; with "from
join" they only see what is written after they join. Guests always get "from join".

## Integration channels

Channels marked as integration channels receive machine output (logs, alerts, CI) from a bridge.
They appear under **Integrations**, show logs as compact rows and only notify you for @mentions.
See [Admin: integrations](../admin/integrations-bridge.md).

## Not available yet

- Buttons to create channels, start DMs and add members; drag-and-drop ordering of the sidebar;
  a channel browser.
- Archiving, converting a channel between public and private, and owner assignment from the app.
