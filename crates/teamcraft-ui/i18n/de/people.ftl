# TeamCraft UI – Deutsch
# People: profiles, presence and status, the directory and user groups.

## Kopfleiste und Kontomenü
status-menu-label = Konto und Status
presence-online = Online
presence-away = Abwesend
presence-dnd = Nicht stören
presence-offline = Offline
presence-dnd-hint = Benachrichtigungen sind pausiert
presence-dnd-for-hour = Für 1 Stunde
presence-dnd-until-morning = Bis morgen 08:00
presence-dnd-custom = Bis zu einer Uhrzeit…
presence-dnd-custom-label = „Nicht stören“ bis
presence-dnd-custom-set = Übernehmen
presence-dnd-end = „Nicht stören“ beenden
presence-dnd-until = Bis { $time }
status-custom-set = Eigenen Status festlegen
status-custom-title = Eigenen Status festlegen
status-emoji = Status-Emoji
status-text = Statustext
status-text-placeholder = Was machst du gerade?
status-presets = Vorgeschlagene Status
status-clear-after = Löschen nach
status-clear = Status löschen
status-expiry-never = Nicht löschen
status-expiry-30m = 30 Minuten
status-expiry-1h = 1 Stunde
status-expiry-today = Heute
status-expiry-week = Diese Woche
status-preset-meeting = In einem Meeting
status-preset-commuting = Unterwegs
status-preset-sick = Krank
status-preset-vacation = Im Urlaub
status-until = bis { $time }
status-until-today = bis Ende des Tages
status-presets-only = Dein Arbeitsbereich bietet diese Status zur Auswahl an.

## Administration
user-deleted-name = Gelöschter Nutzer

## Einrichtung der Schlüssel
# ---- Meetings ----
presence-in-call = In einem Anruf

# Profiles, people and groups (v1 REST UI)
profile-edit-title = Profil bearbeiten
profile-unavailable = Profile sind hier nicht verfügbar.
profile-title = Position
profile-department = Abteilung
profile-phone = Telefon
profile-time-zone = Zeitzone
profile-local-time = Ortszeit
profile-hours = Arbeitszeiten
profile-hours-days = Arbeitstage
profile-hours-from = Von
profile-hours-to = Bis
profile-hours-invalid = Arbeitszeiten brauchen mindestens einen Tag und ein Ende nach dem Beginn.
profile-avatar-change = Bild ändern
profile-avatar-remove = Bild entfernen
profile-message = Nachricht
profile-message-guest-blocked = Als Gast kannst du Nachrichten beantworten, aber keine Unterhaltungen beginnen. Eine Person aus dem Team kann dir zuerst schreiben.
profile-open = Profil von { $name } anzeigen
profile-call = Anrufen
profile-email = E-Mail
profile-copy-email = E-Mail kopieren
profile-email-copied = Kopiert
profile-shared = Gemeinsam
profile-shared-spaces = Gemeinsame Spaces
profile-shared-channels = Gemeinsame Channels
profile-shared-more = und { $count } weitere Channels
directory-title = Personen
directory-people = Personen
directory-groups = Gruppen
directory-filter = Person suchen
directory-empty = Versuche einen anderen Namen, Titel oder eine andere Abteilung.
directory-no-devices = hat TeamCraft noch nicht fertig eingerichtet
directory-organization = Organisation
directory-all-organizations = Alle Organisationen
groups-unavailable = Benutzergruppen brauchen eine angemeldete Sitzung.
groups-members = { $count ->
    [one] 1 Mitglied
   *[other] { $count } Mitglieder
}
groups-synced = Vom Identitätsanbieter
groups-empty = Gruppen bündeln Personen, damit du sie gemeinsam mit einem Handle erwähnen kannst.
groups-loading = Gruppen werden geladen…
groups-handle = Kürzel (was auf @ folgt)
groups-name = Name
groups-add = Hinzufügen
groups-remove = Entfernen

# Meetings: responses and host controls
presence-in-meeting = In einem Meeting

## Aktivität, zuletzt online und Anrufliste
last-seen = Zuletzt online { $when }
last-seen-just-now = Zuletzt online gerade eben
last-seen-short = zul. Online: { $when }
last-seen-short-just-now = zul. Online: jetzt

## Daily-use improvements (docs/ux-backlog.md)
hover-card-local-time = { $time } Ortszeit
hover-card-profile = Profil anzeigen

## Settings: sections, search and rows (#327)
# Startseiten (Issue #585)
directory-you = Du
directory-empty-title = Niemand gefunden
groups-empty-title = Noch keine Benutzergruppen
