# TeamCraft UI – Deutsch
# People: profiles, presence and status, the directory and user groups.

## Kopfleiste und Kontomenü
status-menu-label = Konto und Status
presence-online = Online
presence-away = Abwesend
presence-dnd = Nicht stören
presence-offline = Offline
presence-busy = Beschäftigt
presence-brb = Bin gleich zurück
presence-dnd-for-hour = Für 1 Stunde
presence-dnd-until-morning = Bis morgen 08:00
presence-dnd-until = Bis { $time }
status-pick-label = Status
status-pick-online = Verfügbar
status-pick-busy = Beschäftigt
status-pick-dnd = Nicht stören
status-pick-brb = Bin gleich zurück
status-pick-away = Als abwesend anzeigen
status-pick-offline = Als offline anzeigen
status-reset = Status zurücksetzen
status-duration = Dauer…
status-duration-title = Status und Dauer
status-duration-reset-after = Zurücksetzen nach
status-duration-30m = 30 Minuten
status-duration-1h = 1 Stunde
status-duration-2h = 2 Stunden
status-duration-today = Heute
status-duration-week = Diese Woche
status-duration-until = Bis zu einer Uhrzeit…
status-duration-never = Nie
status-duration-until-label = Zurücksetzen am
status-duration-until-past = Wähle einen Zeitpunkt in der Zukunft.
status-duration-dnd-hint = Nur dringende Nachrichten und Personen, die du zugelassen hast, erreichen dich.
status-duration-set = Status festlegen
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
directory-space-empty = Niemand in diesem Space passt. Versuche einen anderen Namen, Titel oder eine andere Abteilung.
directory-space-hidden = Nur Mitglieder eines Space sehen, wer darin ist.
directory-loading = Personen werden gesucht
groups-empty-title = Noch keine Benutzergruppen
groups-error-handle = Verwende 2 bis 32 Kleinbuchstaben, Ziffern, - oder _.
groups-error-handle-taken = Dieses Kürzel verwendet schon eine Gruppe oder eine Person.

directory-channels = Kanäle
directory-external = Extern
directory-invitations = Einladungen
directory-department = Abteilung
directory-all-departments = Alle Abteilungen
directory-job-title = Position
directory-all-titles = Alle Positionen
directory-show-more = Mehr anzeigen
directory-narrow-down = Mehr passen hier nicht hin. Suche oder filtere, um die übrigen zu finden.
directory-offline-note = Der Server ist nicht erreichbar. Das sind die Personen, die dieses Gerät kennt.
directory-external-empty = Keine passenden Gäste. Gäste sind Personen von außen, die in einen Space oder Kanal eingeladen wurden.
directory-channel-filter = Kanal suchen
directory-channels-loading = Kanäle werden gesucht
directory-channels-empty-title = Keine Kanäle gefunden
directory-channels-empty = Versuche einen anderen Namen oder ein anderes Thema. Private Kanäle erscheinen hier erst, wenn du darin bist.
directory-channel-members = { $count ->
    [one] { $count } Mitglied
   *[other] { $count } Mitglieder
}
directory-invitation-filter = Einladung nach Adresse suchen
directory-invitations-loading = Einladungen werden gesucht
directory-invitations-empty-title = Keine offenen Einladungen
directory-invitations-empty = Einladungen, die du verwalten kannst, stehen hier, bis sich die Person anmeldet.
directory-invited-by = Eingeladen von { $name }
directory-invitation-expires = Gültig bis { $date }
directory-invitation-expired = Abgelaufen
directory-resend = Erneut senden
directory-resent = Erneut gesendet
directory-revoke = Zurückziehen
directory-revoke-confirm = Einladung für { $email } zurückziehen? Die Person tritt bei der Anmeldung dann nicht bei.
directory-resend-later = Vor Kurzem gesendet. Du kannst sie später erneut senden.
