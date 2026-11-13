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
meetings-holiday = Feiertag
meetings-restore-holiday = Trotzdem treffen
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
meetings-link-hint = Für alle, auch für Gäste ohne Konto. Wer nicht im Meeting ist, wartet, bis ein Gastgeber ihn einlässt. Der Link enthält keinen Schlüssel: Gäste bekommen ihren beim Einlassen.
meetings-invite-guests = Gäste einladen
meetings-one-time-link-label = Einmal-Link
meetings-new-link = Neuen Link erstellen
meetings-new-link-hint = Der bisherige Link funktioniert sofort nicht mehr. Eingeladene bekommen den neuen per E-Mail.
meetings-link-renewed = Neuer Link erstellt: Der alte funktioniert nicht mehr.
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
meetings-member-unconfirmed = Noch nicht als Mitglied bestätigt. Lade die Person stattdessen zur Besprechung ein.
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
schedule-skip-holidays = Feiertage überspringen
schedule-skip-holidays-hint = Meetings an Feiertagen deiner Region (Einstellungen, Konto) oder sonst der, die deine Admins eingestellt haben, fallen aus. Einzelne kannst du trotzdem stattfinden lassen.
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
schedule-co-hosts = Co-Gastgeber
schedule-co-hosts-hint = Co-Gastgeber lassen Personen ein, erstellen Gastlinks und nutzen die Host-Steuerung. Nur du änderst das Meeting oder sagst es ab.
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
guest-sign-in = Ich habe ein Konto: anmelden
guest-waiting-title = Warten, bis dich jemand aus dem Meeting einlässt
guest-waiting = Jemand aus dem Meeting lässt dich ein, sobald sie oder er da ist. Ist noch niemand im Meeting, wartest du hier, bis es beginnt. Lass diese Seite geöffnet.
guest-admitted-title = Du bist drin
guest-admitted = Ein Gastgeber hat dich ins Meeting gelassen.
guest-denied-title = Nicht eingelassen
guest-denied = Der Gastgeber hat dich nicht eingelassen.
guest-failed-title = Dieser Link funktioniert nicht
guest-link-unusable = Der Link wurde schon benutzt, ist abgelaufen oder wurde widerrufen, oder das Meeting wurde abgesagt. Bitte den Organisator um einen neuen.
guest-network = Der Server ist nicht erreichbar. Prüfe deine Verbindung und lade die Seite neu.
guest-waiting-encryption = Dein Gerät richtet währenddessen die Verschlüsselung ein. Alle im Meeting sehen dich als nicht verifizierten Gast.
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
# Co-Gastgeber und Übergabe eines Meetings (#1720)
host-role-organizer = Gastgeber
host-role-co-host = Co-Gastgeber
host-make-co-host = Zum Co-Gastgeber machen
host-remove-co-host = Als Co-Gastgeber entfernen
host-hand-over = Gastgeberrolle übergeben
host-hand-over-confirm = Das Meeting an { $name } übergeben? Ab dann ändert nur noch diese Person es oder sagt es ab; du bleibst Co-Gastgeber.
host-hand-over-do = Übergeben
host-hosts-saving = Gastgeber werden gespeichert …
host-hosts-failed = Die Gastgeber konnten nicht geändert werden: { $detail }
host-leave-title = Es warten Personen auf Einlass
host-leave-body = { $count ->
    [one] Eine Person wartet in der Lobby, und sobald du gehst, kann sie hier niemand mehr einlassen.
   *[other] { $count } Personen warten in der Lobby, und sobald du gehst, kann sie hier niemand mehr einlassen.
}
host-leave-hand-over = An { $name } übergeben und gehen
host-leave-anyway = Trotzdem gehen
meetings-now-co-host = Du bist jetzt Co-Gastgeber von „{ $title }“.
meetings-now-organizer = „{ $title }“ wurde an dich übergeben: Du bist jetzt der Gastgeber.

## Settings: sections, search and rows (#327)
# Startseiten (Issue #585)
meetings-live = Läuft
meetings-empty-title = Keine Meetings geplant
meetings-space-empty = Meetings, die in diesem Space geplant werden, erscheinen hier.
guest-own-code = Dein Code
guest-organizer-code = Code der Organisatorin oder des Organisators
guest-codes-hint = Wer sichergehen will, vergleicht: Die Organisatorin oder der Organisator sieht deinen Code neben deinem Namen, ihre oder seine App zeigt den eigenen Code. Weicht einer ab, ist jemand dazwischen: Verlasse das Meeting.
guest-organizer-app-outdated = Die App der Organisatorin oder des Organisators braucht ein Update, um dich einzulassen. Sag Bescheid und lass diese Seite geöffnet.
meetings-verifying = Wird geprüft …
meetings-guest-code = Code
meetings-guest-code-hint = Der Gast sieht denselben Code auf seinem Bildschirm. Vergleiche ihn, um sicherzugehen, dass niemand dazwischen ist.
meetings-organizer-code = Dein Code für Gäste:
meetings-joined-by-link = { $name } ist mit dem Meeting-Link beigetreten
meetings-remove-joined = Entfernen
meetings-visit-from-outside = Ein Link möchte eine Besprechung öffnen. Bist du noch nicht dabei, bittet das Öffnen die Hosts, dich einzulassen.
meetings-visit-open = Besprechung öffnen

# Meetings-Seite: Ansichten, Jetzt / Als Nächstes, Antworten, Zeilen (#956)
meetings-views = Meetings anzeigen
meetings-tab-upcoming = Anstehend
meetings-tab-hosted = Von mir
meetings-tab-invited = Eingeladen
meetings-tab-past = Vergangen
meetings-hosted-empty-title = Nichts, was du leitest
meetings-hosted-empty = Meetings, die du organisierst oder mitleitest, erscheinen hier.
meetings-invited-empty-title = Keine Einladungen
meetings-invited-empty = Meetings, zu denen andere dich einladen, erscheinen hier.
meetings-past-empty-title = Keine vergangenen Meetings
meetings-past-empty = Meetings der letzten 30 Tage erscheinen hier, sobald sie vorbei sind.
meetings-show-more = Mehr anzeigen
meetings-reach = Meetings bis { $date }
meetings-loading = Meetings werden geladen …
meetings-hero-now = Jetzt
meetings-hero-next = Als Nächstes
meetings-hero-running = Läuft · { $time }
meetings-hero-soon = { $count ->
    [one] Beginnt in 1 Minute · { $time }
   *[other] Beginnt in { $count } Minuten · { $time }
}
meetings-details = Details
meetings-more = Weitere Aktionen
meetings-needs-answer = Wartet auf deine Antwort
meetings-answer-covers = { $count ->
    [one] Deine Antwort gilt auch für 1 weiteren Termin der Serie
   *[other] Deine Antwort gilt auch für { $count } weitere Termine der Serie
}
meetings-series-daily = { $count ->
    [one] Jeden Tag
   *[other] Alle { $count } Tage
}
meetings-series-workdays = { $count ->
    [one] Jeden Werktag
   *[other] Alle { $count } Tage an Werktagen
}
meetings-series-weekly = { $count ->
    [one] Jede Woche
   *[other] Alle { $count } Wochen
}
meetings-series-monthly = { $count ->
    [one] Jeden Monat
   *[other] Alle { $count } Monate
}
meetings-state-soon = { $count ->
    [one] In 1 Min.
   *[other] In { $count } Min.
}
meetings-state-ended = Beendet
meetings-state-waiting = { $count ->
    [one] 1 wartet
   *[other] { $count } warten
}
meetings-rsvp-state-accepted = Zugesagt
meetings-rsvp-state-tentative = Vielleicht
meetings-rsvp-state-declined = Abgelehnt
meetings-rsvp-state-none = Antworten
meetings-link-copied-toast = Meeting-Link kopiert
meetings-you-host = Du leitest
meetings-people = { $count ->
    [one] 1 Person
   *[other] { $count } Personen
}
meetings-duration-minutes = { $minutes } Min.
meetings-duration-hours = { $hours } Std.
meetings-duration-both = { $hours } Std. { $minutes } Min.
meetings-yesterday = Gestern
meetings-when = Wann
