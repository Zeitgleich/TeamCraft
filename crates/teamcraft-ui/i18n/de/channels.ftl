# TeamCraft UI – Deutsch
# Spaces and channels: settings, members, joining, browsing, integrations and their log.

## Kompaktes Layout (Telefone): untere Navigation, zurück, Menüs; Anrufe im Browser
conversation-more = Mehr
conversation-bell = Benachrichtigungen für diese Unterhaltung
conversation-search = In Unterhaltung suchen ({ $shortcut })
conversation-mute = Unterhaltung stummschalten
conversation-unmute = Stummschaltung aufheben

## Kanal
channel-region-label = Kanal { $name }
channel-empty = Sag Hallo und bring die Unterhaltung in Gang.
channel-empty-title = Noch keine Nachrichten
channel-tabs = Ansichten der Unterhaltung
channel-tab-messages = Nachrichten
channel-tab-files = Dateien & Links
members-button-label = { $count ->
    [one] Mitglieder anzeigen, { $count } Mitglied
   *[other] Mitglieder anzeigen, { $count } Mitglieder
  }
members-title = Mitglieder
members-panel-label = Kanalmitglieder
members-count = { $count ->
    [one] { $count } Mitglied
   *[other] { $count } Mitglieder
  }
bookmarks-label = Lesezeichen des Kanals
bookmarks-add = Lesezeichen hinzufügen
bookmarks-add-soon = Lesezeichen können noch nicht bearbeitet werden

## Nachrichten
system-joined = { $name } ist dem Kanal beigetreten.
system-left = { $name } hat den Kanal verlassen.
system-topic-changed = { $name } hat das Thema auf „{ $topic }“ geändert.
system-renamed = { $name } hat den Kanal in { $channel } umbenannt.
log-level-debug = DEBUG
log-level-info = INFO
log-level-warn = WARN
log-level-error = ERROR

## Administration
retention-days = Tage
retention-messages = Nachrichten
retention-pinned = Angeheftete Nachrichten behalten
retention-actions = Aktionen
retention-forever = Alles behalten
retention-invalid = Gib eine ganze Zahl von 1 bis zum Limit der Instanz ein oder lass das Feld leer.

## Geräte-Freigabe, Verknüpfung und Schlüsseländerungen
leave-pending = Du verlässt diese Unterhaltung. Das ist abgeschlossen, sobald das Gerät eines Mitglieds dich aus der verschlüsselten Gruppe entfernt hat.
leave-failed = Das Verlassen dieser Unterhaltung ist fehlgeschlagen: Der Server hat es abgelehnt.
removal-pending = { $name } wird aus dieser Unterhaltung entfernt.
removal-failed = { $name } konnte nicht entfernt werden: Der Server hat es abgelehnt.
removal-retry = Erneut versuchen
removal-unconfirmed = Deine Entfernung von { $name } aus #{ $channel } ist nicht bestätigt: Der Server listet die Person wieder, oder jemand hat sie eventuell wieder hinzugefügt.
removal-unconfirmed-remove = Erneut entfernen
removal-unconfirmed-keep = Behalten

## Einrichtung der Schlüssel
# ---- Meetings ----
external-badge = Externe Personen
external-badge-detail = In dieser Unterhaltung sind Personen von außerhalb deiner Organisation.

# Workspace management
space-new-title = Neuer Space
space-name = Name
space-description = Beschreibung
space-color = Farbe
space-color-reset = Standardfarbe
space-image = Bild
space-image-hint = Wird in der Space-Leiste statt der Initialen angezeigt. PNG, JPEG oder WebP; den quadratischen Ausschnitt wählst du nach der Auswahl.
space-image-upload = Bild hochladen
space-image-change = Bild ändern
space-image-remove = Bild entfernen
space-image-uploading = Bild wird gespeichert …
space-image-size = Das Bild ist zu groß.
space-image-unreadable = Das Bild konnte nicht gelesen werden.
space-image-forbidden = Du darfst das Bild nicht ändern.
space-image-failed = Das Bild konnte nicht gespeichert werden. Bitte versuche es noch einmal.
space-image-offline = Keine Verbindung zum Server. Versuche es noch einmal, sobald du online bist.
space-image-demo = Die Demo speichert keine Bilder; mit einem Server wäre dieses jetzt gespeichert.
channel-image = Bild
channel-image-hint = Wird in den Einstellungen dieses Kanals angezeigt. PNG, JPEG oder WebP; den quadratischen Ausschnitt wählst du nach der Auswahl.
space-open = Alle im Workspace können beitreten
space-settings-title = Space-Einstellungen
space-categories = Kategorien
space-category-new = Neue Kategorie
space-category-rename = Kategoriename
space-category-delete = Kategorie löschen
space-archive-action = Diesen Space archivieren
space-settings-read-only = Nur Space-Admins können das ändern.
space-join-policy = Wer beitreten kann
space-join-open = Offen
space-join-open-hint = Alle in der Organisation können den Space finden und beitreten.
space-join-invite-only = Nur auf Einladung
space-join-invite-only-hint = Nur hinzugefügte oder eingeladene Personen sehen den Space.
space-members-filter = Mitglied suchen
space-members-read-only = Space-Admins fügen Mitglieder hinzu, entfernen sie und ändern Rollen.
space-members-loading = Mitglieder werden geladen…
space-members-hidden = Nur Mitglieder des Space sehen, wer darin ist.
space-invites-pending = Offene Einladungen
space-invite-revoke = Zurückziehen
space-role = Rolle
space-role-member = Mitglied
space-role-admin = Space-Admin
space-role-guest = Gast
space-member-remove-confirm = { $name } aus dem Space entfernen? Die Person verlässt auch seine Kanäle und kann dort keine neuen Nachrichten mehr lesen.
space-member-integration = Integration
space-member-integration-hint = Das Bot-Konto einer Integration. Hinzufügen und entfernen in den Kanaleinstellungen unter Integrationen.
space-categories-hint = Kategorien gruppieren die Kanäle in der Seitenleiste für alle im Space.
space-channels = Kanäle
space-archive-hint = Ein archivierter Space und seine Kanäle werden schreibgeschützt und verschwinden aus der Seitenleiste. Mitglieder, Verlauf und Verschlüsselung bleiben unverändert.
space-archive-confirm = Diesen Space für alle archivieren?
channel-new-title = Neuer Kanal
channel-name = Name
channel-topic = Thema
channel-private = Privat (nur auf Einladung)
channel-category = Kategorie
channel-uncategorized = Keine Kategorie
channel-history = Was neue Mitglieder lesen können
history-default = Standard des Workspace
history-from-join = Nur Nachrichten ab dem Beitritt
history-full = Den gesamten Verlauf
channel-settings-title = Kanaleinstellungen
channel-missing = Dieser Kanal ist nicht verfügbar.
channel-move-category = In Kategorie verschieben
channel-privacy = Datenschutz
channel-privacy-hint-public = Alle im Space können diesen Kanal finden und beitreten.
channel-privacy-hint-private = Nur eingeladene Personen sehen diesen Kanal.
channel-make-private = Privat machen
channel-make-public = Öffentlich machen
channel-make-private-confirm = Diesen Kanal privat machen? Personen, die nicht Mitglied sind, verlieren den Zugriff.
channel-make-public-confirm = Diesen Kanal öffentlich machen? Alle im Space können beitreten und neue Nachrichten lesen.
channel-archive = Archivieren
channel-archive-action = Diesen Kanal archivieren
channel-roles-hint = Admins verwalten Mitglieder, Rollen und Einstellungen. Moderatoren entfernen Mitglieder und löschen Nachrichten.
channel-roles-read-only = Nur Kanal-Admins ändern Rollen.
channel-settings-read-only = Nur Kanal-Admins können das ändern.
channel-role = Rolle
channel-role-admin = Kanal-Admin
channel-role-moderator = Moderator
channel-role-member = Mitglied
channel-role-unsigned = Nicht signiert
channel-role-unsigned-hint = Nur der Server führt diese Rolle: Kein Admin hat sie bisher signiert, daher zählen die Apps der Mitglieder sie nicht, wo eine signierte Rolle nötig ist.
channel-role-sign = Rolle signieren
role-change-confirm = Rolle von { $name } auf „{ $role }“ ändern?
channel-archive-hint = Archivierte Kanäle sind schreibgeschützt und verschwinden aus der Seitenleiste. Admins können sie wiederherstellen.
channel-archive-confirm = Diesen Kanal für alle archivieren?
channel-add-people = Personen hinzufügen
channel-pick-person = Person auswählen
channel-share-history = Bisherigen Verlauf mit ihnen teilen
channel-share-history-on = Sie können alles lesen, was bisher gepostet wurde.
channel-share-history-off = Sie sehen nur Nachrichten ab dem Beitritt.
channel-add = Hinzufügen
channel-add-chosen = Hinzuzufügende Personen
channel-already-member = Schon im Kanal
channel-add-joins-space = Wer noch nicht im Space ist, tritt ihm zuerst als Mitglied bei.
channel-add-space-only = Nur Personen im Space können hinzugefügt werden. Space-Admins holen andere dazu:
browse-title = Kanäle durchsuchen
browse-filter = Kanal suchen
browse-archived = Archivierte Kanäle anzeigen
browse-empty = Keine Kanäle gefunden.
browse-open = Öffnen
browse-join = Beitreten
browse-join-admin = Als Admin beitreten
browse-join-admin-hint = Ein privater Kanal: du trittst als Workspace-Admin bei, und seine Mitglieder sehen, dass du beigetreten bist.
browse-join-ask = Beitritt anfragen
browse-join-ask-hint = Ein privater Kanal: Seine Eigentümer entscheiden, ob du als Workspace-Admin beitrittst, und seine Mitglieder sehen, wenn du es tust.
browse-join-requested = Angefragt
browse-join-requested-hint = Die Eigentümer des Kanals entscheiden. Die Anfrage läuft ab, wenn niemand entscheidet.
browse-join-withdraw = Zurückziehen
browse-leave = Verlassen
browse-preview = Vorschau
invite-title = Personen einladen
invite-role = Einladen als
invite-role-member = Mitglied: sieht die öffentlichen Kanäle des Space
invite-role-guest = Gast: Zugriff nur auf hinzugefügte Kanäle
invite-guests-only = Du kannst Gäste in diesen Space einladen. Mitglieder fügen Space-Admins hinzu.
invite-link-only = Nur Space-Admins können Personen zu diesem Space hinzufügen. Teile den Link unten: Alle in deiner Organisation können diesem offenen Space damit beitreten.
invite-from-directory = Personen in deiner Organisation
invite-chosen = Einzuladende Personen
invite-nobody-found = Niemand gefunden. Wer sich noch nie angemeldet hat, kann per E-Mail eingeladen werden.
invite-by-email = Per E-Mail einladen
invite-email-placeholder = name@example.com, …
invite-email-hint = Für Personen ohne Konto. Sie erhalten eine E-Mail und treten dem Space bei der ersten Anmeldung bei.
invite-email-invalid = Bitte prüfe diese Adressen: { $addresses }
invite-email-too-many = Höchstens { $max } Adressen auf einmal.
invite-message = Persönliche Nachricht (optional)
invite-message-hint = Wird den Einladungs-E-Mails hinzugefügt.
invite-done = { $count ->
    [one] { $count } Person eingeladen.
   *[other] { $count } Personen eingeladen.
  }
invite-link = Space-Link
invite-link-copy = Link kopieren
invite-link-copied = Kopiert
invite-link-hint = Alle in deiner Organisation, die diesen Link öffnen, können diesem offenen Space beitreten.
space-missing = Dieser Space ist nicht verfügbar.
invite-send = Einladung senden

# Workspace system messages
ws-member-added = { $by } hat { $name } hinzugefügt
ws-member-added-history = { $by } hat { $name } hinzugefügt und den bisherigen Verlauf geteilt
ws-member-removed = { $by } hat { $name } entfernt
ws-archived = { $by } hat diesen Kanal archiviert
ws-unarchived = { $by } hat diesen Kanal wiederhergestellt
ws-made-private = { $by } hat diesen Kanal privat gemacht
ws-made-public = { $by } hat diesen Kanal öffentlich gemacht

# Der Server meldet die Änderung; keine Ankündigung eines Mitglieds nennt, wer sie gemacht hat (Issue #818).
ws-archived-unattributed = Dieser Kanal wurde archiviert
ws-unarchived-unattributed = Dieser Kanal wurde wiederhergestellt
ws-made-private-unattributed = Dieser Kanal wurde privat gemacht
ws-made-public-unattributed = Dieser Kanal wurde öffentlich gemacht
ws-pinned = { $by } hat eine Nachricht angeheftet
ws-admin-joined = { $name } ist diesem Kanal als Workspace-Admin beigetreten
ws-admin-joined-approved = { $name } ist diesem Kanal als Workspace-Admin beigetreten, zugelassen von { $by }
ws-admin-joined-added = { $name } ist diesem Kanal als Workspace-Admin beigetreten, hinzugefügt von { $by }
log-toolbar-label = Optionen der Protokollansicht
log-mode-log = Protokoll
log-mode-chat = Chat
log-filter-level = Mindeststufe
log-filter-text = Protokolltext filtern
log-follow = Mitlaufen
log-filter-none = Keine Protokolleinträge passen zum Filter.
log-filter-truncated = Die neuesten { $count } passenden Einträge werden angezeigt.

# Bookmarks
bookmarks-edit = Lesezeichen bearbeiten
bookmarks-edit-title = Lesezeichen
bookmarks-emoji = Emoji
bookmarks-title = Titel
bookmarks-url = Link
bookmarks-up = Nach oben
bookmarks-down = Nach unten
bookmarks-remove = Lesezeichen entfernen
bookmarks-invalid = Jedes Lesezeichen braucht einen Titel, und Links müssen mit https:// oder http:// beginnen.

## Aktivität, zuletzt online und Anrufliste
join-requests-title = Beitrittsanfragen
join-request-summary = { $name } möchte { $channel } als Workspace-Admin beitreten
join-request-summary-here = { $name } möchte als Workspace-Admin beitreten
join-request-hint = Wenn du zustimmst, kann die Person lesen, was ab jetzt geschrieben wird, wie bei einem Beitritt aus eigener Kraft. Alle im Kanal sehen, dass ein Workspace-Admin beigetreten ist.
join-request-hint-history = Wenn du zustimmst, kann die Person auch den bisherigen Verlauf lesen. Alle im Kanal sehen, dass ein Workspace-Admin beigetreten ist.
join-request-share-history = Auch den bisherigen Verlauf teilen
join-request-not-admin = Die signierte Rollenliste auf diesem Gerät führt die Person nicht als Workspace-Admin, du kannst nur ablehnen.
join-request-approve = Zustimmen
join-request-decline = Ablehnen

# Bereiche: Kanäle verschieben, stummschalten, Suche im Bereich, gruppierte Seitenleiste, Aufbewahrung
ws-moved = { $by } hat diesen Kanal in { $space } verschoben
ws-moved-unnamed = { $by } hat diesen Kanal in einen anderen Bereich verschoben
ws-moved-unattributed = Dieser Kanal wurde in { $space } verschoben
ws-moved-unnamed-unattributed = Dieser Kanal wurde in einen anderen Bereich verschoben
channel-move-space = In einen anderen Bereich verschieben
channel-move-space-hint = Mitglieder, Verlauf und Verschlüsselung bleiben unverändert. Nur der Bereich ändert sich, in dem der Kanal gelistet ist.
channel-move-space-choose = Bereich auswählen…
channel-move-space-action = Kanal verschieben
channel-move-space-confirm = Diesen Kanal verschieben? Alle im Kanal müssen schon Mitglied des neuen Bereichs sein; danach verwalten ihn dessen Admins.
retention-channel-title = Aufbewahrung von Nachrichten
retention-channel-hint = Ältere Nachrichten oder solche über dieser Anzahl werden für alle gelöscht. Lass beide Felder leer, um die Vorgaben des Bereichs und des Arbeitsbereichs zu übernehmen.
retention-channel-effective = Aktuell gilt: { $value }.
retention-channel-limit = Die Administratoren erlauben höchstens { $value }.
retention-channel-saved = Aufbewahrung gespeichert.
retention-unlimited = alles behalten
retention-summary-days = { $days } Tage
retention-summary-messages = die letzten { $count } Nachrichten
retention-summary-both = { $days } Tage oder die letzten { $count } Nachrichten, je nachdem, was strenger ist
retention-error-number = Bitte nur ganze Zahlen eingeben.
retention-error-positive = Werte müssen größer als null sein.
retention-error-refused = Der Server hat diese Einstellung abgelehnt. Sie überschreitet vielleicht die Grenze der Administratoren, oder du darfst diesen Kanal nicht verwalten.
space-notifications = Benachrichtigungen
space-mute-label = Diesen Bereich stummschalten
space-mute-hint = Seine Kanäle benachrichtigen nicht mehr, außer du legst für einen Kanal selbst eine Stufe fest.
space-mute = { $name } stummschalten
space-unmute = Stummschaltung von { $name } aufheben

## Daily-use improvements (docs/ux-backlog.md)
channel-left-toast = Du hast { $name } verlassen

## Getting started (empty workspace)
space-join = Beitreten
spaces-browse-title = Spaces durchsuchen
spaces-browse-filter = Space suchen
spaces-browse-open = Offene Spaces zum Beitreten
spaces-browse-open-none = Es gibt keine offenen Spaces zum Beitreten.
spaces-browse-mine = Deine Spaces
spaces-browse-mine-none = Du bist noch in keinem Space.
spaces-browse-invite-only-hint = Spaces, die nur auf Einladung offen sind, erscheinen hier, sobald dich jemand einlädt.

## Failed actions (an error toast) and the panic overlay
error-admin-join-request-gone = Diese Anfrage ist nicht mehr offen: Sie wurde auf einem anderen Gerät oder von einem anderen Eigentümer entschieden, zurückgezogen oder ist abgelaufen.
error-admin-join-request-too-soon = Du hast kürzlich angefragt, diesem Kanal beizutreten, und die Anfrage wurde abgelehnt oder zurückgezogen. Du kannst erneut anfragen, sobald sie abgelaufen wäre.

## Settings: sections, search and rows (#327)
# Dateien-Ansicht und Einrichtungs-Checkliste (Issue #301)
channel-kind = Kanaltyp
channel-mode = Modus
channel-mode-normal = Normal
channel-mode-log = Log-Kanal
channel-mode-normal-hint = Ein normaler Kanal für Unterhaltung. Bots können hier ebenfalls schreiben.
channel-mode-log-hint = Maschinelle Ausgabe: als Log dargestellt, nach der Log-Aufbewahrung gelöscht, Push nur bei Erwähnungen.
channel-mode-public-hint = Der Kanal muss privat sein, um ihn als Log-Kanal zu nutzen.
channel-bots = Bots in diesem Kanal
channel-bots-none = Noch keine Bots.
channel-bots-add = Bot hinzufügen
channel-bots-search = Bots nach Namen suchen
channel-bots-hint = Ein Bot liest und schreibt wie ein Mitglied, sieht aber nur, was nach seinem Beitritt geschrieben wird. Bots legen Admins in der Admin-Konsole an.
channel-bots-add-button = Hinzufügen
channel-bots-remove = Entfernen
channel-bots-remove-confirm = Bot entfernen
channel-sources = Quellen
channel-sources-none = Noch keine Quellen.
channel-sources-added-by = Hinzugefügt von { $name } am { $date }
channel-sources-revoke = Widerrufen
channel-sources-revoke-confirm = Quelle widerrufen
channel-sources-name = Name der Quelle
channel-sources-name-placeholder = z. B. ci oder nas-logs
channel-sources-kind = Art
channel-sources-kind-webhook = Webhook
channel-sources-kind-log = Logzeilen
channel-sources-kind-other = Andere
channel-sources-danger-alert = Als „danger“ markierte Anhänge erwähnen Personen
channel-sources-add = Quelle hinzufügen
channel-sources-hint = Grafana, CI oder journalctl auf die Bridge richten. Eine neue Quelle erfährt die Bridge verschlüsselt über diesen Kanal; nichts muss konfiguriert oder neu gestartet werden.
channel-sources-needs-bot = Zuerst einen Bot zu diesem Kanal hinzufügen: Die Bridge erfährt von Quellen über den Kanal.
channel-sources-admins-only = Quellen können vorerst nur Workspace-Admins hinzufügen: Die Bridge vertraut ihrer signierten Rolle und liest Kanalrollen noch nicht.
channel-sources-ask-admins = Quellen können vorerst nur Workspace-Admins hinzufügen: Die Bridge vertraut ihrer signierten Rolle und liest Kanalrollen noch nicht. Frag { $admins }.
channel-sources-no-random = Kein Zufallsgenerator verfügbar, daher konnte kein Token erzeugt werden. Einen anderen Browser versuchen.
channel-sources-created = { $name } ist bereit. Die Adresse jetzt kopieren: Sie wird nur einmal angezeigt.
channel-sources-url = Adresse
channel-sources-example = Beispiel
channel-sources-shown-once = Die Adresse enthält das geheime Token der Quelle. Wer sie hat, kann in diesen Kanal schreiben, also vertraulich behandeln. Zum Ersetzen die Quelle widerrufen und eine neue hinzufügen.
channel-sources-done = Fertig
channel-sources-bridge = Wo die Bridge läuft
channel-sources-bridge-instance = Auf diesem Server
channel-sources-bridge-loopback = Auf eigenem Rechner
channel-sources-bridge-instance-hint = Die Bridge läuft neben dem Server (Deployment-Option teamcraft_bridge); Quellen senden an die Adresse dieses Servers.
channel-sources-bridge-loopback-hint = Die Bridge läuft neben den Quellen und lauscht auf ihrer lokalen Standardadresse. Wurde ihr Listener geöffnet, die eigene Adresse der Bridge verwenden.
