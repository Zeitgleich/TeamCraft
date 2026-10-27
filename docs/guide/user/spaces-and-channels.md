# Spaces and channels

## Concepts

| Thing          | What it is                                                                                    |
|----------------|-----------------------------------------------------------------------------------------------|
| Space          | A group of channels for a project or team. Shown in the rail on the left                      |
| Channel        | A conversation in a space. **Public** (anyone in the space can join) or **private** (invited) |
| Category       | A folder for channels inside a space                                                          |
| Direct message | A conversation with one person                                                                |
| Group message  | A conversation with a few people (at most 8 by default)                                       |
| Meeting chat   | The chat that belongs to a meeting, see [Calls and meetings](calls-and-meetings.md)           |

Message content and attachments are end-to-end encrypted. Admins can see that a channel exists, who
is in it and how it is configured, but never what is written in it.

## Find your way around

- The **sidebar** groups conversations into Unreads, Favorites, Channels, Integrations and Direct
  messages. Star a conversation to make it a favorite. **Saved** lists messages you saved for later.
- **Quick switcher:** `Ctrl/Cmd+K`, type a name, press Enter. `Alt+Up` / `Alt+Down` moves to the
  previous or next conversation.
- **Browse channels** lists the public channels of a space (also archived ones on request); choose
  **Join** or **Leave**.
- **Unread badges** count unread messages; a stronger badge means you were mentioned.
- The channel header shows the topic, members and the favorite star. A badge marks conversations
  that include **external people** and **archived** channels.

## Create and manage

- **New space** (if your role allows it), **New channel** in a space: name, topic, category,
  *Private (invitation only)* and what new members can read. Members may create channels by default.
- **Channel settings:** rename, topic, move to a category, **archive**, and **make private / public**.
  Making a channel private removes access for people who are not members; making it public lets
  anyone in the space join and read new messages. Everyone sees a system message about it.
- **Add people** from the members panel. Choose whether to **share the earlier history** with them
  (the workspace default applies if you do not choose); guests never get earlier history.
- **Invite people** to a space by e-mail, optionally **as guest** (limited access).
- **Direct messages:** pick a person in the directory or use `/msg`. See [People](people-and-profiles.md).
- **Space settings:** categories (create, rename, delete) and archiving the space.

## Roles inside a channel

Channel **admins** manage members, roles and properties. **Moderators** can remove members and
delete other people's messages but cannot change roles or properties. Space admins manage the space's
channels. See the administrator guide for the full permission model.

## Integration channels

Channels marked as integration channels receive machine output (logs, alerts, CI) from a bridge.
They appear under **Integrations**, show logs as compact rows with level colors and a *Jump to
latest* follow mode, and only notify you for @mentions. See
[Admin: integrations](../admin/integrations-bridge.md).
