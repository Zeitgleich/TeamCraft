# TeamCraft UI – Deutsch
# Calls: the call screen and pre-join, incoming calls, huddles, screen sharing and effects.

## Kompaktes Layout (Telefone): untere Navigation, zurück, Menüs; Anrufe im Browser
calls-browser-title = Anrufe sind hier nicht verfügbar
calls-browser-body = Rufe aus der Desktop- oder Mobil-App von TeamCraft an. Deine Unterhaltungen sind dort auch.
calls-browser-download = App herunterladen
calls-unsupported-title = Dieser Browser kann keine Anrufe führen
calls-unsupported-body = Anrufe brauchen ein aktuelles Chrome, Edge, Firefox oder Safari. Aktualisiere deinen Browser, wechsle zu einem davon oder rufe aus der TeamCraft-App an.
calls-other-tab-title = Anrufe laufen in deinem anderen Tab
calls-other-tab-body = TeamCraft ist in einem anderen Tab dieses Browsers geöffnet, und Anrufe laufen dort. Führe Anrufe dort oder schließe ihn und lade diesen Tab neu.

## Anrufe und Huddles
call-start-audio = Audioanruf starten
call-start-video = Videoanruf starten
huddle-start = Huddle starten
huddle-join = Huddle beitreten
huddle-leave = Huddle verlassen
huddle-in-progress = Huddle läuft
huddle-on-other-device = Du bist in diesem Call auf einem anderen Gerät
huddle-join-here = Auch hier beitreten
huddle-move-here = Call hierher holen
huddle-leave-everywhere = Auf allen Geräten verlassen
huddle-bar-label = Huddle
call-title-direct = Anruf mit { $name }
call-title-group = Anruf in { $name }
call-title-huddle = Huddle in { $name }
call-title-unnamed = Anruf
call-title-huddle-unnamed = Huddle
call-open = Anruf öffnen
call-in-progress = Im Anruf
call-pill-label = Laufender Anruf
call-controls-label = Anrufsteuerung
call-leave = Verlassen
call-minimize = Zurück zum Chat
call-add-people = Personen hinzufügen
call-add-chosen = Hinzuzufügende Personen
call-already-in = Schon im Anruf
call-add-hint = Der Anruf zieht mit allen Teilnehmenden in einen neuen Meetingraum um. Die Direktnachrichten bleiben, wie sie sind; die hinzugefügten Personen sehen sie nicht.
call-add = Zum Anruf hinzufügen
call-meeting-title = Anruf
call-move-offer = { $name } hat den Anruf in einen Meetingraum verschoben und { $added } hinzugefügt.
call-move-join = Meetingraum beitreten
call-effects = Hintergrundeffekte
effects-system-hint = macOS kann eigene Videoeffekte darüberlegen (Porträt, Hintergrund, Studiolicht). Ist einer davon an, wähle hier „Keiner“, damit sie sich nicht addieren.
effects-system-open = Videoeffekte von macOS öffnen
call-layout-gallery = Galerieansicht
call-layout-speaker = Ansicht „Aktiver Sprecher“
call-hand-raise = Hand heben
call-hand-lower = Hand senken
call-participants = { $count ->
    [one] { $count } Person
   *[other] { $count } Personen
  }
call-elapsed = Gesprächsdauer { $time }
call-encrypted = Ende-zu-Ende-verschlüsselt
call-encrypted-detail = Nur Personen in diesem Anruf können ihn sehen und hören. Vergleiche den Code zur Kontrolle.
call-verification-code = Code { $code }
control-mic-mute = Mikrofon stummschalten
control-mic-unmute = Mikrofon einschalten
control-camera-off = Kamera ausschalten
control-camera-on = Kamera einschalten
control-camera-flip = Kamera wechseln
control-camera-choose = Kamera wählen
call-camera-fallback = { $failed } ließ sich nicht öffnen, daher wird { $used } verwendet.
share-start = Bildschirm teilen
share-stop = Teilen beenden
share-options = Bildschirm teilen
share-sound = Ton teilen
share-sound-hint = Was dein Computer abspielt, etwa ein Video. Der Ton dieses Anrufs bleibt außen vor.
share-motion = Flüssige Bewegung bevorzugen
share-motion-hint = Für Video, Spiele und Scrollen: 60 Bilder pro Sekunde bei bis zu 1080p. Aus: schärfster Text, bis zu 1440p bei 30 Bildern pro Sekunde.
share-choose = Auswählen, was geteilt wird
share-sound-on = Mit Ton
share-sound-unavailable = Ton kann auf diesem System nicht geteilt werden
share-banner-self = Du teilst deinen Bildschirm
share-banner-remote = { $name } teilt den Bildschirm
share-screen-label = Bildschirm von { $name }
tile-you = Du
tile-muted = stummgeschaltet
tile-hand = Hand gehoben
tile-speaking = spricht
tile-reconnecting = Verbindung wird wiederhergestellt
video-stalled = Video pausiert: schwache Verbindung

## Statistiken für Nerds (#938): technische Angaben über jedem Video des Anrufs
stats-title = Statistiken für Nerds
stats-copy = Statistiken kopieren
stats-copied = Statistiken kopiert
stats-sent-camera = Deine Kamera, gesendet
stats-sent-screen = Dein Bildschirm, gesendet
stats-received = Empfangenes Video (Stream { $stream })
stats-resolution = Auflösung
stats-frame-rate = Bildrate
stats-bitrate = Bitrate
stats-codec = Codec
stats-encoder = Encoder
stats-layers = Simulcast-Ebenen
stats-layer = Simulcast-Ebene
stats-loss = Paketverlust
stats-network = Netzwerk
stats-frames = Bilder
stats-encoder-hardware = { $name } (Hardware)
stats-encoder-software = { $name } (Software)
stats-encoder-browser = { $name } (wählt der Browser)
stats-value-sent-resolution = { $sent } (Aufnahme { $capture })
stats-value-sent-frame-rate = { $fps } fps (Aufnahme { $capture })
stats-value-received-frame-rate = { $fps } fps (angezeigt { $drawn })
stats-value-sent-bitrate = { $rate } (Schätzung { $estimate })
stats-value-sent-layers = { $active } (Ziel { $target })
stats-value-received-layer = { $layer } von { $offered } (angefragt ≤ { $max })
stats-value-uplink-loss = { $loss } (Uplink)
stats-value-network = RTT { $rtt } · Jitter { $jitter }
stats-value-frames = verworfen { $dropped } · schwarz { $black }
stats-waiting = Warte auf die ersten Werte…
# Der Chat neben dem Video (#936)
call-chat = Chat
call-chat-label = Chat des Anrufs
call-chat-show = Chat anzeigen
call-chat-hide = Chat ausblenden
call-chat-show-unread = { $count ->
    [one] Chat anzeigen, { $count } ungelesene Nachricht
   *[other] Chat anzeigen, { $count } ungelesene Nachrichten
  }
call-chat-toast-open = Neue Nachricht von { $name }. Chat anzeigen
call-chat-toast-files = Hat Dateien gesendet

## Vorschau vor dem Beitritt
prejoin-quick-controls = Mikrofon und Kamera
prejoin-direct-hint = Prüfe deine Geräte, bevor der Anruf startet.
prejoin-nobody = Noch niemand ist beigetreten.
prejoin-people-in-call = { $count ->
    [one] { $count } Person ist im Anruf.
   *[other] { $count } Personen sind im Anruf.
  }
prejoin-camera = Kamera
prejoin-microphone = Mikrofon
prejoin-speaker = Lautsprecher
prejoin-mic-level = Mikrofonpegel
prejoin-test-speaker = Lautsprecher testen
prejoin-testing-speaker = Spielt …
prejoin-effects = Hintergrund
prejoin-effects-hint = Effekte werden auf deinem Gerät angewendet, bevor dein Video gesendet wird.
prejoin-mirror = Mein Video spiegeln
prejoin-join = Jetzt beitreten
prejoin-camera-off = Kamera aus
prejoin-camera-at-join = Deine Kamera startet beim Beitreten
prejoin-camera-starting = Kamera startet …
prejoin-camera-failed = Kamera nicht verfügbar
prejoin-no-camera = Keine Kamera gefunden
prejoin-no-microphone = Kein Mikrofon gefunden
prejoin-camera-blocked = Kamera blockiert
prejoin-microphone-blocked = Mikrofon blockiert
prejoin-no-camera-hint = Schließe eine Kamera an, um sie einzuschalten.
prejoin-no-microphone-hint = Schließe ein Mikrofon an, um es einzuschalten.
prejoin-camera-blocked-hint = Erlaube die Kamera für diese Seite im Browser (Symbol in der Adressleiste).
prejoin-microphone-blocked-hint = Erlaube das Mikrofon für diese Seite im Browser (Symbol in der Adressleiste).
effect-none = Keiner
effect-blur-light = Leicht unscharf
effect-blur-strong = Stark unscharf
effect-group-photos = Fotos
effect-meeting-room = Besprechungsraum
effect-open-plan-office = Großraumbüro
effect-home-office = Homeoffice
effect-minimal-office = Schlichtes Büro
effect-living-room = Wohnzimmer
effect-bright-room = Helles Zimmer
effect-library-shelves = Bibliotheksregale
effect-loft = Loft
effect-sofa-beige = Beiges Sofa
effect-white-panels = Weiße Paneele
effect-lounge = Lounge
effect-grey-lounge = Graue Lounge
effect-orange-wall = Orange Wand
effect-library-aisle = Bibliotheksgang
effect-empty-room = Leerer Raum
effect-lake-morning = See am Morgen
effect-group-illustrations = Illustrationen
effect-office = Büro
effect-bookshelf = Bücherregal
effect-sunrise = Sonnenaufgang
effect-forest = Wald
effect-more = Mehr Hintergründe
effect-fewer = Weniger Hintergründe
effect-upload = Bild hochladen
effect-upload-too-large = Dieses Bild ist größer als { $limit }.
effect-upload-unreadable = Das Bild konnte nicht gelesen werden.
effect-delete = Hintergrund { $name } löschen

## Eingehende Anrufe
incoming-title = Eingehender Anruf
incoming-audio = { $name } ruft dich an
incoming-video = { $name } ruft dich per Video an
incoming-decline = Ablehnen
incoming-accept-audio = Mit Audio annehmen
incoming-accept-video = Mit Video annehmen

## Einstellungen: Darstellung, Seitenleiste, Sprache, Datenschutz, Geräte, Anrufe
calls-devices = Standardgeräte
calls-devices-hint = Werden beim Beitritt zu einem Anruf verwendet. Vor dem Beitritt kannst du sie noch ändern.
calls-system-default = Systemstandard
calls-joining = Beim Beitritt
calls-start-muted = Mit ausgeschaltetem Mikrofon beitreten
calls-start-camera-off = Mit ausgeschalteter Kamera beitreten
calls-audio = Audio
calls-noise = Rauschunterdrückung
calls-echo = Echounterdrückung
calls-ptt = Push-to-Talk
calls-ptt-hint = Taste halten zum Sprechen statt offenem Mikrofon.
calls-status-sounds = Statustöne im Anruf
calls-status-sounds-hint = Ein leiser Ton wiederholt sich, solange deine Verbindung unterbrochen ist; kurze Töne zeigen, wenn sie wieder steht oder der Anruf beendet ist. Nur du hörst sie.
calls-status-sounds-all = Meine und die Verbindungen anderer
calls-status-sounds-own-only = Nur meine Verbindung
calls-status-sounds-off = Aus
calls-background = Standardhintergrund
calls-background-hint = Wird auf deinem Gerät angewendet, bevor dein Video gesendet wird.

## Einrichtung der Schlüssel
# ---- Meetings ----
call-connecting = Verbindung wird hergestellt …
call-reconnecting = Verbindung unterbrochen, wird wiederhergestellt …
control-deafen = Anrufton stummschalten
control-undeafen = Anrufton einschalten

## Aktivität, zuletzt online und Anrufliste
calls-title = Anrufe
calls-filter = Anrufe filtern
calls-all = Alle Anrufe
calls-missed = Verpasst
calls-empty = Anrufe, die du führst, annimmst oder verpasst, erscheinen hier.
calls-empty-title = Noch keine Anrufe
calls-direction-missed = Verpasst
calls-direction-incoming = Eingehend
calls-direction-outgoing = Ausgehend
calls-ongoing = Läuft noch
calls-call-back = Zurückrufen
calls-join = Beitreten

## Key names in keyboard shortcuts (the caps shown on a keyboard)
calls-unavailable = Anrufe sind hier nicht verfügbar. Nutze deinen anderen TeamCraft-Tab oder die TeamCraft-App.

## Failed actions (an error toast) and the panic overlay
error-call-microphone-denied = Der Browser hat das Mikrofon blockiert. Erlaube es für diese Seite (Symbol in der Adressleiste) und tritt dann erneut bei.
error-call-microphone-missing = Es wurde kein Mikrofon gefunden. Schließe eines an und tritt dann erneut bei.
error-call-microphone-in-use = Das Mikrofon lässt sich nicht öffnen. Schließe andere Apps, die es nutzen, und versuche es erneut.
error-call-camera-denied = Der Browser hat die Kamera blockiert. Erlaube sie für diese Seite (Symbol in der Adressleiste) und schalte die Kamera dann erneut ein.
error-call-camera-missing = Es wurde keine Kamera gefunden. Schließe eine an und schalte die Kamera dann erneut ein.
error-call-camera-in-use = Eine andere App nutzt die Kamera. Schließe diese App oder wähle eine andere Kamera.
error-call-camera-unsupported = Das Bildformat dieser Kamera wird nicht unterstützt, dein Video bleibt aus. Probiere eine andere Kamera.
error-call-camera-not-connected = Die iPhone-Kamera hat sich nicht verbunden: Sie wurde auf dem iPhone abgelehnt, oder das iPhone ist gesperrt oder zu weit entfernt. Erlaube sie auf dem iPhone oder wähle eine andere Kamera.
error-call-camera-unavailable = Die Kamera lässt sich nicht öffnen. Wähle eine andere Kamera oder versuche es erneut.
error-call-screen-unavailable = Dein Bildschirm kann von hier aus nicht geteilt werden.
error-call-screen-denied = macOS lässt TeamCraft deinen Bildschirm nicht teilen, auch wenn TeamCraft dort eingeschaltet ist. Wähle in den Systemeinstellungen unter Datenschutz & Sicherheit → Bildschirm- & Systemaudioaufnahme TeamCraft aus und entferne es mit −. Beende TeamCraft dann, öffne es erneut und teile noch einmal; erlaube es, wenn macOS fragt.
call-screen-settings-open = Systemeinstellungen öffnen
error-call-screen-needs-click = Dein Browser hat die Bildschirmauswahl nicht geöffnet. Wähle erneut „Bildschirm teilen“ und dann, was geteilt wird.
error-call-camera-no-picture = Deine Kamera ist an, liefert aber kein Bild, daher bleibt dein Video aus. Prüfe, ob eine andere App sie nutzt, oder wähle eine andere Kamera.
error-call-camera-stopped = Deine Kamera hat aufgehört, daher ist dein Video aus: Sie wurde getrennt, oder das System oder eine andere App hat sie übernommen. Schalte sie wieder ein oder wähle eine andere Kamera.
error-call-screen-no-picture = Deine Bildschirmfreigabe hat begonnen, liefert aber kein Bild und wurde daher beendet. Teile erneut oder wähle einen anderen Bildschirm oder ein anderes Fenster.
error-call-camera-black = Deine Kamera liefert nur Schwarz. Prüfe, ob sie verdeckt oder mit einer Abdeckung verschlossen ist, oder wähle eine andere Kamera.
error-call-screen-black = Deine Bildschirmfreigabe zeigt nur Schwarz. Wenn du die Bildschirmaufnahme gerade erlaubt hast, beende TeamCraft, öffne es erneut und teile noch einmal. Geschützte Videos erscheinen ebenfalls schwarz.
error-call-unsupported = Dieser Browser kann keine Anrufe führen. Nutze ein aktuelles Chrome, Edge, Firefox oder Safari oder die TeamCraft-App.
error-call-device-limit = Dein Konto ist bereits auf so vielen Geräten im Anruf, wie erlaubt sind. Verlasse den Anruf zuerst auf einem davon und tritt dann von diesem Gerät bei.
error-call-server-unreachable = Der Anrufserver ist nicht erreichbar. Das Netzwerk blockiert vielleicht Anrufe (UDP); versuche ein anderes Netzwerk oder frag deinen Admin.
error-call-background-required = Deine Organisation verlangt einen Hintergrund hinter deinem Kamerabild, den der Browser noch nicht einfügen kann. Schalte deine Kamera in der TeamCraft-App ein.
error-call-encryption-pending = Dieses Gerät kann dem Anruf noch nicht beitreten: Es wird gerade in die Verschlüsselung der Unterhaltung aufgenommen. Lass TeamCraft auf einem anderen deiner Geräte geöffnet oder warte, bis ein Mitglied online ist, und tritt dann erneut bei. Bei einem Meeting muss eventuell zuerst der Organisator es einmal öffnen.
error-call-move-refused = Der Anruf wurde nicht verschoben: Der Meetingraum passt nicht zu dem, was in der Unterhaltung angekündigt wurde. Bleib in diesem Anruf oder tritt dem Meetingraum über die Liste bei, wenn du ihm vertraust.
error-call-speakers-unavailable = Die gewählten Lautsprecher lassen sich hier nicht nutzen. Der Testton lief über die Standardausgabe.
error-call-speakers-missing = Es wurden keine Lautsprecher gefunden. Schließe Lautsprecher oder Kopfhörer an und versuche es erneut.
error-call-sound-blocked = Der Browser hat der Seite keinen Ton erlaubt. Klicke irgendwo auf die Seite und versuche es erneut.

## Settings: sections, search and rows (#327)
calls-noise-hint = Filtert Tippen, Lüfter und andere Hintergrundgeräusche.
calls-echo-hint = Verhindert, dass andere sich über deine Lautsprecher selbst hören.

# Dateien-Ansicht und Einrichtungs-Checkliste (Issue #301)
calls-loading = Anrufe werden geladen
