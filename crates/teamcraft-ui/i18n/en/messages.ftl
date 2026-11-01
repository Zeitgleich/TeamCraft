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
composer-label = Message
composer-attachments = Attachments
composer-remove-attachment = Remove attachment
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
typing-one = { $name } is typing…
typing-two = { $first } and { $second } are typing…
typing-several = Several people are typing…
# In the chat list and under the name of a direct conversation, while the other person types.
typing-direct = typing…

# ---- Meetings ----
composer-confirm-channel = This message notifies everyone in this conversation ({ $count } people). Send it?
composer-confirm-send = Send anyway
jump-to-date = Jump to date

# Message actions
message-more = More actions
message-history = Earlier versions
message-deleted = This message was deleted
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

# Composer: mentions, slash commands, formatting
mention-channel = Notify everyone in this conversation
composer-suggestions = Suggestions
composer-format = Formatting
composer-format-toggle = Show formatting
format-bold = Bold
format-italic = Italic
format-strike = Strikethrough
format-code = Code
format-code-block = Code block
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
action-react-with = React with { $emoji }
emoji-picker-title = Emoji
emoji-search = Search emoji
emoji-frequent = Frequently used
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
