# TeamCraft UI – Deutsch
# General words, the app's frame (menus, window, tray, about, updates) and app-wide states.

## Allgemein
skip-to-content = Zum Hauptinhalt springen
cancel = Abbrechen
save = Speichern
close = Schließen

## Kompaktes Layout (Telefone): untere Navigation, zurück, Menüs; Anrufe im Browser
back = Zurück
history-back = Zurück
history-forward = Vor

## Seitenleiste
catching-up = Wird nachgeladen…
catching-up-detail = Nachrichten aus der Zeit, in der du weg warst, werden noch geladen.

## Nachrichten
badge-bot = BOT
badge-imported = IMPORTIERT
imported-from-synology = Synology Chat
imported-from-other = einem anderen Chat
imported-title = Importiert aus { $source } von { $importer }
badge-guest = GAST
badge-organization = Organisation

## Anmeldung
data-failed-title = Deine Daten konnten nicht geladen werden
data-failed-body = Der Server hat geantwortet, aber deine Spaces und Unterhaltungen konnten nicht gelesen werden. Versuche es erneut.
data-retry = Erneut versuchen

## Einstellungen: Darstellung, Seitenleiste, Sprache, Datenschutz, Geräte, Anrufe
language-en = English
language-de = Deutsch

## Einrichtung der Schlüssel
# ---- Meetings ----
bundle-rejected = Der Server hat eine Version dieser App angeboten, die nicht mit dem Release-Schlüssel signiert ist. Du nutzt weiter die vorherige Version. Gib keine Geheimnisse ein, bis deine Administration das geprüft hat.
bundle-unsigned = Diese Web-App ist nicht mit einem Release-Schlüssel signiert; nichts prüft, ob der Server den echten Code sendet.
about-title = Über
about-bundle-verified = Web-App Version { $version }, geprüft gegen den Release-Schlüssel { $key }.
about-bundle-unsigned = Web-App: nicht signiert (Entwicklungs-Build). Nichts prüft den Code, den der Server sendet.
about-bundle-unsupported = Web-App: Dieser Browser kann Release-Signaturen nicht prüfen; der Code wird nicht geprüft.
about-bundle-rejected = Web-App: Eine neue Version wurde abgelehnt ({ $reason }); die vorherige geprüfte Version läuft.
about-bundle-desktop = Diese App bezieht ihren Code nicht vom Server.
about-trust-note = Die Web-App vertraut dem, der sie ausliefert: Ein Server, der die App austauscht, kann deine Nachrichten lesen, sobald du sie öffnest. Die Prüfung signierter Releases erkennt geänderte Dateien, nicht aber einen Server, der den Prüfer selbst austauscht. Für den stärksten Schutz nutze die Desktop- oder Mobil-App, die ihren eigenen Code mitbringen.
about-notices = Hinweise zu Drittsoftware

# Message actions
webview-download-saved = { $name } in „Downloads“ gespeichert
webview-download-saved-unnamed = In „Downloads“ gespeichert
webview-download-failed = Der Download wurde nicht abgeschlossen.
badge-pinned = Angeheftet
badge-saved = Gespeichert

# Files and link previews
route-unavailable = Diese Seite ist nicht verfügbar. Vielleicht wurde sie entfernt, oder du hast keinen Zugriff darauf.

# Workspace management
create = Erstellen
confirm = Bestätigen
badge-external = Extern
badge-archived = Archiviert
badge-private = Privat

# Profiles, people and groups (v1 REST UI)
weekday-mon = Mo
weekday-tue = Di
weekday-wed = Mi
weekday-thu = Do
weekday-fri = Fr
weekday-sat = Sa
weekday-sun = So

## Daily-use improvements (docs/ux-backlog.md)
offline-banner = Du bist offline. Die App verbindet sich von selbst wieder, sobald das Netzwerk zurück ist.
connection-banner-connecting = Verbindung zum Server wird aufgebaut…
connection-banner-unreachable = Der Server ist nicht erreichbar. Nachrichten warten, bis er wieder da ist. Nächster Versuch in { $seconds } s.
connection-banner-captive-portal = Dieses Netzwerk verlangt zuerst eine Anmeldung (statt des Servers hat eine Anmeldeseite geantwortet). Öffne eine Webseite, um dich anzumelden; Nachrichten warten. Nächster Versuch in { $seconds } s.
connection-banner-untrusted-certificate = Dem Zertifikat des Servers wird nicht vertraut. In einem Netzwerk, das verschlüsselten Verkehr prüft: Nutze ein anderes Netzwerk oder bitte deine IT, das Firmenzertifikat auf diesem Gerät zu installieren. Passiert es überall, ist vielleicht das Zertifikat des Servers fehlerhaft: Sag deiner Administration Bescheid. Nächster Versuch in { $seconds } s.
connection-hint-offline = Offline
connection-hint-connecting = Verbinden…
connection-hint-reconnecting = Neu verbinden…
connection-banner-retry = Jetzt versuchen
connection-banner-sign-in = Deine Sitzung ist abgelaufen. Nachrichten warten, bis du dich wieder anmeldest.
connection-banner-sign-in-action = Erneut anmelden
connection-banner-session-ended = Du wurdest bei der Anmeldung deiner Organisation oder auf einem anderen Gerät abgemeldet. Nachrichten warten, bis du dich wieder anmeldest.
connection-banner-session-timed-out = Deine Sitzung hat das Zeitlimit deiner Organisation erreicht. Nachrichten warten, bis du dich wieder anmeldest.
storage-quota-exceeded = Dein Browser hat keinen Speicherplatz mehr für TeamCraft. Neue Nachrichten und Schlüssel werden auf diesem Gerät nicht gespeichert und gehen beim Neuladen verloren. Gib Speicherplatz frei und lass diesen Tab offen: TeamCraft speichert sie, sobald es geht.
storage-closed = In einem anderen Tab wurde eine neuere Version von TeamCraft geöffnet. Lade diesen Tab neu, um weiterzuarbeiten; bis dahin werden Änderungen hier nicht gespeichert.
storage-not-saved = TeamCraft kann in diesem Browser keine Daten speichern. Änderungen gehen verloren, wenn du diesen Tab neu lädst.
storage-reload = Neu laden
release-ready = Eine neue Version von TeamCraft ist bereit.
release-too-old = Diese Version von TeamCraft wird vom Server nicht mehr unterstützt (Version { $version } oder neuer wird benötigt).
release-reload = Jetzt neu laden
release-switch-failed = Die neue Version ist noch nicht gestartet. Versuche es gleich noch einmal.
undo = Rückgängig
tray-show = Fenster anzeigen
tray-quit = Beenden
tray-tooltip-unread = { $name }: { $count } ungelesen
menubar-about = Über { $name }
menubar-services = Dienste
menubar-hide = { $name } ausblenden
menubar-hide-others = Andere ausblenden
menubar-show-all = Alle einblenden
menubar-quit = { $name } beenden
menubar-file = Ablage
menubar-close-window = Fenster schließen
menubar-edit = Bearbeiten
menubar-undo = Widerrufen
menubar-redo = Wiederholen
menubar-cut = Ausschneiden
menubar-copy = Kopieren
menubar-paste = Einsetzen
menubar-select-all = Alles auswählen
menubar-view = Darstellung
menubar-fullscreen = Vollbildmodus ein/aus
menubar-window = Fenster
menubar-minimize = Im Dock ablegen
menubar-zoom = Zoomen
menubar-help = Hilfe

## Desktop updates (docs/updates.md)
update-state-idle = TeamCraft ist aktuell.
update-state-unchecked = Noch nicht nach Updates gesucht.
update-state-checking = Suche nach einer neuen Version …
update-state-downloading = TeamCraft { $version } wird geladen …
update-state-downloading-percent = TeamCraft { $version } wird geladen … { $percent } %
update-state-installing = TeamCraft { $version } wird installiert …
update-installing = { $version } wird installiert …
update-downloading = Wird geladen …
update-downloading-percent = Wird geladen … { $percent } %
update-waiting-authorization = Warte auf Bestätigung …
update-waiting-authorization-hint = Bestätige im Passwortdialog des Systems, um TeamCraft { $version } zu installieren.
update-state-failed = Die letzte Suche nach Updates ist fehlgeschlagen: { $message }
update-off-policy = Deine Organisation verwaltet die Updates dieser App.
update-off-machine = Updates sind auf diesem Computer ausgeschaltet.
update-off-preference = TeamCraft sucht nicht nach neuen Versionen.
update-off-not_configured = Dieser Build kann sich nicht selbst aktualisieren. Neue Versionen gibt es auf der Release-Seite.
update-off-unpackaged = Entwicklungs-Build: keine Updates.
update-ready = TeamCraft { $version } ist bereit.
update-ready-banner = TeamCraft { $version } ist bereit – zum Aktualisieren neu starten.
update-ready-installed = TeamCraft { $version } ist installiert – neu starten, um die neue Version zu nutzen.
update-restart-short = Neu starten
update-available = TeamCraft { $version } ist verfügbar.
update-restart = Neu starten und aktualisieren
update-restart-after-call = Nach dem Anruf neu starten
update-download = Herunterladen
update-later = Später
update-whats-new = Was ist neu
update-package-manager = Aktualisiere es mit deiner Paketverwaltung:
update-package-kit = Jetzt aktualisieren
update-flatpak = Aktualisiere es mit Flatpak oder deinem Software-Center:
update-copy-command = Kopieren
update-command-copied = Kopiert
update-download-package = Neues Paket herunterladen
update-download-appimage = Neues AppImage herunterladen
update-add-repository = Paketquelle für automatische Updates einrichten
update-dismiss = Ausblenden
update-too-old = Diese TeamCraft-Version ist für deine Organisation zu alt. Bitte aktualisiere auf { $version } oder neuer.
update-too-old-self = Das Update ist unterwegs; starte neu, sobald TeamCraft es meldet.
update-too-old-elsewhere = Das Update kommt von deiner IT-Abteilung oder aus dem Software-Center.

## Window controls of the desktop app's own title bar
window-controls = Fenster
window-minimize = Minimieren
window-maximize = Maximieren
window-restore = Wiederherstellen
window-close = Schließen

## Failed actions (an error toast) and the panic overlay
error-details = Details
error-generic = Das hat nicht geklappt. Versuche es erneut; wenn es weiter fehlschlägt, kopiere die Details für deinen Admin.
area-load-failed = Dieser Teil der App konnte nicht geladen werden. Prüfe deine Verbindung und versuche es dann erneut. Klappt es weiterhin nicht, lade die App neu: Vielleicht gibt es eine neue Version.
area-reload = App neu laden
error-network = Der Server ist nicht erreichbar. Prüfe deine Verbindung und versuche es erneut.
error-notification-not-allowed = Benachrichtigungen sind für diese App gesperrt. Erlaube sie in den Einstellungen deines Browsers oder Systems und versuche es erneut.
error-notification-unsupported = Dieser Browser kann keine Benachrichtigungen anzeigen.
error-notification-refused = Das System hat die Benachrichtigung nicht angezeigt. Prüfe die Benachrichtigungseinstellungen, etwa Fokus- oder Nicht-stören-Modi.
error-captive-portal = Dieses Netzwerk verlangt zuerst eine Anmeldung. Öffne eine Webseite, melde dich an und versuche es dann erneut.
error-untrusted-certificate = Dem Zertifikat des Servers wird nicht vertraut. In einem Netzwerk, das verschlüsselten Verkehr prüft: Nutze ein anderes Netzwerk oder bitte deine IT, das Firmenzertifikat auf diesem Gerät zu installieren. Passiert es überall, ist vielleicht das Zertifikat des Servers fehlerhaft: Sag deiner Administration Bescheid.
error-not-space-member = Diese Person ist noch nicht im Space. Lade sie zuerst in den Space ein und füge sie dann dem Channel hinzu.
error-forbidden = Dafür fehlt dir die Berechtigung. Frage einen Admin des Space oder Workspace.
error-signed-out = Deine Anmeldung ist abgelaufen. Melde dich erneut an und versuche es dann noch einmal.
error-account-deactivated = Dein Konto ist deaktiviert. Bitte einen Admin, es wieder zu aktivieren.
error-rate-limited = Zu viele Anfragen auf einmal. Warte einen Moment und versuche es erneut.
error-too-large = Das ist zu groß für den Server. Versuche es mit etwas Kleinerem.
error-server-trouble = Der Server hat gerade ein Problem. Versuche es in ein paar Minuten erneut.
error-conflict = Jemand hat das gleichzeitig geändert. Sieh dir den aktuellen Stand an und versuche es erneut.
error-not-found = Das gibt es nicht mehr. Es wurde vielleicht gelöscht oder verschoben.
panic-title = Etwas ist schiefgelaufen
panic-body = Die App ist auf einen Fehler gestoßen und muss neu geladen werden. Was du gerade getippt hast, kann verloren gehen.
panic-reload = Neu laden
panic-details-label = Technische Details für deinen Admin (Version und wo der Fehler auftrat)

## One active tab (#744)
tab-elsewhere-title = TeamCraft ist bereits in einem anderen Tab geöffnet
tab-elsewhere-body = Stattdessen hier verwenden? Der andere Tab wird inaktiv.
tab-moved-title = TeamCraft wurde in einem anderen Tab geöffnet
tab-moved-body = Dieser Tab ist jetzt inaktiv. Verwende TeamCraft hier, um in diesem Tab weiterzumachen.
tab-call-title = Im anderen Tab läuft ein Anruf
tab-call-body = Wenn du TeamCraft hier verwendest, endet der Anruf dort.
tab-timed-out = Der andere Tab hat nicht reagiert. Versuch es noch einmal.
tab-use-here = Hier verwenden
tab-end-call-use-here = Anruf beenden und hier verwenden
tab-cancel = Abbrechen
tab-close = Schließen
tab-close-hint = Du kannst diesen Tab jetzt schließen.
