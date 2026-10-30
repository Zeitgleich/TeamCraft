# TeamCraft UI – Deutsch
# Setting up this device: keys, recovery, linking and approving devices, key changes.

## Geräte-Freigabe, Verknüpfung und Schlüsseländerungen
approval-title = Neue Anmeldung
approval-question = Jemand hat sich auf { $device } angemeldet. Dieses Gerät freigeben?
approval-code-label = Code { $code }
approval-compare = Vergleiche diesen Code mit dem Code auf dem neuen Gerät. Gib nur frei, wenn sie übereinstimmen.
approval-location = In der Nähe von
approval-warning = Wenn du dich nicht gerade angemeldet hast, wähle „Nicht ich“. Mit der Freigabe erhält das neue Gerät Zugriff auf deine Nachrichten.
approval-approve = Freigeben
approval-deny = Nicht ich
approval-approved = Gerät freigegeben. Es erhält jetzt deine Schlüssel.
approval-denied = Anfrage abgelehnt. Prüfe deine Geräte in den Einstellungen, wenn du unsicher bist.
link-title = Dieses Gerät verknüpfen
link-qr-label = QR-Code zum Verknüpfen dieses Geräts
link-qr-note = Demo-Code, keine echte Anmeldung.
link-step-open = Öffne auf einem Gerät, auf dem du angemeldet bist, die Einstellungen und wähle Geräte.
link-step-scan = Wähle „Neues Gerät verknüpfen“ und scanne diesen Code.
link-step-code = Prüfe, dass beide Bildschirme denselben Code zeigen, und gib dann frei.
link-waiting = Warte auf Freigabe …
link-done = Gerät verknüpft. Deine Nachrichten werden geladen.
link-alternative = Keine Kamera? Melde dich hier an und gib den Code auf deinem anderen Gerät frei.
link-simulate = Freigabe simulieren
device-waiting-title = Dieses Gerät ist für diese Unterhaltung noch nicht vollständig eingerichtet
device-waiting-body = Du kannst lesen, was deine anderen Geräte lesen konnten. Neue Nachrichten bleiben eventuell gesperrt, bis eines deiner anderen Geräte oder ein Mitglied dieser Unterhaltung online ist.
device-waiting-notes-body = Deine Notizen öffnen sich hier, sobald eines deiner anderen Geräte online ist. Wenn du es nicht mehr hast, entferne es unter Einstellungen > Geräte: Dieses Gerät richtet deine Notizen dann von selbst neu ein. Frühere Notizen bleiben lesbar.
left-in-group-unknown = Jemand
group-refused = Der Server lässt dieses Gerät die verschlüsselte Gruppe dieser Unterhaltung nicht lesen. Neue Nachrichten fehlen eventuell.
never-set-up = { $name } hat TeamCraft nie eingerichtet. { $name } sieht diese Unterhaltung, falls sich { $name } später auf einem Gerät anmeldet.
waiting-first-device = { $name } hat TeamCraft noch nicht fertig eingerichtet. Du kannst trotzdem schon schreiben: { $name } sieht deine Nachrichten, sobald die Einrichtung abgeschlossen ist.
devices-unverified = { $name } kann nicht verschlüsselt hinzugefügt werden: Keines der Geräte ließ sich prüfen. Bitte { $name }, sich neu anzumelden; ein neues Gerät wird von selbst versucht.
waiting-key-package = { $name } kann noch nicht verschlüsselt hinzugefügt werden, weil keines der Geräte mehr Schlüssel auf dem Server hat. Das behebt sich, sobald eines der Geräte wieder online ist.
key-changed-title = Der Sicherheitsschlüssel von { $name } hat sich geändert
key-changed-body = Das passiert normalerweise nur nach einem vollständigen Zurücksetzen des Kontos. Frage { $name } über einen anderen Kanal, bevor du etwas Sensibles teilst.
key-verify = Schlüssel vergleichen
key-accept = Neuen Schlüssel akzeptieren
key-verify-title = Schlüssel von { $name } vergleichen
key-verify-body = Lies den neuen Schlüssel der Person laut vor, zum Beispiel in einem Anruf, und prüfe, ob er mit den eigenen Sicherheitseinstellungen übereinstimmt.
key-new = Neuer Schlüssel
key-old = Vorheriger Schlüssel
key-mark-verified = Als geprüft markieren

## Anmeldung
welcome-title = Willkommen, { $name }
welcome-subtitle = Das ist das erste Gerät deines Kontos. Zwei kurze Schritte schützen deine Nachrichten.
welcome-step-keys = Schlüssel werden auf diesem Gerät erzeugt
welcome-step-keys-hint = Deine Schlüssel verlassen deine Geräte nie. Der Server sieht nur verschlüsselte Daten.
welcome-step-passkey = Schlüsselsicherung mit einem Passkey schützen
welcome-step-passkey-hint = Mit einem Passkey (Face ID, Fingerabdruck oder Sicherheitsschlüssel) stellst du deine Schlüssel auf einem neuen Gerät wieder her.
welcome-create-passkey = Passkey erstellen
welcome-continue = Weiter
welcome-demo-note = Schlüsselerzeugung und Passkeys sind noch nicht angebunden; „Weiter“ öffnet nur die App.

## Suche
contact-verify-title = { $name } überprüfen
contact-verify-body = Vergleiche diese Nummer mit der Nummer, die { $name } in den eigenen Einstellungen sieht, zum Beispiel in einem Anruf. Sind beide identisch, sitzt niemand dazwischen.
contact-verified = Überprüft: Du hast diese Nummer verglichen.
contact-not-verified = Noch nicht überprüft.
contact-unmark = Überprüfung entfernen

## Einstellungen: Darstellung, Seitenleiste, Sprache, Datenschutz, Geräte, Anrufe
contact-verified-short = Überprüft

## Einrichtung der Schlüssel
enroll-title = Dieses Gerät einrichten, { $name }
enroll-new-subtitle = Erstelle deine Verschlüsselungsschlüssel. Nur du kannst deine Nachrichten lesen, nicht einmal der Server. Speichere zuerst dein Wiederherstellungs-Kit: Damit bekommst du deine Nachrichten zurück, falls du einmal alle deine Geräte verlierst.
enroll-recover-subtitle = Dein Konto hat bereits Verschlüsselungsschlüssel. Öffne dein Wiederherstellungs-Kit oder gib seinen Schlüssel ein, um sie auf diesem Gerät zu entsperren.
enroll-data-lost = Die Daten von TeamCraft auf diesem Gerät sind weg: Der Browser hat sie gelöscht, zum Beispiel um Speicherplatz freizugeben, oder sie konnten nicht gelesen werden und wurden beiseitegelegt. Entsperre deine Schlüssel wieder mit deinem Wiederherstellungs-Kit oder verbinde dieses Gerät von einem anderen Gerät aus.
enroll-device-name = Name dieses Geräts
enroll-create = Schlüssel erstellen
enroll-recover = Entsperren
enroll-working = Einen Moment …
enroll-failed = Die Schlüssel konnten nicht erstellt werden.
enroll-recover-failed = Der Wiederherstellungsschlüssel oder die Passphrase hat deine Schlüssel nicht entsperrt, oder der Server war nicht erreichbar.
enroll-secret-label = Wiederherstellungsschlüssel oder Passphrase
enroll-secret-hint = Der Schlüssel aus deinem Wiederherstellungs-Kit (8 Gruppen zu je 4 Zeichen) oder deine Passphrase, falls du vor den Wiederherstellungs-Kits eine festgelegt hast.
enroll-key-typo = Das sieht aus wie ein Wiederherstellungsschlüssel, aber ein Zeichen stimmt nicht. Vergleiche ihn mit deinem Wiederherstellungs-Kit.
enroll-kit-read = Der Wiederherstellungsschlüssel wurde aus der Datei gelesen.
enroll-restore-device = Auf diesem Computer wiederherstellen
enroll-restore-device-hint = Dieser Computer hat einen Schlüssel für dein Konto in seinem Schlüsselbund aufbewahrt. Dein Wiederherstellungs-Kit brauchst du hier nicht.

## Recovery kit (e2ee.md §6.5)
kit-setup-intro = TeamCraft hat einen Wiederherstellungsschlüssel für dich erstellt. Speichere das Wiederherstellungs-Kit: Drucke es aus oder bewahre es in deinem Passwort-Manager auf. Du brauchst es nur, wenn du alle deine Geräte verlierst.
kit-key-label = Wiederherstellungsschlüssel
kit-download = Wiederherstellungs-Kit herunterladen
kit-save-hint = Das Kit ist eine Datei mit deinem Schlüssel, einem QR-Code und einer Anleitung. Es entsteht auf diesem Gerät und wird nie an den Server geschickt.
kit-downloaded = Das Wiederherstellungs-Kit wurde heruntergeladen. Drucke es aus oder lege es in deinem Passwort-Manager ab und fahre dann fort.
kit-download-failed = Das Wiederherstellungs-Kit konnte nicht heruntergeladen werden. Schreibe stattdessen den Schlüssel auf und fahre dann fort.
kit-saved-continue = Ich habe mein Wiederherstellungs-Kit gespeichert
kit-confirm-intro = Damit sicher ist, dass du es aufbewahrt hast: Gib Gruppe { $number } des Schlüssels aus deinem gespeicherten Kit ein oder öffne die Datei.
kit-confirm-group = Gruppe { $number }
kit-confirm-wrong = Das ist nicht Gruppe { $number }. Sieh noch einmal in dein gespeichertes Wiederherstellungs-Kit.
kit-confirm-file-ok = Das ist dein neues Wiederherstellungs-Kit.
kit-confirm-file-other = Diese Datei enthält einen anderen Wiederherstellungsschlüssel als den gerade erstellten. Wähle das Kit, das du gerade heruntergeladen hast.
kit-confirm-back = Schlüssel noch einmal zeigen
kit-open = Wiederherstellungs-Kit öffnen …
kit-file-no-key = In dieser Datei wurde kein Wiederherstellungsschlüssel gefunden. Wähle das Wiederherstellungs-Kit, das TeamCraft für dich gespeichert hat.
kit-file-title = TeamCraft-Wiederherstellungs-Kit
kit-file-account = Konto: { $account } · Server: { $server } · Erstellt am { $date }
kit-file-intro = Das ist der Schlüssel zu deinen verschlüsselten Nachrichten. Wenn du alle deine Geräte verlierst, ist er der einzige Weg, deine Nachrichten wieder zu lesen.
kit-file-keep-title = Bewahre es sicher auf
kit-file-keep-print = Drucke diese Seite aus und bewahre sie an einem sicheren Ort auf, oder lege die Datei in deinem Passwort-Manager ab.
kit-file-keep-share = Schicke es nicht per E-Mail oder Chat und speichere es nicht in TeamCraft.
kit-file-keep-anyone = Wer diesen Schlüssel hat und sich als du anmelden kann, kann deine Nachrichten lesen.
kit-file-use-title = Wann du es brauchst
kit-file-use = Nur wenn du ein Gerät einrichtest und keines deiner anderen Geräte zur Hand ist: Melde dich bei TeamCraft an, wähle „Wiederherstellungs-Kit öffnen …“ und diese Datei, oder gib den Schlüssel ein. Ist ein anderes deiner Geräte zur Hand, verbinde das neue stattdessen von dort aus.
kit-file-lost-title = Wenn es verloren ist oder jemand anderes es gesehen hat
kit-file-lost = Erstelle in TeamCraft unter Einstellungen, Sicherheit & Datenschutz ein neues Wiederherstellungs-Kit. Dieses Kit funktioniert dann nicht mehr.
kit-file-never = TeamCraft und deine IT-Abteilung werden dich nie nach diesem Schlüssel fragen.

# ---- Meetings ----
enroll-link = Mit anderem Gerät verknüpfen
link-new-title = Dieses Gerät verknüpfen
link-new-steps = Öffne auf einem Gerät, auf dem du schon angemeldet bist, TeamCraft, dann Einstellungen, Geräte und Sicherheit, Neues Gerät verknüpfen, und scanne oder gib diesen Code dort ein. Die Kamera-App allein kann ihn nicht lesen.
link-new-compare = Gib auf dem anderen Gerät diesen Code ein, um zu bestätigen. Bestätige nicht, wenn du das nicht selbst gestartet hast.
link-new-failed = Das Verknüpfen hat nicht geklappt.
link-new-back = Zurück
link-new-retry = Erneut versuchen
approve-link-retry = Erneut versuchen
link-new-no-device = Kein anderes Gerät angemeldet? Geh zurück und nutze dein Wiederherstellungs-Kit oder bitte einen Admin um die Wiederherstellung durch die Organisation.
approve-link-title = Neues Gerät verknüpfen
approve-link-intro = Gib den Code ein, den das neue Gerät zeigt.
approve-link-code = Code des neuen Geräts
approve-link-hint = Der Code enthält einen Einmalschlüssel; gib ihn nur hier ein.
approve-link-connect = Verbinden
approve-link-compare = Prüfe, dass das neue Gerät denselben Code zeigt. Bestätige nur, wenn sie übereinstimmen.
approve-link-sending = Deine Schlüssel werden gesendet, das neue Gerät bestätigt den Empfang …
approve-link-failed = Das Gerät konnte nicht verknüpft werden.
recovery-title = Wiederherstellungs-Passphrase
recovery-hint = Stellt deine Schlüssel auf einem neuen Gerät wieder her. Beim Ändern wird dein Schlüssel-Backup neu geschützt; die alte Passphrase funktioniert nicht mehr.
recovery-new = Neue Wiederherstellungs-Passphrase
recovery-confirm = Passphrase wiederholen
recovery-length-hint = Mindestens { $count } Zeichen. Du brauchst sie, um deine Nachrichten auf einem neuen Gerät wiederherzustellen. Wenn du sie verlierst, können wir sie nicht für dich wiederherstellen.
recovery-save = Passphrase ändern
recovery-done = Die Wiederherstellungs-Passphrase wurde geändert. Schreibe sie an einem sicheren Ort auf.
recovery-failed = Die Passphrase konnte nicht geändert werden.
approve-link-type-code = Tippe den kurzen Code ein, den das neue Gerät zeigt. Zeigt es keinen oder hast du das nicht selbst gestartet, bestätige nicht.
approve-link-short = Kurzer Code vom neuen Gerät
approve-link-wrong = Das ist nicht der Code auf dem neuen Gerät. Versuche es noch einmal.

## Getting started (empty workspace)
device-kind-desktop = Desktop
device-kind-browser = Browser
passphrase-show = Anzeigen
passphrase-hide = Verbergen

## Key names in keyboard shortcuts (the caps shown on a keyboard)
link-unavailable = Zum Verknüpfen eines Geräts wird die Verschlüsselung dieser App benötigt, die hier nicht läuft. Lade die Seite neu oder nutze die Desktop-App.

## Failed actions (an error toast) and the panic overlay
error-link-code-invalid = Das ist kein TeamCraft-Verknüpfungscode. Scanne den Code, den das neue Gerät unter „Dieses Gerät verknüpfen“ zeigt.
error-link-other-version = Das andere Gerät hat eine andere TeamCraft-Version. Aktualisiere TeamCraft auf beiden Geräten und versuche es erneut.
error-link-peer-gone = Das neue Gerät wartet nicht mehr auf diesen Code. Lass dort einen neuen Code anzeigen und scanne ihn erneut.
error-link-timed-out = Das andere Gerät antwortet nicht mehr. Versuche es erneut.
error-link-code-expired = Der Code wurde nicht rechtzeitig verwendet. Versuche es erneut, um einen neuen Code zu bekommen.
error-link-denied = Ein Gerät deines Kontos hat die Anfrage abgelehnt.
error-link-not-confirmed = Die Schlüssel sind auf dem neuen Gerät nicht angekommen. Verknüpfe es erneut.
error-link-request-gone = Diese Anfrage ist nicht mehr offen: Ein anderes Gerät hat sie übernommen, oder sie ist abgelaufen.

## Settings: sections, search and rows (#327)
# Startseiten (Issue #585)
link-request-region = Anfragen, ein neues Gerät zu verknüpfen
link-request-title = Ein neues Gerät möchte sich mit deinem Konto verbinden
link-request-device = Gibt sich als { $device } ({ $platform }) aus. Nicht überprüft.
link-request-warning = Fahre nur fort, wenn du dieses Gerät gerade selbst einrichtest. Lehne die Anfrage sonst ab.
link-request-review = Ansehen
link-request-deny = Ablehnen
link-request-joining = Verbindung zum neuen Gerät wird aufgebaut …
link-request-type-code = Gib den Code ein, den das neue Gerät anzeigt. Wird keiner angezeigt, lehne die Anfrage ab.
link-platform-android = Android
link-platform-ios = iPhone oder iPad
link-platform-macos = Mac
link-platform-windows = Windows
link-platform-linux = Linux
link-platform-web = Webbrowser
link-platform-other = Unbekanntes System
link-new-ask = Meine anderen Geräte fragen
link-new-ask-hint = Statt zu scannen zeigen deine anderen Geräte eine Anfrage. Der Server sieht dann den Schlüssel in diesem Code; du gibst den hier angezeigten Code trotzdem auf dem anderen Gerät ein, ohne dich wird also nichts bestätigt.
link-new-asked = Deine anderen Geräte zeigen jetzt eine Anfrage.
link-new-ask-failed = Die anderen Geräte konnten nicht gefragt werden.
