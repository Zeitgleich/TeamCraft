# TeamCraft UI – English (source language)
# People: profiles, presence and status, the directory and user groups.

## Top bar and account menu
status-menu-label = Account and status
presence-online = Online
presence-away = Away
presence-dnd = Do not disturb
presence-offline = Offline
presence-dnd-hint = Notifications are paused
presence-dnd-for-hour = For 1 hour
presence-dnd-until-morning = Until tomorrow 08:00
presence-dnd-custom = Until a time…
presence-dnd-custom-label = Do not disturb until
presence-dnd-custom-set = Set
presence-dnd-end = End do not disturb
presence-dnd-until = Until { $time }
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
groups-empty-title = No user groups yet
groups-error-handle = Use lower-case letters, digits, - or _, at least two.
groups-error-handle-taken = This handle is already used by a group or a person.
