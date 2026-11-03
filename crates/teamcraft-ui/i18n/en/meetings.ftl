# TeamCraft UI – English (source language)
# Meetings: scheduling, responses, host controls and guests.

## First-run key setup
# ---- Meetings ----
meetings-title = Meetings
meetings-start-now = Meet now
meetings-start-now-hint = A new meeting right away, with nobody invited yet. To start a planned meeting early, use Start on it.
meetings-start = Start
meetings-schedule = Schedule
meetings-empty = Schedule one for later or start one right now.
meetings-today = Today
meetings-tomorrow = Tomorrow
meetings-join = Join
meetings-edit = Edit meeting
meetings-cancel = Cancel meeting
meetings-cancelled = Cancelled
meetings-restore = Restore
meetings-remove = Remove from list
meetings-cancel-title = Cancel this meeting?
meetings-cancel-title-series = Which meetings do you want to cancel?
meetings-cancel-occurrence = Only this meeting
meetings-cancel-following = This and all following meetings
meetings-cancel-series = All meetings of the series
meetings-cancel-hint = Everyone invited is told. Until it would have taken place, a cancelled meeting stays in the list, where you can restore it.
meetings-cancel-keep = Keep
meetings-cancel-confirm = Cancel meeting
meetings-recurring = Repeats
meetings-organizer = Organized by { $name }
meetings-rsvp = Your answer
meetings-rsvp-accepted = Yes
meetings-rsvp-tentative = Maybe
meetings-rsvp-declined = No
meetings-guest-link = Copy a one-time link
meetings-guest-link-hint = Personal link, works once. Send it to one guest.
meetings-link = Meeting link
meetings-copy-link = Copy link
meetings-link-copied = Copied
meetings-link-hint = For everybody, also guests without an account. Anyone who isn't in the meeting waits until a host lets them in. The link holds no key: guests get theirs when they are let in.
meetings-invite-guests = Invite guests
meetings-one-time-link-label = One-time link
meetings-new-link = Make a new link
meetings-new-link-hint = The current link stops working at once. Invitees get the new one by e-mail.
meetings-link-renewed = New link made: the old one no longer works.
meetings-created-title = Your meeting is ready
meetings-created-hint = Send the link to the people you want to meet.
meetings-visit-opening = Opening the meeting…
meetings-visit-waiting-title = Waiting for a host
meetings-visit-waiting = You're not in this meeting yet. Its hosts have been asked to let you in; keep this page open.
meetings-visit-cancel = Stop waiting
meetings-visit-denied = A host didn't let you into this meeting.
meetings-visit-ended = This meeting is over or was cancelled.
meetings-visit-failed = The meeting couldn't be opened. Check the link or try again.
meetings-lobby = Waiting to join
meetings-admit = Let in
meetings-guest-device-pending = Waiting for the guest's device to set up encryption
meetings-deny = Turn away
meetings-error = Meetings could not be updated: { $detail }
schedule-title = Schedule a meeting
schedule-name = Title
schedule-start = Starts
schedule-duration = Length
schedule-minutes = { $count } min
schedule-description = Description (optional)
schedule-invite-members = Invite people
schedule-invite-external = Invite by email
schedule-external-email = Email address
schedule-external-name = Name (optional)
schedule-add = Add
schedule-remove = Remove
schedule-repeat = Repeat
schedule-repeat-never = Does not repeat
schedule-repeat-daily = Daily
schedule-repeat-weekly = Weekly
schedule-repeat-monthly = Monthly
schedule-every = Every
schedule-unit-never = { "" }
schedule-unit-daily = { $count ->
    [one] day
   *[other] days
}
schedule-unit-weekly = { $count ->
    [one] week
   *[other] weeks
}
schedule-unit-monthly = { $count ->
    [one] month
   *[other] months
}
schedule-unit-meetings = { $count ->
    [one] meeting
   *[other] meetings
}
schedule-on-days = On
schedule-ends = Ends
schedule-ends-never = Never
schedule-ends-count = After
schedule-ends-date = On
schedule-lobby = Guests wait in a lobby until a host lets them in
schedule-lobby-hint = Turn this off only for meetings where anyone with a link may join straight away.
schedule-notetaker = Notetaker joins the calls
schedule-notetaker-hint = It makes a transcript of the meeting's calls and posts a summary in its chat. For this, the audio of those calls is sent to OpenRouter; everyone in the call sees that it is there.
schedule-edit-title = Edit meeting
schedule-save = Save
schedule-submit = Schedule
meeting-error-title = Give the meeting a title.
meeting-error-start = Pick a start date and time.
meeting-error-past = The start is in the past.
meeting-error-duration = Pick a length between a minute and a day.
meeting-error-interval = Enter a number from 1 to 100.
meeting-error-no-weekday = Pick at least one day.
meeting-error-start-weekday = The first meeting is on another day: pick its day too, or move the start.
meeting-error-count = Enter how many meetings, from 1 to 1000.
meeting-error-until = Pick the date of the last meeting.
meeting-error-until-before = The last date is before the first meeting.
meeting-error-email = "{ $email }" is not a valid email address.
guest-title = Join the meeting
guest-subtitle = Enter your name so the hosts know who is joining. You do not need an account.
guest-name = Your name
guest-join = Ask to join
guest-sign-in = I have an account: sign in
guest-waiting-title = Waiting for the organizer
guest-waiting = The organizer lets you in once they are here. If nobody has joined yet, you wait here until the meeting begins. Keep this page open.
guest-admitted-title = You are in
guest-admitted = A host let you into the meeting.
guest-denied-title = Not admitted
guest-denied = The host did not let you in.
guest-failed-title = This link does not work
guest-link-unusable = The link was already used, has expired or was revoked, or the meeting was cancelled. Ask the organizer for a new one.
guest-network = The server could not be reached. Check your connection and reload the page.
guest-waiting-encryption = Your device is setting up encryption while you wait. Everybody in the meeting sees you as an unverified guest.
guest-setup-failed = Your browser could not set up encryption for this meeting. Reload the page and try the link again, or ask for a new one.
guest-securing = Opening the meeting…
guest-join-call = Join the call
guest-leave = Leave the meeting
guest-left-title = You left the meeting
guest-left = Your session ended and this browser forgot the meeting's keys. Close the page.

# Meetings: responses and host controls
meetings-responses = Responses ({ $count })
meetings-rsvp-awaiting = No answer yet
meetings-rsvp-summary = { $accepted } accepted · { $tentative } maybe · { $declined } declined · { $awaiting } waiting
host-controls = Host controls
host-mute-all = Mute everyone
host-lock = Lock meeting
host-unlock = Unlock meeting
host-end = End for everyone
host-end-confirm = End the meeting now
host-remove = Remove

## Settings: sections, search and rows (#327)
# Home pages (issue #585)
meetings-live = Now
meetings-empty-title = No meetings coming up
guest-own-code = Your code
guest-organizer-code = The organizer's code
guest-codes-hint = Whoever wants to be sure can compare: the organizer sees your code next to your name, and their app shows their own code. If one differs, somebody is in between: leave the meeting.
guest-organizer-app-outdated = The organizer's app needs an update to let you in. Let them know, then keep this page open.
meetings-verifying = Verifying…
meetings-guest-code = Code
meetings-guest-code-hint = The guest sees the same code on their screen. Compare it to be sure nobody is in between.
meetings-organizer-code = Your code for guests:
meetings-joined-by-link = { $name } joined with the meeting link
meetings-remove-joined = Remove
