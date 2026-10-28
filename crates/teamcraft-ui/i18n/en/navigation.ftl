# TeamCraft UI – English (source language)
# The rail, sidebar and bottom navigation, the account menu, home pages, activity and getting started.

## Top bar and account menu
account-menu-label = Account menu for { $name }, status { $status }
menu-settings = Settings
menu-log-out = Log out

## Space rail
rail-label = Spaces
rail-add-space = Browse or create spaces
rail-space-mentions = { $name }, { $count ->
    [one] { $count } unread mention
   *[other] { $count } unread mentions
  }
rail-space-unread = { $name }, unread messages
rail-chats-unread = Chats, { $count } unread
rail-chats = Chats
rail-activity = Activity
rail-activity-tip = Mentions of you, replies in threads you follow and reactions to your messages
rail-activity-unread = Activity, { $count } unread
rail-activity-mentions = Activity, { $count } unread, including mentions
chats-title = Chats
chats-filter = Filter chats
home-filters = Show
home-filter-all = All
home-filter-unread = Unread
home-filter-people = People
home-filter-groups = Groups
dm-new = New message
dm-options = List options
dm-filter-empty = No direct messages match.
dm-sort = Sort
dm-sort-recent = Newest first
dm-sort-alphabetical = Alphabetically
dm-item-unread = { $name }, { $count } unread
dm-empty = Direct messages with one or several colleagues show up here, outside your spaces.
dm-home-hint = Pick a conversation on the left, or write to one or several colleagues.
dm-new-chosen = Recipients
dm-new-remove = Remove { $name }
dm-new-filter = Find people by name
dm-new-people = People
dm-new-hint = Pick one person for a direct message, or several for a group.
dm-new-start = Open conversation
dm-new-start-group = Start group

## Compact layout (phones): bottom navigation, back, menus; calls in the browser
bottom-nav-label = Main navigation
bottom-nav-spaces = Spaces
bottom-nav-you = You

## Sidebar
sidebar-label = Channels and direct messages
sidebar-threads = Threads
sidebar-unreads = Unreads
sidebar-favorites = Favorites
sidebar-channels = Channels
sidebar-integrations = Integrations
sidebar-direct-messages = Direct messages
sidebar-deactivated-accounts = Deactivated accounts
sidebar-item-mentions = { $name }, { $count } mentions
sidebar-item-unread = { $name }, { $count ->
    [one] 1 unread message
   *[other] { $count } unread messages
  }
sidebar-item-unread-mentions = { $name }, { $unread } unread messages, { $count ->
    [one] 1 mention
   *[other] { $count } mentions
  }
sidebar-resize = Resize the sidebar
sidebar-item-muted = Notifications muted
sidebar-item-muted-label = { $label }, notifications muted
sidebar-item-catching-up = { $name }, catching up

## Channel
favorite-add = Add to favorites
favorite-remove = Remove from favorites

## Administration
menu-administration = Administration
brand-home-label = Home

## Device approval, linking and key changes
dm-setup-pending = Setting up encryption… The other side's group is being joined; messages can be sent once it is done.
dm-setup-failed = Encryption couldn't be set up for this conversation yet. Retry asks the other side's devices to let this device in.

## Sign-in
menu-switch-account = Switch account
menu-log-out-demo = You are using demo data, there is nothing to sign out of.

## Search
sidebar-show-more = { $count ->
    [one] Show { $count } more conversation
   *[other] Show { $count } more conversations
  }

## First-run key setup
# ---- Meetings ----
sidebar-meetings = Meetings

# Message actions
saved-title = Saved items
saved-open = Open conversation
saved-empty = Use "Save for later" in a message's menu.
saved-empty-title = Nothing saved yet
sidebar-saved = Saved

# Profiles, people and groups (v1 REST UI)
sidebar-people = People

## Activity, last seen and call history
sidebar-activity = Activity
sidebar-calls = Calls
activity-title = Activity
activity-filter = Filter activity
activity-all = All
activity-unread = Unread
activity-mentions = Mentions
activity-threads = Threads
activity-empty-title = You're all caught up
activity-mark-read = Mark all as read
activity-empty = Mentions, reactions to your messages and replies in your threads show up here.
activity-reactions = Reactions
activity-empty-mentions-title = No mentions
activity-empty-mentions = When someone mentions you with @ in a channel or a chat, it shows up here.
activity-empty-threads-title = No threads you follow
activity-empty-threads = Threads you start, reply to or follow show up here, with their new replies.
activity-empty-reactions-title = No reactions
activity-empty-reactions = When someone reacts to one of your messages, it shows up here.
activity-thread-new = { $count ->
    [one] { $count } new reply in { $channel }, in a thread you follow
   *[other] { $count } new replies in { $channel }, in a thread you follow
  }
activity-thread-quiet = A thread you follow in { $channel }
activity-place-title = What happened while you were away
activity-place-body = Choose an entry on the left to open it here: mentions of you, replies in threads you follow and reactions to your messages.
activity-unread-mark = Unread
activity-mentioned = { $name } mentioned you in { $channel }
activity-reacted = { $name } reacted { $emoji } to your message in { $channel }
activity-replied = { $name } replied in a thread in { $channel }

# Spaces: moving channels, muting, scoped search, grouped sidebar, channel retention
sidebar-all-spaces = All spaces

## Daily-use improvements (docs/ux-backlog.md)
sidebar-mark-all-read = Mark all as read

## Getting started (empty workspace)
start-title = Welcome, { $name }
start-empty-subtitle = Your workspace is still empty. Start with a space for your team, then add channels and invite your colleagues.
start-guest-subtitle = You are not in any conversation yet. The person who invited you will add you to the right channels.
start-space-subtitle = Next steps for { $name }:
start-create-space = Create a space
start-create-space-hint = A space holds the channels of a team, a department or a project.
start-create-channel = Create the first channel
start-create-channel-hint = Channels are where the team talks, for example #general.
start-browse-channels = Browse channels
start-browse-channels-hint = Join the channels that are already there.
start-browse-spaces = Browse spaces
start-browse-spaces-hint = Find the open spaces of other teams and join them.
start-invite = Invite colleagues
start-invite-hint = Add colleagues from the directory or invite them by e-mail, as members or guests.
start-find-people = Find people
start-find-people-hint = Everyone who has signed in already; start a direct message.
start-join-heading = Spaces you can join

## Settings: sections, search and rows (#327)
# Home pages (issue #585)
page-today = Today
page-yesterday = Yesterday
saved-count = { $count ->
    [one] { $count } saved message
   *[other] { $count } saved messages
  }

# Shell (issue #299)
sidebar-filter = Filter conversations
rail-create = Create
rail-create-short = Create
rail-focus = Focus mode: pause notifications
rail-focus-short = Focus
focus-30-minutes = For 30 minutes

# Files view and setup checklist (issue #301)
sidebar-files = Files
sidebar-shortcuts = Shortcuts
setup-title = Set up your workspace
setup-progress = { $done } of { $total }
setup-dismiss = Hide the checklist
setup-picture = Add a profile picture
setup-picture-hint = So colleagues recognize you.
setup-app = Get the mobile app
setup-app-hint = Calls and messages on your phone.
setup-channels = Browse channels
setup-channels-hint = Join the ones that matter to you.
setup-invite = Invite people
setup-invite-hint = Bring your team along.
notes-title = Notes
notes-empty-title = Nothing noted yet
notes-empty = Keep ideas, links and files here. Only your own devices can read them.
