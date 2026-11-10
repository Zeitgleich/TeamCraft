# TeamCraft UI – English (source language)
# People: profiles, presence and status, the directory and user groups.

## Top bar and account menu
status-menu-label = Account and status
presence-online = Online
presence-away = Away
presence-dnd = Do not disturb
presence-offline = Offline
presence-busy = Busy
presence-brb = Be right back
presence-dnd-for-hour = For 1 hour
presence-dnd-until-morning = Until tomorrow 08:00
presence-dnd-until = Until { $time }
status-pick-label = Status
status-pick-online = Available
status-pick-busy = Busy
status-pick-dnd = Do not disturb
status-pick-brb = Be right back
status-pick-away = Appear away
status-pick-offline = Appear offline
status-reset = Reset status
status-duration = Duration…
status-duration-title = Status and duration
status-duration-reset-after = Reset after
status-duration-30m = 30 minutes
status-duration-1h = 1 hour
status-duration-2h = 2 hours
status-duration-today = Today
status-duration-week = This week
status-duration-until = Until a time…
status-duration-never = Never
status-duration-until-label = Reset at
status-duration-until-past = Pick a time in the future.
status-duration-dnd-hint = Only urgent messages and people you allowed reach you.
status-duration-set = Set status
status-custom-set = Set a custom status
status-custom-title = Set a custom status
status-emoji = Status emoji
status-text = Status text
status-text-placeholder = What's your status?
status-presets = Suggested statuses
status-clear-after = Clear after
status-clear = Clear status
status-expiry-never = Don't clear
status-expiry-30m = 30 minutes
status-expiry-1h = 1 hour
status-expiry-today = Today
status-expiry-week = This week
status-preset-meeting = In a meeting
status-preset-commuting = Commuting
status-preset-sick = Out sick
status-preset-vacation = On vacation
status-until = until { $time }
status-until-today = until end of today
status-presets-only = Your workspace offers these statuses to choose from.

## Administration
user-deleted-name = Deleted user

## First-run key setup
# ---- Meetings ----
presence-in-call = In a call

# Profiles, people and groups (v1 REST UI)
profile-edit-title = Edit profile
profile-unavailable = Profiles are not available here.
profile-title = Job title
profile-department = Department
profile-phone = Phone
profile-time-zone = Time zone
profile-local-time = Local time
profile-hours = Working hours
profile-hours-days = Working days
profile-hours-from = From
profile-hours-to = To
profile-hours-invalid = Working hours need at least one day and an end after the start.
profile-avatar-change = Change picture
profile-avatar-remove = Remove picture
profile-message = Message
profile-message-guest-blocked = As a guest you can answer messages but not start conversations. A member can write to you first.
profile-open = Show the profile of { $name }
profile-call = Call
profile-email = E-mail
profile-copy-email = Copy e-mail
profile-email-copied = Copied
profile-shared = In common
profile-shared-spaces = Shared spaces
profile-shared-channels = Shared channels
profile-shared-more = and { $count } more channels
directory-title = People
directory-people = People
directory-groups = Groups
directory-filter = Find a person
directory-empty = Try a different name, title or department.
directory-no-devices = hasn't finished setting up TeamCraft yet
directory-not-signed-in = hasn't signed in yet
directory-organization = Organization
directory-all-organizations = All organizations
groups-unavailable = User groups need a signed-in session.
groups-members = { $count ->
    [one] 1 member
   *[other] { $count } members
}
groups-synced = From the identity provider
groups-empty = Groups bundle people, so you can mention them together with one handle.
groups-loading = Loading groups…
groups-handle = Handle (what follows @)
groups-name = Name
groups-add = Add
groups-remove = Remove

# Meetings: responses and host controls
presence-in-meeting = In a meeting

## Activity, last seen and call history
last-seen = Last seen { $when }
last-seen-just-now = Last seen just now
last-seen-short = last seen: { $when }
last-seen-short-just-now = last seen: now

## Daily-use improvements (docs/ux-backlog.md)
hover-card-local-time = { $time } local time
hover-card-profile = View profile

## Settings: sections, search and rows (#327)
# Home pages (issue #585)
directory-you = You
directory-empty-title = Nobody found
directory-space-empty = Nobody in this space matches. Try a different name, title or department.
directory-space-hidden = Only members of a space can see who is in it.
directory-loading = Looking for people
groups-empty-title = No user groups yet
groups-error-handle = Use 2 to 32 lower-case letters, digits, - or _.
groups-error-handle-taken = This handle is already used by a group or a person.

directory-channels = Channels
directory-external = External
directory-invitations = Invitations
directory-department = Department
directory-all-departments = All departments
directory-job-title = Title
directory-all-titles = All titles
directory-show-more = Show more
directory-narrow-down = That's as many as fit here. Search or filter to find the rest.
directory-offline-note = Can't reach the server. These are the people this device knows.
directory-external-empty = No guests match. Guests are people from outside who were invited to a space or channel.
directory-channel-filter = Find a channel
directory-channels-loading = Looking for channels
directory-channels-empty-title = No channels found
directory-channels-empty = Try a different name or topic. Private channels only show here once you're in them.
directory-channel-members = { $count ->
    [one] { $count } member
   *[other] { $count } members
}
directory-invitation-filter = Find an invitation by address
directory-invitations-loading = Looking for invitations
directory-invitations-empty-title = No open invitations
directory-invitations-empty = Invitations you can manage show here until the person signs in.
directory-invited-by = Invited by { $name }
directory-invitation-expires = Valid until { $date }
directory-invitation-expired = Expired
directory-resend = Send again
directory-resent = Sent again
directory-revoke = Revoke
directory-revoke-confirm = Revoke the invitation for { $email }? They won't join when they sign in.
directory-resend-later = Sent a short while ago. You can send it again later.
