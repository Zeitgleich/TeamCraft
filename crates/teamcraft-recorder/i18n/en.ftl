# TeamCraft recorder bot – English (source language)
# What the recorder bot posts into a call's conversation (e2ee.md §8.3, #2273).

## A recording
posted = Recording of the call ({ $minutes } min).
posted-part = Recording of the call, part { $part } ({ $minutes } min). The next part follows: a recording stops at the longest length your organization allows.
earlier-lost = The recorder restarted during this call: the part of the call before that was not recorded.

## When it can't record
lost-at-restart = The recorder restarted while it recorded a call in this conversation: that recording is lost.
disk-full = The recorder could not record this call: the disk it writes to is almost full. Tell your administrator.
write-failed = The recorder stopped recording this call: it could not write the recording.
join-failed = The recorder could not join this call: the apps in the call did not take its keys. Whoever is in the call with an older app needs to join it again or update the app.
notice-unconfirmed = The recorder stopped recording this call: the server did not confirm that everybody in the call is told about the recording.
removed = A host removed the recorder from this call: the rest of the call is not recorded.
