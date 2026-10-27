# TeamCraft UI – English (source language)
# Files, pictures and videos, previews, the lightbox and share links.

## Threads
files-count = { $count ->
    [one] { $count } file
   *[other] { $count } files
  }
files-download-all = Download all

## First-run key setup
# ---- Meetings ----
share-page-loading = Checking the link…
share-page-title = A file was shared with you
share-page-size = Size: { $size }
share-page-password = Password
share-page-wrong-password = That password is not right.
share-page-download = Download
share-page-working = Decrypting…
share-page-note = The file is decrypted in your browser. The key stays in this link and is never sent to the server.
share-page-done-title = Your file is ready
share-page-save = Save file
share-page-failed-title = This link does not work
share-page-bad-link = The link is incomplete or damaged. Ask the sender to share it again and copy the whole link.
share-page-unavailable = The link has expired, was used up or revoked. Ask the sender for a new one.
share-page-too-large = This file is too large to open in the browser. Ask the sender to share it another way.
share-page-network = The server could not be reached. Check your connection and reload the page.
share-page-damaged = The file could not be decrypted. The link may belong to another file, or the download was damaged. Try again.
share-page-retry = Try again

# Message actions
attachment-menu-label = File
attachment-menu-open = Open
attachment-menu-save = Save…
attachment-menu-copy-image = Copy image
file-save-failed = The file couldn’t be saved.

# Files and link previews
files-empty = Files and images shared here show up in this tab.
files-empty-title = No files yet
shared-kind-label = What is shared
shared-kind-files = Files
shared-kind-links = Links
links-empty-title = No links yet
links-empty = Links shared in this conversation show up here.
links-more = Show more
links-show-message = Show message
file-download = Download
file-progress = Transfer progress
file-saved = Saved on this device
file-download-network = Download failed. Check your connection and try again.
file-download-storage-unavailable = The file storage is unavailable right now. Try again in a moment.
file-download-missing = This file is no longer on the server.
file-download-intercepted = The network blocked the download: a sign-in page answered, or a certificate isn’t trusted.
file-download-corrupted = The file arrived damaged and wasn’t saved.
file-save-unwritable = The file couldn’t be written there. Pick another folder, or check that TeamCraft may write to it.
file-too-large = File is too large
file-retry = Try again
file-upload-stalled = Upload failed. The message waits until the file is sent.
file-uploading = Uploading…
file-cancel-upload = Cancel upload
file-progress-percent = { $percent } %
file-progress-bytes = { $done } / { $total }
file-upload-retrying = Retrying…
file-upload-failed = Upload failed
file-upload-storage-unavailable = The file storage is unavailable right now. Try again in a moment.
file-upload-unreadable = The file could not be read any more. Attach it again.
file-preview = Show preview
preview-unavailable = The file could not be loaded. Check your connection and try again.
preview-show-source = Show source
preview-show-formatted = Show formatted
preview-html-note = Shown without scripts and without loading anything from the web.
preview-links = { $count ->
    [one] { $count } link in this file
   *[other] { $count } links in this file
  }
preview-pdf-unreadable = This PDF could not be opened. Download it to view it elsewhere.
preview-zoom-in = Zoom in
preview-zoom-out = Zoom out
preview-page = Page
preview-find = Find in document
preview-find-none = No match
preview-too-large-title = Too large to preview
preview-too-large = This file is { $size }, more than the { $limit } a preview opens. Download it to open it, or raise the limit in Settings under Preferences.
preview-download-to-open = Download to open
text-preview-preparing = Preparing the preview…
text-preview-binary = This file doesn’t look like text, so it can’t be previewed. Download it to open it with another app.
text-preview-tools = Text tools
text-preview-content = Content of { $name }
text-preview-empty = This file is empty.
text-preview-find = Find in file
text-preview-find-count = { $current } of { $total }
text-preview-find-none = No match
text-preview-find-previous = Previous match
text-preview-find-next = Next match
text-preview-go-to-line = Go to line
text-preview-line = Line
text-preview-level = Levels shown
text-preview-level-all = All levels
text-preview-level-info = Info and above
text-preview-level-warn = Warnings and errors
text-preview-level-error = Errors only
text-preview-wrap = Wrap lines
text-preview-colors = Colors
text-preview-format-json = Format JSON
text-preview-copy = Copy the selection, or all lines shown
text-preview-filtered = { $shown } of { $total } lines shown.
text-preview-long-lines = Very long lines are shown in parts.
text-preview-json-invalid = Not valid JSON (line { $line }, column { $column }), so it is shown as it is.
text-preview-json-too-deep = Nested too deeply to format, so it is shown as it is.
text-preview-json-too-large = Too large to format, so it is shown as it is.
text-preview-truncated = Only the first { $count } lines are shown. Download the file to see all of it.
text-preview-encoding-repaired = Some bytes are not valid UTF-8 and are shown as replacement characters.
text-preview-encoding-utf16 = Read as UTF-16.
text-preview-encoding-latin1 = Not UTF-8, so it is read as Latin-1 (Windows-1252).
file-share = Share link…

# Workspace management
image-pick-type = Pictures must be PNG, JPEG or WebP files.
image-pick-size = That picture is too large (20 MB at most).
image-pick-unreadable = The picture could not be read.
crop-title-avatar = Crop profile picture
crop-title-space = Crop space picture
crop-title-channel = Crop channel picture
crop-stage = Picture section
crop-help = Drag to move, scroll or pinch to zoom. Keyboard: arrow keys move, + and − zoom, R rotates.
crop-preparing = Preparing the picture…
crop-zoom = Zoom
crop-zoom-value = { $percent } %
crop-zoom-in = Zoom in
crop-zoom-out = Zoom out
crop-rotate = Rotate by 90°
crop-reset = Reset
crop-preview = Preview
crop-preview-profile = Profile
crop-preview-messages = Messages
crop-preview-lists = Lists
crop-preview-settings = Settings
crop-preview-header = Channel header
crop-preview-rail = Space bar
crop-preview-sidebar = Sidebar
crop-apply = Apply
crop-retry = Try again
crop-uploading = Saving…
preview-label = Preview of { $name }
preview-badge = Preview
preview-title = Join to read along
preview-body = Messages are end-to-end encrypted for the members of this channel. Once you've joined, you can read and write here.
preview-bar = You're previewing #{ $name }
preview-details-button = Details
preview-join = Join channel
preview-joining = Joining…
preview-topic = Topic
preview-no-topic = No topic set.
preview-archived = This channel is archived and can't be joined.
preview-missing-title = Channel not found
preview-missing = It may have been made private or deleted.

## Daily-use improvements (docs/ux-backlog.md)
drop-files-here = Drop to attach
drop-files-unreadable = The dropped files couldn’t be read. Attach them with the + button instead.
drop-file-too-large = { $name } is larger than { $limit }, too large to attach by dropping it. Attach it with the + button instead.
paste-picture-failed = The picture on the clipboard couldn’t be pasted.

## Window controls of the desktop app's own title bar
# Image lightbox
lightbox-toolbar = File actions
lightbox-position = { $index } of { $count }
lightbox-previous = Previous image
lightbox-next = Next image
lightbox-zoom-in = Zoom in
lightbox-zoom-out = Zoom out
lightbox-zoom-reset = Zoom { $zoom }, fit to window
lightbox-show-all = All images and videos in the conversation
lightbox-save = Download
lightbox-saving = Saving…
lightbox-forward = Forward message
lightbox-loading = Loading the full file…
lightbox-failed = This file could not be shown.
lightbox-gone = This file is no longer available.
lightbox-too-large = This file is too large to show here. Download it instead.
lightbox-shortcut = { $action } ({ $key })
lightbox-close-tip = Close (Esc)
lightbox-view-tools = View
lightbox-rotate = Rotate
lightbox-zoom = Zoom
lightbox-fit = Fit to window
lightbox-play = Play
lightbox-pause = Pause
lightbox-thread-show = Show thread
lightbox-thread-hide = Hide thread
lightbox-more = More actions
lightbox-view-thread = View thread
lightbox-jump = Jump to message
lightbox-saved-added = Added to Saved
lightbox-saved-removed = Removed from Saved
lightbox-copy-link = Copy link to message
lightbox-link-copied = Link copied
lightbox-link-unavailable = The link is not ready yet. Try again in a moment.
lightbox-share-copied = Share link copied
share-title = Share link
share-active = Active links
share-new = New link
share-expiry = Link lasts
share-expiry-hour = 1 hour
share-expiry-day = 1 day
share-expiry-week = 7 days
share-expiry-month = 30 days
share-limit = Download limit
share-limit-none = No limit
share-password = Password (optional)
share-password-hint = At least { $count } characters; longer is better, a few random words work well. Anyone with the link can try passwords offline, so a short one protects little. The password is not sent to the server; give it to people another way than the link.
share-create = Create link
share-created = Share link created
share-copy = Copy
share-revoke = Revoke
share-revoked = Share link revoked
share-expires = Expires { $when }
share-downloads = { $used } downloads
share-downloads-limited = { $used } of { $max } downloads
share-has-password = Password
lightbox-copy-image = Copy image
lightbox-image-copied = Image copied
lightbox-copy-failed = The image could not be copied.
lightbox-details = File details
lightbox-details-close = Close file details
lightbox-details-name = Name
lightbox-details-type = Type
lightbox-details-size = Size
lightbox-details-dimensions = Dimensions
lightbox-details-dimensions-value = { $width } × { $height } px
lightbox-details-sender = Sent by
lightbox-details-sent = Sent
lightbox-thumbnails = Files in this view
lightbox-thumbnail = Show { $name }
media-open = Open { $name }
video-play = Play { $name }
video-expand = Open larger
video-play-button = Play
video-pause = Pause
video-seek = Playback position
video-position = { $current } of { $total }
video-mute = Mute
video-unmute = Unmute
video-volume = Volume
video-fullscreen = Full screen
video-exit-fullscreen = Exit full screen

## Settings: sections, search and rows (#327)
# Files view and setup checklist (issue #301)
files-title = Files
files-filter = Filter files
files-filter-all = All
files-filter-media = Images and videos
files-filter-documents = Documents
files-all-empty = Files and images shared in any of your conversations show up here.
