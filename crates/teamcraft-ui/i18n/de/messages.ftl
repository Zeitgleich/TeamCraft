# TeamCraft UI – Deutsch
# Messages, threads and the composer.

## Kompaktes Layout (Telefone): untere Navigation, zurück, Menüs; Anrufe im Browser
composer-camera = Foto oder Video aufnehmen
composer-photos = Fotos und Videos

## Kanal
messages-label = Nachrichten
jump-to-latest = Zur neuesten springen

## Eingabefeld
composer-placeholder = Nachricht an { $name }
composer-placeholder-untitled = Nachricht schreiben
composer-placeholder-reply = Antworten …
composer-readonly = In diesem Kanal kannst du nichts schreiben.
composer-label = Nachricht
composer-attachments = Anhänge
composer-remove-attachment = Anhang entfernen
composer-attach = Dateien anhängen
composer-emoji = Emoji hinzufügen
composer-send = Nachricht senden
composer-hint-enter = Enter zum Senden, { $newline } für eine neue Zeile
composer-hint-ctrl-enter = { $send } zum Senden, Enter für eine neue Zeile

## Nachrichten
message-edited = (bearbeitet)
message-actions = Nachrichtenaktionen
action-react = Reaktion hinzufügen
action-reply = Im Thread antworten
reaction-label = Reaktion { $emoji }, { $count } Personen

## Threads
thread-replies = { $count ->
    [one] { $count } Antwort
   *[other] { $count } Antworten
  }
thread-new = { $count ->
    [one] { $count } neu
   *[other] { $count } neu
  }
thread-panel-label = Thread
thread-title = Thread
thread-follow = Folgen
thread-following = Folge ich
thread-messages-label = Nachrichten im Thread
thread-last-reply = letzte Antwort { $time }
date-jump-label = Zu einem Tag springen
date-jump-open = { $date }, zu einem Tag springen
date-jump-today = Heute
date-jump-yesterday = Gestern
date-jump-last-week = Vor einer Woche
date-jump-pick = Tag wählen
thread-replies-divider = Antworten

## Einrichtung der Schlüssel
message-seen-by = Gesehen von { $names }
typing-one = { $name } schreibt …
typing-two = { $first } und { $second } schreiben …
typing-several = Mehrere Personen schreiben …
# In der Chatliste und unter dem Namen einer Direktnachricht, solange die andere Person schreibt.
typing-direct = schreibt …

# ---- Meetings ----
composer-confirm-channel = Diese Nachricht benachrichtigt alle in dieser Unterhaltung ({ $count } Personen). Trotzdem senden?
composer-confirm-send = Trotzdem senden
jump-to-date = Zu einem Datum springen

# Message actions
message-more = Weitere Aktionen
message-history = Frühere Versionen
message-deleted = Diese Nachricht wurde gelöscht
message-placeholder-waiting = Warte auf diese Nachricht
message-placeholder-not-available = Auf diesem Gerät nicht verfügbar
message-placeholder-failed = Diese Nachricht konnte nicht entschlüsselt werden
action-quote = Zitat-Antwort
action-forward = Weiterleiten
action-pin = An Unterhaltung anheften
action-unpin = Lösen
action-save = Für später speichern
action-unsave = Aus Gespeichertem entfernen
link-menu-label = Link
link-menu-open = Im Browser öffnen
action-copy-link = Link kopieren
action-mark-unread = Als ungelesen markieren
action-edit = Bearbeiten
action-delete = Löschen
quote-unavailable = Originalnachricht nicht verfügbar
quote-replying-to = Antwort an { $name }
quote-cancel = Antwort abbrechen
forwarded-from = Weitergeleitet von { $name } { $conversation }
priority-important = Wichtig
priority-urgent = Dringend
ack-confirm = Als gelesen bestätigen
ack-done = Bestätigt
forward-title = Nachricht weiterleiten
forward-filter = Unterhaltung suchen
forward-comment = Kommentar hinzufügen (optional)
forward-send = Weiterleiten
pins-title = Angeheftete Nachrichten
pins-empty = Hefte eine Nachricht über ihr Menü an, damit sie hier für alle bleibt.
pins-empty-title = Keine angehefteten Nachrichten

# Files and link previews
link-open-new = Link in neuem Tab öffnen
composer-file-too-large = { $name } überschreitet das Upload-Limit von { $limit }.
composer-file-unreadable = { $name } konnte nicht gelesen werden.
composer-file-unstaged = { $name } konnte nicht zum Senden vorbereitet werden.
permalink-label = Nachricht in { $name }
permalink-unavailable = Link zu einer Unterhaltung, in der du nicht Mitglied bist
permalink-hidden = Nachrichtenlink

# Composer: mentions, slash commands, formatting
mention-channel = Alle in dieser Unterhaltung benachrichtigen
composer-suggestions = Vorschläge
composer-format = Formatierung
composer-format-toggle = Formatierung zeigen
format-bold = Fett
format-italic = Kursiv
format-strike = Durchgestrichen
format-code = Code
format-code-block = Codeblock
format-underline = Unterstrichen
format-style = Textstil
format-paragraph = Normaler Text
format-heading-1 = Überschrift 1
format-heading-2 = Überschrift 2
format-heading-3 = Überschrift 3
format-link = Link
format-numbered-list = Nummerierte Liste
format-bulleted-list = Aufzählung
format-quote = Zitat
composer-link-text = Text
composer-link-url = Linkadresse
composer-link-invalid = Gib eine Adresse ein, die mit http:// oder https:// beginnt.
composer-attach-file = Datei hochladen
composer-attach-media = Foto oder Video
composer-attach-camera = Foto aufnehmen
composer-mention = Jemanden erwähnen
composer-commands = Befehle
composer-send-options = Sendeoptionen
composer-priority = Priorität
priority-normal = Normal
composer-silent = Ohne Benachrichtigung senden
composer-silent-detail = Kommt leise an, ohne Ton und Push.
composer-ack = Lesebestätigung anfordern
composer-ack-detail = Alle können bestätigen, dass sie es gelesen haben.
composer-flag-silent = Lautlos
composer-flag-ack = Lesebestätigung
slash-missing-argument = Aufruf: { $usage }
slash-bad-reminder = Sag wann: „in 10m“, „in 2h“, „in 1d“ oder „at 14:30“, dann der Text.
slash-help-title = Befehle (mit // sendest du eine Nachricht, die mit einem Schrägstrich beginnt):
slash-not-found = Nichts gefunden für „{ $name }“.
slash-status-set = Status gesetzt.
slash-status-cleared = Status gelöscht.
slash-status-presets-only = Dein Arbeitsbereich erlaubt nur seine vorgegebenen Status. Wähle einen im Kontomenü.
slash-status-disabled = Status sind in diesem Arbeitsbereich ausgeschaltet.
slash-kicked = { $name } wurde aus dieser Unterhaltung entfernt.
slash-invited = { $name } wurde zu dieser Unterhaltung hinzugefügt.
slash-msg-send-there = Unterhaltung geöffnet. Schreibe deine Nachricht dort.
slash-groupmsg-unsupported = Gruppenunterhaltungen sind noch nicht verfügbar.
slash-muted = Unterhaltung stummgeschaltet.
slash-unmuted = Stummschaltung aufgehoben.
slash-channel-only = Dieser Befehl funktioniert nur in Kanälen.
slash-not-allowed = Dazu fehlt dir hier die Berechtigung.
slash-reminder-set = Erinnerung gesetzt für { $time }.
slash-meet-starting = Meeting wird gestartet …
slash-meet-unavailable = Meetings brauchen eine angemeldete Sitzung.
reminder-due = Erinnerung: { $text }

# Bookmarks
action-bookmark-file = Datei zu Lesezeichen hinzufügen
history-title = Bearbeitungsverlauf
history-current = Aktuelle Version
history-replaced = Ersetzt { $time }

## Daily-use improvements (docs/ux-backlog.md)
new-messages = Neue Nachrichten
jump-to-latest-new = { $count ->
    [one] 1 neue Nachricht
   *[other] { $count } neue Nachrichten
  }
action-react-with = Mit { $emoji } reagieren
emoji-picker-title = Emoji
emoji-search = Emoji suchen
emoji-frequent = Häufig verwendet
emoji-categories = Emoji-Kategorien
emoji-group-smileys = Smileys & Gefühle
emoji-group-people = Menschen & Körper
emoji-group-nature = Tiere & Natur
emoji-group-food = Essen & Trinken
emoji-group-travel = Reisen & Orte
emoji-group-activities = Aktivitäten
emoji-group-objects = Gegenstände
emoji-group-symbols = Symbole
emoji-group-flags = Flaggen
emoji-skin-tone = Hautfarbe
emoji-tone-default = Standard-Hautfarbe
emoji-tone-light = Helle Hautfarbe
emoji-tone-medium-light = Mittelhelle Hautfarbe
emoji-tone-medium = Mittlere Hautfarbe
emoji-tone-medium-dark = Mitteldunkle Hautfarbe
emoji-tone-dark = Dunkle Hautfarbe
emoji-none = Kein Emoji gefunden
message-show-more = Mehr anzeigen
message-show-less = Weniger anzeigen
message-sending = Wird gesendet …
message-status-sent = Gesendet
message-status-read = Gelesen
message-not-sent = Noch nicht gesendet, wird automatisch wiederholt
message-send-failed = Nicht gesendet
message-send-retry = Erneut senden
message-send-discard = Löschen
message-deleted-toast = Nachricht gelöscht
composer-also-send = Auch in { $name } senden
pins-jump = Zur Nachricht springen

## Failed actions (an error toast) and the panic overlay
action-running = Läuft…

## Message toolbar and action sheet (#320)
action-with-shortcut = { $action } ({ $shortcut })
action-copy-text = Text kopieren
action-copied = Kopiert
action-copy-failed = Konnte nicht in die Zwischenablage kopieren
action-copy-link-unready = Der Link ist noch nicht bereit. Versuch es gleich noch einmal.
message-action-sheet = Aktionen für diese Nachricht

## Previews of linked messages (#319)
linked-message-from = In { $conversation }
linked-message-from-thread = Aus einem Thread in { $conversation }
linked-message-open = Zur Nachricht
linked-message-deleted = Diese Nachricht wurde gelöscht
linked-message-not-loaded = Nachricht nicht geladen
linked-message-unavailable = Nachricht aus einem Gespräch, das du nicht sehen kannst

## Settings: sections, search and rows (#327)
# Dateien-Ansicht und Einrichtungs-Checkliste (Issue #301)
composer-placeholder-notes = Notiere etwas für dich
