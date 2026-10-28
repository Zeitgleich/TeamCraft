# TeamCraft UI – English (source language)
# Spaces and channels: settings, members, joining, browsing, integrations and their log.

## Compact layout (phones): bottom navigation, back, menus; calls in the browser
conversation-more = More
conversation-bell = Notifications for this conversation
conversation-search = Search in conversation ({ $shortcut })
conversation-mute = Mute this conversation
conversation-unmute = Unmute this conversation

## Channel
channel-region-label = Channel { $name }
channel-empty = Say hello and get the conversation going.
channel-empty-title = No messages yet
channel-tabs = Conversation views
channel-tab-messages = Messages
channel-tab-files = Files & links
members-button-label = { $count ->
    [one] Show members, { $count } member
   *[other] Show members, { $count } members
  }
members-title = Members
members-panel-label = Channel members
members-count = { $count ->
    [one] { $count } member
   *[other] { $count } members
  }
bookmarks-label = Channel bookmarks
bookmarks-add = Add bookmark
bookmarks-add-soon = Bookmarks can't be edited yet

## Messages
system-joined = { $name } joined the channel.
system-left = { $name } left the channel.
system-topic-changed = { $name } changed the topic to "{ $topic }".
system-renamed = { $name } renamed the channel to { $channel }.
log-level-debug = DEBUG
log-level-info = INFO
log-level-warn = WARN
log-level-error = ERROR

## Administration
retention-days = Days
retention-messages = Messages
retention-pinned = Keep pinned messages
retention-actions = Actions
retention-forever = Keep all
retention-invalid = Enter a whole number from 1 up to the instance limit, or leave empty.

## Device approval, linking and key changes
leave-pending = Leaving this conversation. It finishes when a member's device has removed you from the encrypted group.
leave-failed = Leaving this conversation failed: the server refused it.
removal-pending = Removing { $name } from this conversation.
removal-failed = Removing { $name } failed: the server refused it.
removal-retry = Retry
removal-unconfirmed = Your removal of { $name } from #{ $channel } isn't confirmed: the server lists them again or someone may have added them back.
removal-unconfirmed-remove = Remove again
removal-unconfirmed-keep = Keep them

## First-run key setup
# ---- Meetings ----
external-badge = External people
external-badge-detail = This conversation includes people from outside your organization.

# Workspace management
space-new-title = New space
space-name = Name
space-description = Description
space-color = Color
space-color-reset = Default color
space-image = Picture
space-image-hint = Shown in the space list instead of the initials. PNG, JPEG or WebP; you choose the square after picking it.
space-image-upload = Upload picture
space-image-change = Change picture
space-image-remove = Remove picture
space-image-uploading = Saving the picture…
space-image-size = That picture is too large.
space-image-unreadable = The picture could not be read.
space-image-forbidden = You are not allowed to change the picture.
space-image-failed = The picture could not be saved. Try again.
space-image-offline = No connection to the server. Try again when you are online.
space-image-demo = The demo keeps no pictures; with a server, this one would be saved now.
channel-image = Picture
channel-image-hint = Shown for this channel in its settings. PNG, JPEG or WebP; you choose the square after picking it.
space-open = Anyone in the workspace can join
space-settings-title = Space settings
space-categories = Categories
space-category-new = New category
space-category-rename = Category name
space-category-delete = Delete category
space-archive-action = Archive this space
space-settings-read-only = Only space admins can change this.
space-join-policy = Who can join
space-join-open = Open
space-join-open-hint = Everyone in the organization can find the space and join it.
space-join-invite-only = Invite only
space-join-invite-only-hint = Only people who are added or invited see the space.
space-members-filter = Find a member
space-members-read-only = Space admins add, remove and change members.
space-members-loading = Loading members…
space-members-hidden = Only members of the space see who is in it.
space-invites-pending = Pending invitations
space-invite-revoke = Withdraw
space-role = Role
space-role-member = Member
space-role-admin = Space admin
space-role-guest = Guest
space-member-remove-confirm = Remove { $name } from the space? They also leave its channels and can no longer read new messages there.
space-member-integration = Integration
space-member-integration-hint = A bot account of an integration. Add or remove it in a channel's settings under Integrations.
space-categories-hint = Categories group the channels in the sidebar for everyone in the space.
space-channels = Channels
space-archive-hint = An archived space and its channels become read-only and leave the sidebar. Members, history and encryption stay as they are.
space-archive-confirm = Archive this space for everyone?
channel-new-title = New channel
channel-name = Name
channel-topic = Topic
channel-private = Private (invitation only)
channel-category = Category
channel-uncategorized = No category
channel-history = What new members can read
history-default = Workspace default
history-from-join = Only messages from when they join
history-full = The full history
channel-settings-title = Channel settings
channel-missing = This channel is not available.
channel-move-category = Move to category
channel-privacy = Privacy
channel-privacy-hint-public = Anyone in the space can find and join this channel.
channel-privacy-hint-private = Only invited people can see this channel.
channel-make-private = Make private
channel-make-public = Make public
channel-make-private-confirm = Make this channel private? People who are not members lose access.
channel-make-public-confirm = Make this channel public? Anyone in the space can join and read new messages.
channel-archive = Archive
channel-archive-action = Archive this channel
channel-roles-hint = Admins manage members, roles and settings. Moderators remove members and delete messages.
channel-roles-read-only = Only channel admins change roles.
channel-settings-read-only = Only channel admins can change this.
channel-role = Role
channel-role-admin = Channel admin
channel-role-moderator = Moderator
channel-role-member = Member
channel-role-unsigned = Not signed
channel-role-unsigned-hint = Only the server lists this role: no admin signed it yet, so members' apps don't count it where a signed role is needed.
channel-role-sign = Sign role
role-change-confirm = Change the role of { $name } to { $role }?
channel-archive-hint = Archived channels are read-only and leave the sidebar. Admins can restore them.
channel-archive-confirm = Archive this channel for everyone?
channel-add-people = Add people
channel-pick-person = Choose a person
channel-share-history = Share the earlier history with them
channel-share-history-on = They can read everything posted so far.
channel-share-history-off = They only see messages from when they join.
channel-add = Add
channel-add-chosen = People to add
channel-already-member = Already in the channel
channel-add-joins-space = People who are not in the space yet join it as members first.
channel-add-space-only = Only people in the space can be added. Space admins can bring others in:
browse-title = Browse channels
browse-filter = Find a channel
browse-archived = Show archived channels
browse-empty = No channels found.
browse-open = Open
browse-join = Join
browse-join-admin = Join as admin
browse-join-admin-hint = A private channel: you join as workspace admin, and its members see that you joined.
browse-join-ask = Ask to join
browse-join-ask-hint = A private channel: its owners decide whether you join as workspace admin, and its members see when you do.
browse-join-requested = Requested
browse-join-requested-hint = The channel's owners decide. The request expires if nobody does.
browse-join-withdraw = Withdraw
browse-leave = Leave
invite-title = Invite people
invite-role = Invite as
invite-role-member = Member: sees the space's public channels
invite-role-guest = Guest: sees only what they are added to
invite-guests-only = You can invite guests to this space. Space admins add members.
invite-link-only = Only space admins can add people to this space. Share the link below: anyone in your organization can join this open space with it.
invite-from-directory = People in your organization
invite-chosen = People to invite
invite-nobody-found = Nobody found. People who have never signed in can be invited by e-mail.
invite-by-email = Invite by e-mail
invite-email-placeholder = name@example.com, …
invite-email-hint = For people without an account yet. They get an e-mail and join the space when they first sign in.
invite-email-invalid = Please check these addresses: { $addresses }
invite-email-too-many = At most { $max } addresses at a time.
invite-message = Personal message (optional)
invite-message-hint = Added to the invitation e-mails.
invite-done = { $count ->
    [one] { $count } person invited.
   *[other] { $count } people invited.
  }
invite-link = Space link
invite-link-copy = Copy link
invite-link-copied = Copied
invite-link-hint = Anyone in your organization who opens this link can join this open space.
space-missing = This space is not available.
invite-send = Send invitation

# Workspace system messages
ws-member-added = { $by } added { $name }
ws-member-added-history = { $by } added { $name } and shared the earlier history
ws-member-removed = { $by } removed { $name }
ws-archived = { $by } archived this channel
ws-unarchived = { $by } restored this channel
ws-made-private = { $by } made this channel private
ws-made-public = { $by } made this channel public

# The server reports the change; no member's announcement named who made it (issue #818).
ws-archived-unattributed = This channel was archived
ws-unarchived-unattributed = This channel was restored
ws-made-private-unattributed = This channel was made private
ws-made-public-unattributed = This channel was made public
ws-pinned = { $by } pinned a message
ws-admin-joined = { $name } joined this channel as a workspace admin
ws-admin-joined-approved = { $name } joined this channel as a workspace admin, approved by { $by }
ws-admin-joined-added = { $name } joined this channel as a workspace admin, added by { $by }
log-toolbar-label = Log view options
log-mode-log = Log
log-mode-chat = Chat
log-filter-level = Minimum level
log-filter-text = Filter log text
log-follow = Follow
log-filter-none = No log entries match the filter.
log-filter-truncated = Showing the newest { $count } matching entries.

# Bookmarks
bookmarks-edit = Edit bookmarks
bookmarks-edit-title = Bookmarks
bookmarks-emoji = Emoji
bookmarks-title = Title
bookmarks-url = Link
bookmarks-up = Move up
bookmarks-down = Move down
bookmarks-remove = Remove bookmark
bookmarks-invalid = Every bookmark needs a title, and links must start with https:// or http://.

## Activity, last seen and call history
join-requests-title = Requests to join
join-request-summary = { $name } asks to join { $channel } as a workspace admin
join-request-summary-here = { $name } asks to join as a workspace admin
join-request-hint = Approving lets them read what is posted from now on, as joining by themselves would. Everybody in the channel sees that a workspace admin joined.
join-request-hint-history = Approving lets them read the earlier history too. Everybody in the channel sees that a workspace admin joined.
join-request-share-history = Also share the earlier history
join-request-not-admin = The signed roster on this device doesn't list them as a workspace admin, so you can only decline.
join-request-owner-not-signed = Your owner role in this channel isn't signed yet, so other members wouldn't accept the admin. Ask a signed admin or a workspace admin to sign your role in the channel's members; until then you can only decline.
join-request-approve = Approve
join-request-decline = Decline

# Spaces: moving channels, muting, scoped search, grouped sidebar, channel retention
ws-moved = { $by } moved this channel to { $space }
ws-moved-unnamed = { $by } moved this channel to another space
ws-moved-unattributed = This channel was moved to { $space }
ws-moved-unnamed-unattributed = This channel was moved to another space
channel-move-space = Move to another space
channel-move-space-hint = Members, history and encryption stay as they are. Only the space the channel is listed in changes.
channel-move-space-choose = Choose a space…
channel-move-space-action = Move channel
channel-move-space-confirm = Move this channel? Everyone in it must already be a member of the new space; the admins of that space manage it from then on.
retention-channel-title = Message retention
retention-channel-hint = Messages older than this, or beyond this count, are deleted for everyone. Leave both fields empty to follow the defaults of the space and the workspace.
retention-channel-effective = In effect: { $value }.
retention-channel-limit = Administrators allow at most { $value }.
retention-channel-saved = Retention saved.
retention-unlimited = keep everything
retention-summary-days = { $days } days
retention-summary-messages = the last { $count } messages
retention-summary-both = { $days } days or the last { $count } messages, whichever is stricter
retention-error-number = Enter whole numbers only.
retention-error-positive = Values must be greater than zero.
retention-error-refused = The server refused this setting. It may exceed the limit set by the administrators, or you may not manage this channel.
space-notifications = Notifications
space-mute-label = Mute this space
space-mute-hint = Its channels stop notifying, unless you set a notification level for a channel itself.
space-mute = Mute { $name }
space-unmute = Unmute { $name }

## Daily-use improvements (docs/ux-backlog.md)
channel-left-toast = You left { $name }

## Getting started (empty workspace)
space-join = Join
spaces-browse-title = Browse spaces
spaces-browse-filter = Find a space
spaces-browse-open = Open spaces you can join
spaces-browse-open-none = There are no open spaces to join.
spaces-browse-mine = Your spaces
spaces-browse-mine-none = You are not in any space yet.
spaces-browse-invite-only-hint = Invite-only spaces show up here as soon as someone invites you.

## Failed actions (an error toast) and the panic overlay
error-admin-join-request-gone = This request is no longer open: it was decided on another device or by another owner, withdrawn, or it expired.
error-admin-join-request-too-soon = You asked to join this channel recently, and that request was declined or withdrawn. You can ask again once it would have expired.
error-guest-dm-not-allowed = Guests can't start conversations on their own. You can answer as soon as someone writes to you first.

## Settings: sections, search and rows (#327)
# Files view and setup checklist (issue #301)
channel-kind = Channel type
channel-mode = Mode
channel-mode-normal = Normal
channel-mode-log = Log channel
channel-mode-normal-hint = A normal channel for conversation. Bots can post here too.
channel-mode-log-hint = Machine output: shown as a log, kept for the log retention period, and pushes only for mentions.
channel-mode-public-hint = Make the channel private to use it as a log channel.
channel-bots = Bots in this channel
channel-bots-none = No bots yet.
channel-bots-add = Add a bot
channel-bots-search = Search bots by name
channel-bots-hint = A bot reads and writes like a member but sees only what is written after it joined. Admins create bots in the admin console.
channel-bots-add-button = Add
channel-bots-remove = Remove
channel-bots-remove-confirm = Remove bot
channel-sources = Sources
channel-sources-none = No sources yet.
channel-sources-added-by = Added by { $name } on { $date }
channel-sources-revoke = Revoke
channel-sources-revoke-confirm = Revoke source
channel-sources-name = Source name
channel-sources-name-placeholder = e.g. ci or nas-logs
channel-sources-kind = Kind
channel-sources-kind-webhook = Webhook
channel-sources-kind-log = Log lines
channel-sources-kind-other = Other
channel-sources-danger-alert = Attachments marked "danger" mention people
channel-sources-add = Add source
channel-sources-hint = Point Grafana, CI or journalctl at the bridge. A new source reaches the bridge as an encrypted message in this channel; there is nothing to configure or restart.
channel-sources-needs-bot = Add a bot to this channel first: the bridge learns about sources from the channel.
channel-sources-admins-only = Only workspace admins can add sources for now: the bridge trusts their signed role and doesn't read channel roles yet.
channel-sources-ask-admins = Only workspace admins can add sources for now: the bridge trusts their signed role and doesn't read channel roles yet. Ask { $admins }.
channel-sources-no-random = No random source is available, so no token could be made. Try another browser.
channel-sources-created = { $name } is ready. Copy its address now: it is shown only once.
channel-sources-url = Address
channel-sources-example = Example
channel-sources-shown-once = The address contains the source's secret token. Anyone who has it can post into this channel, so keep it private. To replace it, revoke the source and add a new one.
channel-sources-done = Done
channel-sources-bridge = Where the bridge runs
channel-sources-bridge-instance = On this server
channel-sources-bridge-loopback = On its own machine
channel-sources-bridge-instance-hint = The bridge runs beside the server (deployment option teamcraft_bridge); producers post to this server's address.
channel-sources-bridge-loopback-hint = The bridge runs next to the producers and listens on its default local address. Use the bridge's own address if you opened its listener.
