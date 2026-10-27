# TeamCraft UI – Deutsch
# Files, pictures and videos, previews, the lightbox and share links.

## Threads
files-count = { $count ->
    [one] { $count } Datei
   *[other] { $count } Dateien
  }
files-download-all = Alle herunterladen

## Einrichtung der Schlüssel
# ---- Meetings ----
share-page-loading = Link wird geprüft…
share-page-title = Dir wurde eine Datei geteilt
share-page-size = Größe: { $size }
share-page-password = Passwort
share-page-wrong-password = Das Passwort stimmt nicht.
share-page-download = Herunterladen
share-page-working = Wird entschlüsselt…
share-page-note = Die Datei wird in deinem Browser entschlüsselt. Der Schlüssel bleibt in diesem Link und wird nie an den Server gesendet.
share-page-done-title = Deine Datei ist bereit
share-page-save = Datei speichern
share-page-failed-title = Dieser Link funktioniert nicht
share-page-bad-link = Der Link ist unvollständig oder beschädigt. Bitte den Absender, ihn erneut zu teilen, und den ganzen Link zu kopieren.
share-page-unavailable = Der Link ist abgelaufen, aufgebraucht oder widerrufen. Bitte den Absender um einen neuen.
share-page-too-large = Diese Datei ist zu groß, um sie im Browser zu öffnen. Bitte den Absender, sie anders zu teilen.
share-page-network = Der Server war nicht erreichbar. Verbindung prüfen und die Seite neu laden.
share-page-damaged = Die Datei konnte nicht entschlüsselt werden. Der Link gehört vielleicht zu einer anderen Datei, oder der Download war beschädigt. Bitte erneut versuchen.
share-page-retry = Erneut versuchen

# Message actions
attachment-menu-label = Datei
attachment-menu-open = Öffnen
attachment-menu-save = Speichern unter…
attachment-menu-copy-image = Bild kopieren
file-save-failed = Die Datei konnte nicht gespeichert werden.

# Files and link previews
files-empty = Dateien und Bilder, die hier geteilt werden, erscheinen in diesem Tab.
files-empty-title = Noch keine Dateien
shared-kind-label = Was geteilt wurde
shared-kind-files = Dateien
shared-kind-links = Links
links-empty-title = Noch keine Links
links-empty = Links, die in dieser Unterhaltung geteilt werden, erscheinen hier.
links-more = Mehr anzeigen
links-show-message = Nachricht zeigen
file-download = Herunterladen
file-progress = Übertragungsfortschritt
file-saved = Auf diesem Gerät gespeichert
file-download-network = Herunterladen fehlgeschlagen. Prüfe deine Verbindung und versuche es erneut.
file-download-storage-unavailable = Der Dateispeicher ist gerade nicht erreichbar. Versuche es gleich noch einmal.
file-download-missing = Diese Datei ist nicht mehr auf dem Server.
file-download-intercepted = Das Netzwerk hat den Download blockiert: Eine Anmeldeseite hat geantwortet oder ein Zertifikat ist nicht vertrauenswürdig.
file-download-corrupted = Die Datei kam beschädigt an und wurde nicht gespeichert.
file-save-unwritable = Die Datei konnte dort nicht geschrieben werden. Wähle einen anderen Ordner oder prüfe, ob TeamCraft dort schreiben darf.
file-too-large = Datei ist zu groß
file-retry = Erneut versuchen
file-upload-stalled = Hochladen fehlgeschlagen. Die Nachricht wartet, bis die Datei gesendet ist.
file-uploading = Wird hochgeladen…
file-cancel-upload = Hochladen abbrechen
file-progress-percent = { $percent } %
file-progress-bytes = { $done } / { $total }
file-upload-retrying = Wird erneut versucht …
file-upload-failed = Hochladen fehlgeschlagen
file-upload-storage-unavailable = Der Dateispeicher ist gerade nicht erreichbar. Versuche es gleich noch einmal.
file-upload-unreadable = Die Datei ließ sich nicht mehr lesen. Hänge sie erneut an.
file-preview = Vorschau anzeigen
preview-unavailable = Die Datei konnte nicht geladen werden. Prüfe deine Verbindung und versuche es erneut.
preview-show-source = Quelltext anzeigen
preview-show-formatted = Formatiert anzeigen
preview-html-note = Wird ohne Skripte und ohne Inhalte aus dem Web angezeigt.
preview-links = { $count ->
    [one] { $count } Link in dieser Datei
   *[other] { $count } Links in dieser Datei
  }
preview-pdf-unreadable = Dieses PDF konnte nicht geöffnet werden. Lade es herunter, um es anderswo anzusehen.
preview-zoom-in = Vergrößern
preview-zoom-out = Verkleinern
preview-page = Seite
preview-find = Im Dokument suchen
preview-find-none = Keine Treffer
preview-too-large-title = Zu groß für eine Vorschau
preview-too-large = Diese Datei ist { $size } groß, mehr als die { $limit }, bis zu denen sich eine Vorschau öffnet. Lade sie herunter, um sie zu öffnen, oder erhöhe die Grenze in den Einstellungen unter Allgemein.
preview-download-to-open = Zum Öffnen herunterladen
text-preview-preparing = Vorschau wird vorbereitet…
text-preview-binary = Diese Datei sieht nicht nach Text aus und kann deshalb nicht angezeigt werden. Lade sie herunter, um sie mit einer anderen App zu öffnen.
text-preview-tools = Textwerkzeuge
text-preview-content = Inhalt von { $name }
text-preview-empty = Diese Datei ist leer.
text-preview-find = In Datei suchen
text-preview-find-count = { $current } von { $total }
text-preview-find-none = Keine Treffer
text-preview-find-previous = Vorheriger Treffer
text-preview-find-next = Nächster Treffer
text-preview-go-to-line = Gehe zu Zeile
text-preview-line = Zeile
text-preview-level = Angezeigte Stufen
text-preview-level-all = Alle Stufen
text-preview-level-info = Info und höher
text-preview-level-warn = Warnungen und Fehler
text-preview-level-error = Nur Fehler
text-preview-wrap = Zeilen umbrechen
text-preview-colors = Farben
text-preview-format-json = JSON formatieren
text-preview-copy = Auswahl kopieren oder alle angezeigten Zeilen
text-preview-filtered = { $shown } von { $total } Zeilen angezeigt.
text-preview-long-lines = Sehr lange Zeilen werden in Teilen angezeigt.
text-preview-json-invalid = Kein gültiges JSON (Zeile { $line }, Spalte { $column }), daher unverändert angezeigt.
text-preview-json-too-deep = Zu tief verschachtelt zum Formatieren, daher unverändert angezeigt.
text-preview-json-too-large = Zu groß zum Formatieren, daher unverändert angezeigt.
text-preview-truncated = Nur die ersten { $count } Zeilen werden angezeigt. Lade die Datei herunter, um alles zu sehen.
text-preview-encoding-repaired = Einige Bytes sind kein gültiges UTF-8 und werden als Ersatzzeichen angezeigt.
text-preview-encoding-utf16 = Als UTF-16 gelesen.
text-preview-encoding-latin1 = Kein UTF-8, daher als Latin-1 (Windows-1252) gelesen.
file-share = Freigabelink…

# Workspace management
image-pick-type = Bilder müssen PNG-, JPEG- oder WebP-Dateien sein.
image-pick-size = Das Bild ist zu groß (höchstens 20 MB).
image-pick-unreadable = Das Bild konnte nicht gelesen werden.
crop-title-avatar = Profilbild zuschneiden
crop-title-space = Space-Bild zuschneiden
crop-title-channel = Kanalbild zuschneiden
crop-stage = Bildausschnitt
crop-help = Ziehen zum Verschieben, scrollen oder mit zwei Fingern zoomen. Tastatur: Pfeiltasten verschieben, + und − zoomen, R dreht.
crop-preparing = Bild wird vorbereitet …
crop-zoom = Zoom
crop-zoom-value = { $percent } %
crop-zoom-in = Vergrößern
crop-zoom-out = Verkleinern
crop-rotate = Um 90° drehen
crop-reset = Zurücksetzen
crop-preview = Vorschau
crop-preview-profile = Profil
crop-preview-messages = Nachrichten
crop-preview-lists = Listen
crop-preview-settings = Einstellungen
crop-preview-header = Kanalkopf
crop-preview-rail = Space-Leiste
crop-preview-sidebar = Seitenleiste
crop-apply = Übernehmen
crop-retry = Erneut versuchen
crop-uploading = Wird gespeichert …
preview-label = Vorschau von { $name }
preview-badge = Vorschau
preview-title = Beitreten zum Mitlesen
preview-body = Nachrichten sind für die Mitglieder dieses Kanals Ende-zu-Ende-verschlüsselt. Sobald du beigetreten bist, kannst du hier lesen und schreiben.
preview-bar = Du siehst eine Vorschau von #{ $name }
preview-details-button = Details
preview-join = Kanal beitreten
preview-joining = Wird beigetreten…
preview-topic = Thema
preview-no-topic = Kein Thema festgelegt.
preview-archived = Dieser Kanal ist archiviert; beitreten ist nicht möglich.
preview-missing-title = Kanal nicht gefunden
preview-missing = Er wurde vielleicht privat gemacht oder gelöscht.

## Daily-use improvements (docs/ux-backlog.md)
drop-files-here = Zum Anhängen hier ablegen
drop-files-unreadable = Die abgelegten Dateien konnten nicht gelesen werden. Hänge sie stattdessen mit der Plus-Schaltfläche an.
drop-file-too-large = { $name } ist größer als { $limit } und zu groß, um es per Ablegen anzuhängen. Hänge es stattdessen mit der Plus-Schaltfläche an.
paste-picture-failed = Das Bild aus der Zwischenablage konnte nicht eingefügt werden.

## Window controls of the desktop app's own title bar
# Bildbetrachter
lightbox-toolbar = Dateiaktionen
lightbox-position = { $index } von { $count }
lightbox-previous = Vorheriges Bild
lightbox-next = Nächstes Bild
lightbox-zoom-in = Vergrößern
lightbox-zoom-out = Verkleinern
lightbox-zoom-reset = Zoom { $zoom }, an Fenster anpassen
lightbox-show-all = Alle Bilder und Videos der Unterhaltung
lightbox-save = Herunterladen
lightbox-saving = Wird gespeichert …
lightbox-forward = Nachricht weiterleiten
lightbox-loading = Datei in voller Größe wird geladen …
lightbox-failed = Diese Datei kann nicht angezeigt werden.
lightbox-gone = Diese Datei ist nicht mehr verfügbar.
lightbox-too-large = Diese Datei ist zu groß, um sie hier anzuzeigen. Lade sie stattdessen herunter.
lightbox-shortcut = { $action } ({ $key })
lightbox-close-tip = Schließen (Esc)
lightbox-view-tools = Ansicht
lightbox-rotate = Drehen
lightbox-zoom = Zoom
lightbox-fit = An Fenster anpassen
lightbox-play = Abspielen
lightbox-pause = Anhalten
lightbox-thread-show = Thread einblenden
lightbox-thread-hide = Thread ausblenden
lightbox-more = Weitere Aktionen
lightbox-view-thread = Thread anzeigen
lightbox-jump = Zur Nachricht springen
lightbox-saved-added = Zu „Gespeichert“ hinzugefügt
lightbox-saved-removed = Aus „Gespeichert“ entfernt
lightbox-copy-link = Link zur Nachricht kopieren
lightbox-link-copied = Link kopiert
lightbox-link-unavailable = Der Link ist noch nicht bereit. Versuche es gleich noch einmal.
lightbox-share-copied = Freigabelink kopiert
share-title = Freigabelink
share-active = Aktive Links
share-new = Neuer Link
share-expiry = Link gilt
share-expiry-hour = 1 Stunde
share-expiry-day = 1 Tag
share-expiry-week = 7 Tage
share-expiry-month = 30 Tage
share-limit = Download-Limit
share-limit-none = Kein Limit
share-password = Passwort (optional)
share-password-hint = Mindestens { $count } Zeichen; länger ist besser, mehrere zufällige Wörter eignen sich gut. Wer den Link hat, kann Passwörter offline durchprobieren, ein kurzes schützt kaum. Das Passwort wird nicht an den Server gesendet; gib es auf einem anderen Weg weiter als den Link.
share-create = Link erstellen
share-created = Freigabelink erstellt
share-copy = Kopieren
share-revoke = Widerrufen
share-revoked = Freigabelink widerrufen
share-expires = Läuft ab { $when }
share-downloads = { $used } Downloads
share-downloads-limited = { $used } von { $max } Downloads
share-has-password = Passwort
lightbox-copy-image = Bild kopieren
lightbox-image-copied = Bild kopiert
lightbox-copy-failed = Das Bild konnte nicht kopiert werden.
lightbox-details = Dateidetails
lightbox-details-close = Dateidetails schließen
lightbox-details-name = Name
lightbox-details-type = Typ
lightbox-details-size = Größe
lightbox-details-dimensions = Abmessungen
lightbox-details-dimensions-value = { $width } × { $height } px
lightbox-details-sender = Gesendet von
lightbox-details-sent = Gesendet
lightbox-thumbnails = Dateien in dieser Ansicht
lightbox-thumbnail = { $name } anzeigen
media-open = { $name } öffnen
video-play = { $name } abspielen
video-expand = Größer anzeigen
video-play-button = Abspielen
video-pause = Pause
video-seek = Wiedergabeposition
video-position = { $current } von { $total }
video-mute = Ton aus
video-unmute = Ton an
video-volume = Lautstärke
video-fullscreen = Vollbild
video-exit-fullscreen = Vollbild beenden

## Settings: sections, search and rows (#327)
# Dateien-Ansicht und Einrichtungs-Checkliste (Issue #301)
files-title = Dateien
files-filter = Dateien filtern
files-filter-all = Alle
files-filter-media = Bilder und Videos
files-filter-documents = Dokumente
files-all-empty = Dateien und Bilder aus allen deinen Unterhaltungen erscheinen hier.
