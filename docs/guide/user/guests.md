# Guests' view

A guest is an external person with limited access. There are two kinds.


## Guest account

Your admin gave you an account with the **guest** role in the identity provider.

- You sign in like everybody else ([Getting started](getting-started.md)).
- You see **only** the spaces and channels you were added to and nothing else; the user directory
  and other spaces are hidden.
- You can write, react and make calls in your conversations. You can start direct messages only with
  people you share a conversation with (default policy).
- Your history starts when you joined: you do not see older messages.
- Others see a **GUEST** badge next to your name.
- Your admin can disable your account or remove your guest role at any time; sign-in then fails
  with *no access to TeamCraft*, and open sessions end.
- A space invite for a guest can carry an expiry date and a list of channels; you only get those.

## Meeting guest (no account)

You received a personal link like `https://chat.example.com/meeting/<id>#invite=<token>`.

1. Open the link in a browser. No sign-in is needed.
2. Enter your name and choose **Ask to join**.
3. Wait on the page. A host lets you in or turns you away.
4. Once admitted you are in the meeting chat.

The link works **once** and only until shortly after the meeting ends (two hours by default).
If you see *This link does not work*, it was used, expired or the meeting was cancelled: ask the
organizer for a new one. Audio and video for browser guests are coming soon.

Meeting guests show up to others as "Guest (unverified)": nobody has verified your keys.
