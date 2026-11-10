# TeamCraft UI – English (source language)
# Messages, threads and the composer.

## Compact layout (phones): bottom navigation, back, menus; calls in the browser
composer-camera = Take a photo or video
composer-photos = Photos and videos

## Channel
messages-label = Messages
jump-to-latest = Jump to latest

## Composer
composer-placeholder = Write to { $name }
composer-placeholder-untitled = Write a message
composer-placeholder-reply = Reply…
composer-readonly = You can't post in this channel.
composer-archived = This channel is archived. You can read it, but nobody can post here.
composer-label = Message
composer-attachments = Attachments
composer-remove-attachment = Remove attachment
composer-draft-file-failed = Not uploaded
composer-draft-file-expired = Expired
composer-draft-files-expired = A file in this draft was kept too long and is gone. Remove it, or attach it again.
composer-attach = Attach files
composer-emoji = Add emoji
composer-send = Send message
composer-hint-enter = Enter to send, { $newline } for a new line
composer-hint-ctrl-enter = { $send } to send, Enter for a new line

## Messages
message-edited = (edited)
message-actions = Message actions
action-react = Add reaction
action-reply = Reply in thread
reaction-label = { $emoji } reaction, { $count } people

## Threads
thread-replies = { $count ->
    [one] { $count } reply
   *[other] { $count } replies
  }
thread-new = { $count ->
    [one] { $count } new
   *[other] { $count } new
  }
thread-panel-label = Thread
thread-title = Thread
thread-follow = Follow
thread-following = Following
thread-messages-label = Thread messages
thread-replies-divider = Replies
thread-last-reply = last reply { $time }
date-jump-label = Jump to a day
date-jump-open = { $date }, jump to a day
date-jump-today = Today
date-jump-yesterday = Yesterday
date-jump-last-week = A week ago
date-jump-pick = Pick a day

## First-run key setup
message-seen-by = Seen by { $names }
typing-one = { $name } is typing
typing-two = { $first } and { $second } are typing
typing-several = Several people are typing
# In the chat list and under the name of a direct conversation, while the other person types.
typing-direct = typing

# ---- Meetings ----
composer-confirm-channel = This message notifies everyone in this conversation ({ $count } people). Send it?
composer-confirm-group = This message notifies { $count } people in the mentioned groups. Send it?
composer-confirm-group-capped = This message notifies only the first { $cap } of the { $count } people in the mentioned groups. Send it?
composer-group-outside = { $count ->
    [one] 1 member of the mentioned groups isn't in this conversation and won't be notified.
   *[other] { $count } members of the mentioned groups aren't in this conversation and won't be notified.
}
composer-confirm-send = Send anyway
jump-to-date = Jump to date

# Message actions
message-more = More actions
message-history = Earlier versions
message-deleted = This message was deleted
message-placeholder-waiting = Waiting for this message
message-placeholder-not-available = Not available on this device
message-placeholder-failed = This message couldn't be decrypted
action-quote = Quote reply
action-forward = Forward
action-pin = Pin to conversation
action-unpin = Unpin
action-save = Save for later
action-unsave = Remove from saved
link-menu-label = Link
link-menu-open = Open in browser
action-copy-link = Copy link
action-mark-unread = Mark as unread
action-edit = Edit
action-delete = Delete
quote-unavailable = Original message not available
quote-replying-to = Replying to { $name }
quote-cancel = Cancel reply
forwarded-from = Forwarded from { $name } { $conversation }
priority-important = Important
priority-urgent = Urgent
ack-confirm = Confirm read
ack-done = Confirmed
forward-title = Forward message
forward-filter = Find a conversation
forward-comment = Add a comment (optional)
forward-send = Forward
pins-title = Pinned messages
pins-empty = Pin a message from its menu to keep it here for everyone.
pins-empty-title = No pinned messages

# Files and link previews
link-open-new = Open link in a new tab
composer-file-too-large = { $name } is larger than the upload limit of { $limit }.
composer-file-unreadable = { $name } could not be read.
composer-file-unstaged = { $name } could not be prepared for sending.
permalink-label = Message in { $name }
permalink-unavailable = Link to a conversation you are not in
permalink-hidden = Message link
message-table-label = Table

# Composer: mentions, slash commands, formatting
mention-channel = Notify everyone in this conversation
mention-admins = Admins and owners
composer-suggestions = Suggestions
composer-format = Formatting
composer-format-toggle = Show formatting
format-bold = Bold
format-italic = Italic
format-strike = Strikethrough
format-code = Code
format-code-block = Code block
format-table = Table
format-underline = Underline
format-style = Text style
format-paragraph = Normal text
format-heading-1 = Heading 1
format-heading-2 = Heading 2
format-heading-3 = Heading 3
format-link = Link
format-numbered-list = Numbered list
format-bulleted-list = Bulleted list
format-quote = Quote
composer-link-text = Text
composer-link-url = Link address
composer-link-invalid = Enter an address that starts with http:// or https://.
composer-attach-file = Upload a file
composer-attach-media = Photo or video
composer-attach-camera = Take a photo
composer-mention = Mention someone
composer-commands = Commands
composer-send-options = Send options
composer-priority = Priority
priority-normal = Normal
composer-silent = Send without notification
composer-silent-detail = Arrives quietly, without a sound or push.
composer-ack = Ask for read confirmation
composer-ack-detail = Everyone can confirm they have read it.
composer-flag-silent = Silent
composer-flag-ack = Read confirmation
slash-missing-argument = Usage: { $usage }
slash-bad-reminder = Say when: "in 10m", "in 2h", "in 1d" or "at 14:30", then the text.
slash-help-title = Commands (type // to send a message that starts with a slash):
slash-not-found = Nothing found for "{ $name }".
slash-leave-last-member = You are the only one in this channel: leaving deletes it. Use the channel's menu to confirm.
slash-status-set = Status set.
slash-status-cleared = Status cleared.
slash-status-presets-only = Your workspace allows only its preset statuses. Pick one in the account menu.
slash-status-disabled = Statuses are switched off in this workspace.
slash-kicked = { $name } was removed from this conversation.
slash-invited = { $name } was added to this conversation.
slash-msg-send-there = Conversation opened. Write your message there.
slash-groupmsg-unsupported = Group conversations are not available yet.
slash-muted = Conversation muted.
slash-unmuted = Conversation unmuted.
slash-channel-only = This command works only in channels.
slash-not-allowed = You don't have the right to do that here.
slash-reminder-set = Reminder set for { $time }.
slash-meet-starting = Starting a meeting…
slash-meet-unavailable = Meetings need a signed-in session.
reminder-due = Reminder: { $text }

# Bookmarks
action-bookmark-file = Add file to bookmarks
history-title = Edit history
history-current = Current version
history-replaced = Replaced { $time }

## Daily-use improvements (docs/ux-backlog.md)
new-messages = New messages
jump-to-latest-new = { $count ->
    [one] 1 new message
   *[other] { $count } new messages
  }
jump-to-reaction = { $count ->
    [one] 1 new reaction to your messages, jump to it
   *[other] { $count } new reactions to your messages, jump to the first
  }
action-react-with = React with { $emoji }
emoji-picker-title = Emoji
emoji-search = Search emoji
emoji-frequent = Frequently used
emoji-workspace = Workspace
emoji-categories = Emoji categories
emoji-group-smileys = Smileys & emotion
emoji-group-people = People & body
emoji-group-nature = Animals & nature
emoji-group-food = Food & drink
emoji-group-travel = Travel & places
emoji-group-activities = Activities
emoji-group-objects = Objects
emoji-group-symbols = Symbols
emoji-group-flags = Flags
emoji-skin-tone = Skin tone
emoji-tone-default = Default skin tone
emoji-tone-light = Light skin tone
emoji-tone-medium-light = Medium-light skin tone
emoji-tone-medium = Medium skin tone
emoji-tone-medium-dark = Medium-dark skin tone
emoji-tone-dark = Dark skin tone
emoji-none = No emoji found
message-show-more = Show more
message-show-less = Show less
message-sending = Sending…
message-status-sent = Sent
message-status-delivered = Delivered
message-status-read = Read
message-not-sent = Not sent yet, retrying automatically
message-send-failed = Not sent
message-send-retry = Try again
message-send-discard = Delete
message-deleted-toast = Message deleted
composer-also-send = Also send to { $name }
pins-jump = Jump to message

## Failed actions (an error toast) and the panic overlay
action-running = Working…

## Message toolbar and action sheet (#320)
action-with-shortcut = { $action } ({ $shortcut })
action-copy-text = Copy text
action-copied = Copied
action-copy-failed = Couldn’t copy to the clipboard
action-copy-link-unready = The link isn’t ready yet. Try again in a moment.
message-action-sheet = Actions for this message

## Previews of linked messages (#319)
linked-message-from = In { $conversation }
linked-message-from-thread = From a thread in { $conversation }
linked-message-open = Open message
linked-message-deleted = This message was deleted
linked-message-not-loaded = Message not loaded yet
linked-message-unavailable = Message from a conversation you can’t see

## Settings: sections, search and rows (#327)
# Files view and setup checklist (issue #301)
composer-placeholder-notes = Note something for yourself
messages-loading = Loading messages…
older-history-loading = Loading older messages…
older-history-failed = Older messages could not be loaded.
older-history-retry = Try again
history-gap-loading = Loading messages…
history-gap-failed = Messages could not be loaded.

# Send later (docs/scheduled-messages.md)
schedule-heading = Send later
schedule-online-hint = Goes out on time, even with your devices off, unless the group changes and all your devices stay off.
schedule-write-first = Write something first.
schedule-no-files = Messages with files can't wait yet: send them now.
schedule-no-command = Commands run right away and can't be scheduled.
schedule-in-one-hour = In 1 hour
schedule-tomorrow = Tomorrow morning
schedule-monday = Monday morning
schedule-custom = Pick a time
schedule-custom-label = Send at
schedule-confirm = Schedule
schedule-done = Scheduled for { $time }.
schedule-refused-off = Your workspace doesn't allow scheduling messages.
schedule-refused-empty = Write something first.
schedule-refused-past = Pick a time in the future.
schedule-refused-far = { $count ->
    [one] Pick a time at most 1 day ahead.
   *[other] Pick a time at most { $count } days ahead.
}
schedule-refused-many = You have { $max } scheduled messages already. Send or cancel one first.
scheduled-title = Scheduled messages
scheduled-count = { $count ->
    [one] 1 message waiting
   *[other] { $count } messages waiting
}
scheduled-note = The server keeps these, encrypted, and sends them on time, even with your devices off. Only if the group changes (someone joins or leaves) and all your devices stay off does a message wait for one of them to come back.
scheduled-empty-title = Nothing scheduled
scheduled-empty = Use "Send later" in the send menu next to the send button.
scheduled-unknown-conversation = Unknown conversation
scheduled-status-waiting = Goes out on time.
scheduled-status-needs-sealing = The group changed. One of your devices updates the encryption by itself; it goes out on time if one does in time.
scheduled-status-stale = Late: the group changed and none of your devices was on. It goes out as soon as one is.
scheduled-status-policy-off = Your workspace turned scheduling off. It stays here until you send or cancel it.
scheduled-status-not-a-member = You're no longer in this conversation, so it won't go out.
scheduled-status-no-permission = You may no longer post in this conversation, so it didn't go out.
scheduled-status-conversation-gone = This conversation was deleted, so it won't go out.
scheduled-status-thread-gone = The thread it answers is gone, so it didn't go out.
scheduled-status-unreadable = This device can't read it yet. It still goes out.
scheduled-status-unknown = Your app can't tell what happens to this message. Update it.
scheduled-status-removed = The server removed it after holding it this long, so it won't go out. The text is only on your devices now: copy it if you still need it.
scheduled-removal-date = The server removes it on { $date }. Copy the text before then if you need it.
scheduled-dismiss = Remove from list
scheduled-dismiss-question = Remove it from this device? Its text is gone from here then.
scheduled-dismiss-confirm = Remove it
scheduled-send-now = Send now
scheduled-edit = Edit
scheduled-edit-text = Message
scheduled-save = Save
scheduled-cancel = Cancel message
scheduled-cancel-question = Cancel this message? Nobody will see it.
scheduled-cancel-confirm = Cancel it
scheduled-keep = Keep it
schedule-refused-limit = You have as many scheduled messages as your workspace allows. Send or cancel one first.

## Polls (issue #205)
composer-poll = Poll
poll-title = Poll
poll-fallback-note = Update TeamCraft to vote in this poll.
poll-new-title = New poll
poll-question = Question
poll-options = Options
poll-option = Option { $number }
poll-add-option = Add option
poll-remove-option = Remove option { $number }
poll-multiple = Allow several answers
poll-hide-voters = Hide who voted
poll-hide-voters-hint = The poll shows only counts. It isn't anonymous: every member's app still receives each vote with its sender.
poll-send = Send poll
poll-error-question = Enter a question.
poll-error-question-long = A question can have at most { $max } characters.
poll-error-options = A poll needs at least { $min } options.
poll-error-options-many = A poll can have at most { $max } options.
poll-error-option-long = An option can have at most { $max } characters.
poll-error-option-duplicate = This option is already in the poll.
poll-single-choice = Choose one
poll-multiple-choice = Choose any
poll-hidden-voters = Names hidden
poll-closed = Closed
poll-votes = { $count ->
    [one] { $count } vote
   *[other] { $count } votes
  }
poll-option-count = { $count } · { $percent }%
poll-option-label = { $option }: { $count ->
    [one] { $count } vote
   *[other] { $count } votes
  }, { $percent } percent
poll-voters-more = { $count ->
    [one] and { $count } more
   *[other] and { $count } more
  }
poll-close = Close poll
poll-close-confirm = Close for everyone? Later votes won't count.
poll-unknown-choice = This poll needs a newer version of TeamCraft to vote.
