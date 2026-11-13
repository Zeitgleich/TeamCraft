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
anchor-refused = Dieses Gerät kann diesen Kanal nicht öffnen: Der Server hat ihm eine verschlüsselte Gruppe angeboten, die deine Geräte für diesen Kanal nicht kennen. Neue Nachrichten bleiben hier unlesbar. Sag einem Workspace-Admin Bescheid.
waiting-for-admission = Du wirst zu diesem Kanal hinzugefügt. Die App eines Mitglieds, das online ist, fügt dich hinzu, meist innerhalb von Sekunden. Lesbar sind nur Nachrichten ab deinem Beitritt.
waiting-for-admission-no-member-online = Gerade ist kein Mitglied dieses Kanals online. Du wirst hinzugefügt, sobald eines online ist; diese App behält die Anfrage, auch nach einem Neustart. Lesbar sind nur Nachrichten ab deinem Beitritt.
waiting-for-admission-not-yet = Du wurdest noch nicht zu diesem Kanal hinzugefügt: Keine App eines Mitglieds hat dich hinzugefügt, vielleicht weil sie die Anfrage abgelehnt haben. Diese App fragt von selbst erneut. Lesbar sind nur Nachrichten ab deinem Beitritt.
import-in-progress = Der Verlauf wird importiert. Das Import-Konto { $name } kann dieses Gespräch lesen, bis der Import abgeschlossen ist.
read-by-outsider = { $name } gehört nicht zu diesem Gespräch, kann es aber lesen.
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
contact-attested = Die Anmeldung deiner Organisation hat bestätigt, dass dieser Schlüssel zu dieser Person gehört.
contact-not-attested = Die Anmeldung deiner Organisation hat diesen Schlüssel nicht bestätigt. Diese App hat ihn so übernommen, wie der Server ihn ausgegeben hat; vergleiche die Nummer, um sicherzugehen.

## Einstellungen: Darstellung, Seitenleiste, Sprache, Datenschutz, Geräte, Anrufe
contact-verified-short = Überprüft

## Einrichtung der Schlüssel
enroll-data-lost = Die Daten von TeamCraft auf diesem Gerät sind weg: Der Browser hat sie gelöscht, zum Beispiel um Speicherplatz freizugeben, oder sie konnten nicht gelesen werden und wurden beiseitegelegt. Entsperre deine Schlüssel wieder mit deinem Wiederherstellungs-Kit oder verbinde dieses Gerät von einem anderen Gerät aus.
enroll-device-name = Name dieses Geräts
enroll-recover = Entsperren
enroll-working = Einen Moment …
enroll-failed = Die Schlüssel konnten nicht erstellt werden.
enroll-recover-failed = Der Wiederherstellungsschlüssel oder die Passphrase hat deine Schlüssel nicht entsperrt, oder der Server war nicht erreichbar.
enroll-secret-label = Wiederherstellungsschlüssel oder Passphrase
enroll-secret-hint = Der Schlüssel aus deinem Wiederherstellungs-Kit (8 Gruppen zu je 4 Zeichen) oder deine Passphrase, falls du vor den Wiederherstellungs-Kits eine festgelegt hast.
enroll-key-typo = Das sieht aus wie ein Wiederherstellungsschlüssel, aber ein Zeichen stimmt nicht. Vergleiche ihn mit deinem Wiederherstellungs-Kit.
enroll-kit-read = Der Wiederherstellungsschlüssel wurde aus der Datei gelesen.
enroll-restore-device = Auf diesem Computer wiederherstellen

## Recovery kit (e2ee.md §6.5)
kit-setup-intro = TeamCraft hat einen Wiederherstellungsschlüssel für dich erstellt. Speichere das Wiederherstellungs-Kit: Drucke es aus oder bewahre es in deinem Passwort-Manager auf. Du brauchst es nur, wenn du alle deine Geräte verlierst.
kit-key-label = Wiederherstellungsschlüssel
kit-save-hint = Das Kit ist eine Datei mit deinem Schlüssel, einem QR-Code und einer Anleitung. Es entsteht auf diesem Gerät und wird nie an den Server geschickt.
kit-downloaded = Heruntergeladen. Du findest es in deinen Downloads.
kit-download-failed = Das Wiederherstellungs-Kit konnte nicht heruntergeladen werden. Schreibe stattdessen den Schlüssel auf und fahre dann fort.
kit-confirm-intro-short = Damit sicher ist, dass du es aufbewahrt hast, gib unten die gefragte Gruppe aus deinem gespeicherten Kit ein oder öffne die Datei.
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
link-new-title = Dieses Gerät verknüpfen
link-new-compare = Gib auf dem anderen Gerät diesen Code ein, um zu bestätigen. Bestätige nicht, wenn du das nicht selbst gestartet hast.
link-new-failed = Das Verknüpfen hat nicht geklappt.
link-new-retry = Erneut versuchen
approve-link-retry = Erneut versuchen
link-new-no-device = Kein anderes Gerät angemeldet? Geh zurück und nutze dein Wiederherstellungs-Kit oder bitte einen Admin um die Wiederherstellung durch die Organisation.
approve-link-title = Neues Gerät verknüpfen
approve-link-intro = Gib den Code ein, den der neue Computer zeigt.
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
approve-link-phone-intro = Richtest du ein neues Handy ein? Wähle in TeamCraft auf dem Handy „Mit anderem Gerät verknüpfen“, dann „Code scannen“, und richte es auf diesen Code.
link-code-valid = Dieser Code gilt noch { $time }.
approve-link-phone-expired = Der Code ist abgelaufen, bevor ein Handy ihn gescannt hat.
approve-link-phone-new-code = Neuen Code zeigen
approve-link-phone-other-ways = Das neue Handy kann auch um Bestätigung bitten: Dieses Gerät zeigt dann eine Anfrage.
approve-link-enter-instead = Das neue Gerät ist ein Computer: seinen Code eingeben
approve-link-show-instead = Stattdessen einen Code für ein Handy zeigen
error-link-for-new-device = Dieser Code ist für das neue Handy: Scanne ihn dort mit „Code scannen“.
error-link-for-signed-in-device = Dieser Code kommt von einem neuen Gerät: Scanne ihn mit einem Gerät, auf dem du schon angemeldet bist.

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

## Guided first run on a device (#1701)
kit-download-again = Noch einmal herunterladen
kit-wrote-down = Download klappt nicht? Ich habe den Schlüssel auf Papier notiert
kit-check-missing = Gib Gruppe { $number } aus deinem gespeicherten Kit ein oder öffne die Kit-Datei.
kit-check-group-ok = Stimmt.
kit-check-or = oder
enroll-secret-missing = Öffne deine Kit-Datei oder gib den Schlüssel daraus ein.
onboarding-step = Schritt { $current } von { $total }
onboarding-back = Zurück
onboarding-continue = Weiter
onboarding-start = Los geht's
onboarding-recommended = Empfohlen
onboarding-intro-title = Schön, dass du da bist, { $name }
onboarding-intro-lead = Bevor es losgeht, schützt TeamCraft deine Nachrichten auf diesem Gerät. Das dauert etwa zwei Minuten.
onboarding-intro-locked = Deine Nachrichten sind verschlossen
onboarding-intro-locked-text = Nur deine eigenen Geräte können sie lesen. Nicht der Server, nicht deine IT-Abteilung.
onboarding-intro-kit = Du bekommst ein Wiederherstellungs-Kit
onboarding-intro-kit-text = Eine Datei mit deinem persönlichen Schlüssel. Sie ist der Ersatzschlüssel zu deinen Nachrichten, wie der Ersatzschlüssel für deine Wohnung.
onboarding-intro-keep = Bewahre es gut auf, verliere es nicht
onboarding-intro-keep-text = Wenn du einmal alle deine Geräte verlierst, ist das Kit der einzige Weg zurück zu deinen Nachrichten. Niemand kann dir ein neues machen.
onboarding-save-title = Speichere dein Wiederherstellungs-Kit
onboarding-save-lead = Lade das Kit herunter und bewahre es dort auf, wo du wichtige Unterlagen oder Passwörter aufbewahrst: Drucke es aus oder lege es in deinen Passwort-Manager.
onboarding-save-new-key = Du hattest schon einmal angefangen. Zu deiner Sicherheit ist das ein neuer Schlüssel: Ein vorher gespeichertes Kit funktioniert nicht. Lösche es und speichere dieses.
onboarding-check-title = Prüfe dein Wiederherstellungs-Kit
onboarding-check-lead = Stell sicher, dass du es wirklich hast: Schau in dein gespeichertes Kit und gib unten die gefragte Gruppe ein oder öffne die Datei.
onboarding-finish-title = Bewahre dein Kit sicher auf
onboarding-finish-lead = Fast geschafft. Merk dir diese drei Dinge, dann schließ die Einrichtung ab.
onboarding-finish-where = Bewahre es sicher auf
onboarding-finish-where-text = Bei deinen wichtigen Unterlagen oder in deinem Passwort-Manager. Nicht nur auf diesem Gerät.
onboarding-finish-share = Verschicke es nie
onboarding-finish-share-text = Nicht per E-Mail, nicht im Chat, auch nicht in TeamCraft. Wer es hat, kann deine Nachrichten lesen.
onboarding-finish-ask = Niemand wird danach fragen
onboarding-finish-ask-text = Nicht TeamCraft, nicht deine IT-Abteilung. Wer danach fragt, will dich täuschen.
onboarding-finish = Einrichtung abschließen
onboarding-device-hint = Diesen Namen siehst du später in deiner Geräteliste.
onboarding-unlock-title = Entsperre deine Nachrichten hier
onboarding-unlock-lead = Dein Konto hat schon einen Schlüssel. Damit du deine Nachrichten auf diesem Gerät lesen kannst, bring den Schlüssel hierher. Wähle wie:
onboarding-way-device-text = Dieser Computer hat deinen Schlüssel aufbewahrt. Ein Klick, und du bist fertig.
onboarding-way-link = Mit einem anderen Gerät
onboarding-way-link-text = Bestätige dieses Gerät auf deinem Handy oder einem anderen Computer, auf dem du angemeldet bist. Der schnellste Weg.
onboarding-way-kit = Mit meinem Wiederherstellungs-Kit
onboarding-way-kit-text = Öffne die Kit-Datei oder gib den Schlüssel daraus ein. Für den Fall, dass kein anderes Gerät zur Hand ist.
onboarding-kit-title = Mit dem Wiederherstellungs-Kit entsperren
onboarding-kit-lead = Öffne die Kit-Datei, die du beim Einrichten deines ersten Geräts gespeichert hast. Du kannst auch den darin gedruckten Schlüssel eingeben.
onboarding-link-open = Öffne TeamCraft auf einem Gerät, auf dem du angemeldet bist.
onboarding-link-scan = Geh zu Einstellungen, Geräte, Gerät verknüpfen, und scanne diesen Code (am Computer gibst du ihn ein).
onboarding-link-compare = Vergleiche die Zahl, die beide Geräte zeigen, und bestätige.
onboarding-link-no-camera = Scannen geht nicht? Deine anderen Geräte können stattdessen eine Abfrage zeigen.
onboarding-way-it = Meine IT-Abteilung fragen
onboarding-way-it-text = Kein anderes Gerät und kein Kit? Deine IT-Abteilung hilft dir.
onboarding-it-title = Frag deine IT-Abteilung
onboarding-it-lead = Sag deiner IT-Abteilung, dass du alle deine Geräte und dein Wiederherstellungs-Kit verloren hast. Sie kann dein Konto zurücksetzen: Du startest dann mit neuen Schlüsseln, und deine alten Nachrichten sind verloren.
org-recovery-restarted = Dieses Gerät hat deine frühere Anfrage verloren, der damals gezeigte Code gilt nicht mehr. Frag erneut nach einem neuen Code. Warst du schon beim Abschließen und es ist fehlgeschlagen, versuch „Mit meinem Wiederherstellungs-Kit“ und dem damals gespeicherten Kit.
org-recovery-point-decide = Deine IT-Abteilung entscheidet
org-recovery-point-decide-text = Niemand kann deine Nachrichten allein zurückholen: Admins deiner Organisation müssen zustimmen.
org-recovery-point-code = Lies ihnen einen Code vor
org-recovery-point-code-text = Hat deine Organisation die Wiederherstellung eingerichtet, zeigt dieses Gerät einen Code. Lies ihn einem Admin am Telefon oder persönlich vor.
org-recovery-point-kit = Du bekommst ein neues Wiederherstellungs-Kit
org-recovery-point-kit-text = Die Admins haben deinen alten Schlüssel gesehen, darum bekommt dein Konto einen neuen. Speichere ihn, während du wartest.
org-recovery-show-code = Code für meine IT-Abteilung zeigen
org-recovery-start-failed = Die Anfrage konnte nicht gesendet werden. Versuch es noch einmal.
org-recovery-code-title = Lies deiner IT-Abteilung diesen Code vor
org-recovery-code-lead = Ruf bei einem Admin deiner Organisation an oder geh persönlich vorbei und lies diesen Code vor. Die Admins vergleichen ihn, bevor sie zustimmen.
org-recovery-code-label = Code für deine IT-Abteilung
org-recovery-code-next = Speichere inzwischen ein neues Wiederherstellungs-Kit. Sobald die Admins zustimmen, schließt du hier ab.
org-recovery-withdraw = Anfrage zurückziehen
org-recovery-unavailable-title = Deine IT-Abteilung kann dein Konto zurücksetzen
org-recovery-unavailable = Deine Organisation hat die Wiederherstellung für dich nicht eingerichtet. Deine IT-Abteilung kann dein Konto nur zurücksetzen: Du startest dann mit neuen Schlüsseln, und deine alten Nachrichten sind verloren.
org-recovery-sent = Anfrage gesendet. Warte auf die Admins.
org-recovery-waiting = { $approvals } von { $count ->
    [one] { $count } Admin
   *[other] { $count } Admins
  } haben zugestimmt.
org-recovery-approved = Zugestimmt. Das Gerät eines Admins entsperrt deinen Schlüssel.
org-recovery-ready = Zugestimmt. Du kannst jetzt abschließen.
org-recovery-offline = Der Server ist gerade nicht erreichbar. Neuer Versuch läuft.
org-recovery-denied = Deine IT-Abteilung hat die Anfrage abgelehnt. Sprich mit ihr und frag erneut, wenn sie zustimmt.
org-recovery-expired = Die Anfrage ist abgelaufen, bevor genug Admins zugestimmt haben. Frag erneut nach einem neuen Code.
org-recovery-cancelled = Die Anfrage wurde zurückgezogen. Frag erneut nach einem neuen Code.
org-recovery-lost = Dieses Gerät hat die Anfrage verloren (die App wurde neu gestartet). Frag erneut nach einem neuen Code.
org-recovery-ask-again = Erneut fragen
org-recovery-save-lead = Dein Konto bekommt ein neues Wiederherstellungs-Kit, denn die Admins haben deinen alten Schlüssel gesehen. Lade es herunter und bewahre es dort auf, wo du wichtige Unterlagen oder Passwörter aufbewahrst.
org-recovery-finish-title = Wiederherstellung abschließen
org-recovery-finish-lead = Sobald die Admins zugestimmt haben, schließt du hier ab. Bewahre dein neues Kit dann sicher auf: Schick es nie weiter, und niemand wird dich danach fragen.
org-recovery-finish = Wiederherstellung abschließen
org-recovery-finish-failed = Die Wiederherstellung konnte nicht abgeschlossen werden. Prüf deine Verbindung und versuch es noch einmal.
kit-download-short = Kit herunterladen
