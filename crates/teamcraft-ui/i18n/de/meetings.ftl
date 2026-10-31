# TeamCraft UI – Deutsch
# Meetings: scheduling, responses, host controls and guests.

## Einrichtung der Schlüssel
# ---- Meetings ----
meetings-title = Meetings
meetings-start-now = Sofort-Meeting
meetings-start-now-hint = Ein neues Meeting sofort, noch ohne Eingeladene. Ein geplantes Meeting startest du früher mit „Starten“ an diesem Meeting.
meetings-start = Starten
meetings-schedule = Planen
meetings-empty = Plane eines für später oder starte gleich eins.
meetings-today = Heute
meetings-tomorrow = Morgen
meetings-join = Beitreten
meetings-edit = Meeting bearbeiten
meetings-cancel = Meeting absagen
meetings-cancelled = Abgesagt
meetings-restore = Wiederherstellen
meetings-remove = Aus der Liste entfernen
meetings-cancel-title = Dieses Meeting absagen?
meetings-cancel-title-series = Welche Meetings möchtest du absagen?
meetings-cancel-occurrence = Nur dieses Meeting
meetings-cancel-following = Dieses und alle folgenden Meetings
meetings-cancel-series = Alle Meetings der Serie
meetings-cancel-hint = Alle Eingeladenen werden benachrichtigt. Bis zu seinem Termin bleibt ein abgesagtes Meeting in der Liste; dort kannst du es wiederherstellen.
meetings-cancel-keep = Behalten
meetings-cancel-confirm = Meeting absagen
meetings-recurring = Wiederholt sich
meetings-organizer = Organisiert von { $name }
meetings-rsvp = Deine Antwort
meetings-rsvp-accepted = Ja
meetings-rsvp-tentative = Vielleicht
meetings-rsvp-declined = Nein
meetings-guest-link = Einmal-Link kopieren
meetings-guest-link-hint = Persönlicher Link, funktioniert einmal. Schicke ihn einem Gast.
meetings-link = Meeting-Link
meetings-copy-link = Link kopieren
meetings-link-copied = Kopiert
meetings-link-hint = Für Personen in deiner Organisation. Wer nicht eingeladen ist, wartet, bis ein Gastgeber ihn einlässt.
meetings-invite-guests = Gäste einladen
meetings-shared-guest-link = Gästelink kopieren
meetings-shared-guest-link-label = Gästelink
meetings-shared-guest-link-hint = Für mehrere Personen ohne Konto. Alle, die ihn öffnen, warten in der Lobby; der Link endet mit dem Meeting. Jeder, der ihn hat, kann ihn nutzen; ein weitergegebener Link kann alle Gastplätze aufbrauchen. Widerrufe ihn dann.
meetings-one-time-link-label = Einmal-Link
meetings-revoke-guest-link = Gästelink widerrufen
meetings-guest-link-revoked = Gästelink widerrufen: Damit kommt niemand Neues mehr herein.
meetings-created-title = Dein Meeting ist bereit
meetings-created-hint = Schicke den Link an alle, mit denen du dich treffen willst.
meetings-visit-opening = Meeting wird geöffnet …
meetings-visit-waiting-title = Warten auf einen Gastgeber
meetings-visit-waiting = Du bist noch nicht in diesem Meeting. Die Gastgeber wurden gebeten, dich einzulassen; lass diese Seite geöffnet.
meetings-visit-cancel = Nicht mehr warten
meetings-visit-denied = Ein Gastgeber hat dich nicht in dieses Meeting gelassen.
meetings-visit-ended = Dieses Meeting ist vorbei oder wurde abgesagt.
meetings-visit-failed = Das Meeting konnte nicht geöffnet werden. Prüfe den Link oder versuche es erneut.
meetings-lobby = Wartet auf Einlass
meetings-admit = Einlassen
meetings-guest-device-pending = Das Gerät des Gasts richtet gerade die Verschlüsselung ein
meetings-deny = Abweisen
meetings-error = Meetings konnten nicht aktualisiert werden: { $detail }
schedule-title = Meeting planen
schedule-name = Titel
schedule-start = Beginn
schedule-duration = Dauer
schedule-minutes = { $count } Min.
schedule-description = Beschreibung (optional)
schedule-invite-members = Personen einladen
schedule-invite-external = Per E-Mail einladen
schedule-external-email = E-Mail-Adresse
schedule-external-name = Name (optional)
schedule-add = Hinzufügen
schedule-remove = Entfernen
schedule-repeat = Wiederholung
schedule-repeat-never = Keine Wiederholung
schedule-repeat-daily = Täglich
schedule-repeat-weekly = Wöchentlich
schedule-repeat-monthly = Monatlich
schedule-every = Alle
schedule-unit-never = { "" }
schedule-unit-daily = { $count ->
    [one] Tag
   *[other] Tage
}
schedule-unit-weekly = { $count ->
    [one] Woche
   *[other] Wochen
}
schedule-unit-monthly = { $count ->
    [one] Monat
   *[other] Monate
}
schedule-unit-meetings = { $count ->
    [one] Meeting
   *[other] Meetings
}
schedule-on-days = An
schedule-ends = Endet
schedule-ends-never = Nie
schedule-ends-count = Nach
schedule-ends-date = Am
schedule-lobby = Gäste warten in einer Lobby, bis ein Gastgeber sie einlässt
schedule-lobby-hint = Schalte das nur für Meetings aus, bei denen jeder mit Link sofort beitreten darf.
schedule-notetaker = Notetaker nimmt an den Anrufen teil
schedule-notetaker-hint = Er erstellt Transkripte der Anrufe dieses Meetings und schreibt eine Zusammenfassung in den Chat. Dazu wird der Ton dieser Anrufe an OpenRouter gesendet; alle im Anruf sehen, dass er dabei ist.
schedule-edit-title = Meeting bearbeiten
schedule-save = Speichern
schedule-submit = Planen
meeting-error-title = Gib dem Meeting einen Titel.
meeting-error-start = Wähle Datum und Uhrzeit des Beginns.
meeting-error-past = Der Beginn liegt in der Vergangenheit.
meeting-error-duration = Wähle eine Dauer zwischen einer Minute und einem Tag.
meeting-error-interval = Gib eine Zahl von 1 bis 100 ein.
meeting-error-no-weekday = Wähle mindestens einen Tag.
meeting-error-start-weekday = Das erste Meeting liegt an einem anderen Tag: Wähle diesen Tag auch, oder verschiebe den Beginn.
meeting-error-count = Gib die Anzahl der Meetings ein, von 1 bis 1000.
meeting-error-until = Wähle das Datum des letzten Meetings.
meeting-error-until-before = Das letzte Datum liegt vor dem ersten Meeting.
meeting-error-email = „{ $email }" ist keine gültige E-Mail-Adresse.
guest-title = Dem Meeting beitreten
guest-subtitle = Gib deinen Namen ein, damit die Gastgeber wissen, wer beitritt. Du brauchst kein Konto.
guest-name = Dein Name
guest-join = Beitritt anfragen
guest-waiting-title = Warten auf den Gastgeber
guest-waiting = Ein Gastgeber lässt dich gleich ein. Lass diese Seite geöffnet.
guest-admitted-title = Du bist drin
guest-admitted = Ein Gastgeber hat dich ins Meeting gelassen.
guest-denied-title = Nicht eingelassen
guest-denied = Der Gastgeber hat dich nicht eingelassen.
guest-failed-title = Dieser Link funktioniert nicht
guest-link-unusable = Der Link wurde schon benutzt, ist abgelaufen oder wurde widerrufen, oder das Meeting wurde abgesagt. Bitte den Organisator um einen neuen.
guest-network = Der Server ist nicht erreichbar. Prüfe deine Verbindung und lade die Seite neu.
guest-waiting-encryption = Dein Gerät richtet währenddessen die Verschlüsselung ein. Alle im Meeting sehen dich als nicht verifizierten Gast.
guest-link-incomplete = In diesem Link fehlt der Schlüssel der Organisatorin oder des Organisators, deshalb konnte der Raum nicht geprüft werden. Bitte um einen neuen Link aus einer aktuellen App.
guest-setup-failed = Dein Browser konnte die Verschlüsselung für dieses Meeting nicht einrichten. Lade die Seite neu und öffne den Link noch einmal, oder bitte um einen neuen.
guest-securing = Das Meeting wird geöffnet …
guest-join-call = Dem Anruf beitreten
guest-leave = Meeting verlassen
guest-left-title = Du hast das Meeting verlassen
guest-left = Deine Sitzung ist beendet und dieser Browser hat die Schlüssel des Meetings vergessen. Schließe die Seite.

# Meetings: responses and host controls
meetings-responses = Antworten ({ $count })
meetings-rsvp-awaiting = Noch keine Antwort
meetings-rsvp-summary = { $accepted } zugesagt · { $tentative } vielleicht · { $declined } abgesagt · { $awaiting } ausstehend
host-controls = Host-Steuerung
host-mute-all = Alle stummschalten
host-lock = Meeting sperren
host-unlock = Meeting entsperren
host-end = Für alle beenden
host-end-confirm = Meeting jetzt beenden
host-remove = Entfernen

## Settings: sections, search and rows (#327)
# Startseiten (Issue #585)
meetings-live = Läuft
meetings-empty-title = Keine Meetings geplant
