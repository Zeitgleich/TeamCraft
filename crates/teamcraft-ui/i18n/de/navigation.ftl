# TeamCraft UI – Deutsch
# The rail, sidebar and bottom navigation, the account menu, home pages, activity and getting started.

## Kopfleiste und Kontomenü
account-menu-label = Kontomenü für { $name }, Status { $status }
menu-settings = Einstellungen
menu-log-out = Abmelden

## Space-Leiste
rail-label = Spaces
rail-add-space = Spaces durchsuchen oder anlegen
rail-space-mentions = { $name }, { $count ->
    [one] { $count } ungelesene Erwähnung
   *[other] { $count } ungelesene Erwähnungen
  }
rail-space-unread = { $name }, ungelesene Nachrichten
rail-chats-unread = Chats, { $count } ungelesen
rail-chats = Chats
rail-activity = Aktivität
rail-activity-tip = Erwähnungen, Antworten in Threads, denen du folgst, und Reaktionen auf deine Nachrichten
rail-activity-unread = Aktivität, { $count } ungelesen
rail-activity-mentions = Aktivität, { $count } ungelesen, auch Erwähnungen
chats-title = Chats
chats-filter = Chats filtern
home-filters = Anzeigen
home-filter-all = Alle
home-filter-unread = Ungelesen
home-filter-people = Personen
home-filter-groups = Gruppen
dm-new = Neue Nachricht
dm-options = Listenoptionen
dm-filter-empty = Keine Direktnachricht passt.
dm-sort = Sortierung
dm-sort-recent = Neueste zuerst
dm-sort-alphabetical = Alphabetisch
dm-item-unread = { $name }, { $count } ungelesen
dm-empty = Direktnachrichten mit einer oder mehreren Personen erscheinen hier, außerhalb deiner Spaces.
dm-home-hint = Wähle links eine Unterhaltung oder schreibe einer oder mehreren Personen.
dm-new-chosen = Empfänger
dm-new-remove = { $name } entfernen
dm-new-filter = Personen nach Namen suchen
dm-new-people = Personen
dm-new-hint = Eine Person für eine Direktnachricht, mehrere für eine Gruppe.
dm-new-start = Unterhaltung öffnen
dm-new-start-group = Gruppe starten

## Kompaktes Layout (Telefone): untere Navigation, zurück, Menüs; Anrufe im Browser
bottom-nav-label = Hauptnavigation
bottom-nav-spaces = Spaces
bottom-nav-you = Ich

## Seitenleiste
sidebar-label = Kanäle und Direktnachrichten
sidebar-threads = Threads
sidebar-unreads = Ungelesen
sidebar-favorites = Favoriten
sidebar-channels = Kanäle
sidebar-integrations = Integrationen
sidebar-direct-messages = Direktnachrichten
sidebar-deactivated-accounts = Deaktivierte Konten
sidebar-item-mentions = { $name }, { $count } Erwähnungen
sidebar-item-unread = { $name }, { $count ->
    [one] 1 ungelesene Nachricht
   *[other] { $count } ungelesene Nachrichten
  }
sidebar-item-unread-mentions = { $name }, { $unread } ungelesene Nachrichten, { $count ->
    [one] 1 Erwähnung
   *[other] { $count } Erwähnungen
  }
sidebar-resize = Größe der Seitenleiste ändern
sidebar-item-muted = Benachrichtigungen stummgeschaltet
sidebar-item-muted-label = { $label }, Benachrichtigungen stummgeschaltet
sidebar-item-catching-up = { $name }, wird nachgeladen

## Kanal
favorite-add = Zu Favoriten hinzufügen
favorite-remove = Aus Favoriten entfernen

## Administration
menu-administration = Administration
brand-home-label = Startseite

## Geräte-Freigabe, Verknüpfung und Schlüsseländerungen
dm-setup-pending = Verschlüsselung wird eingerichtet… Die Gruppe der anderen Seite wird beigetreten; sobald das fertig ist, lassen sich Nachrichten senden.
dm-setup-failed = Die Verschlüsselung dieser Unterhaltung konnte noch nicht eingerichtet werden. Erneut versuchen bittet die Geräte der anderen Seite, dieses Gerät aufzunehmen.

## Anmeldung
menu-switch-account = Konto wechseln
menu-log-out-demo = Du verwendest Demodaten, es gibt nichts abzumelden.

## Suche
sidebar-show-more = { $count ->
    [one] { $count } weitere Unterhaltung anzeigen
   *[other] { $count } weitere Unterhaltungen anzeigen
  }

## Einrichtung der Schlüssel
# ---- Meetings ----
sidebar-meetings = Meetings

# Message actions
saved-title = Gespeicherte Elemente
saved-open = Unterhaltung öffnen
saved-empty = Nutze „Für später speichern“ im Menü einer Nachricht.
saved-empty-title = Noch nichts gespeichert
sidebar-saved = Gespeichert

# Profiles, people and groups (v1 REST UI)
sidebar-people = Personen

## Aktivität, zuletzt online und Anrufliste
sidebar-activity = Aktivität
sidebar-calls = Anrufe
activity-title = Aktivität
activity-filter = Aktivität filtern
activity-all = Alle
activity-unread = Ungelesen
activity-mentions = Erwähnungen
activity-threads = Threads
activity-empty-title = Alles erledigt
activity-mark-read = Alle als gelesen markieren
activity-empty = Erwähnungen, Reaktionen auf deine Nachrichten und Antworten in deinen Threads erscheinen hier.
activity-reactions = Reaktionen
activity-empty-mentions-title = Keine Erwähnungen
activity-empty-mentions = Wenn dich jemand in einem Kanal oder Chat mit @ erwähnt, erscheint das hier.
activity-empty-threads-title = Keine Threads, denen du folgst
activity-empty-threads = Threads, die du beginnst, in denen du antwortest oder denen du folgst, erscheinen hier mit ihren neuen Antworten.
activity-empty-reactions-title = Keine Reaktionen
activity-empty-reactions = Wenn jemand auf eine deiner Nachrichten reagiert, erscheint das hier.
activity-thread-new = { $count ->
    [one] { $count } neue Antwort in { $channel }, in einem Thread, dem du folgst
   *[other] { $count } neue Antworten in { $channel }, in einem Thread, dem du folgst
  }
activity-thread-quiet = Thread in { $channel }, dem du folgst
activity-place-title = Was passiert ist, während du weg warst
activity-place-body = Wähle links einen Eintrag, um ihn hier zu öffnen: Erwähnungen, Antworten in Threads, denen du folgst, und Reaktionen auf deine Nachrichten.
activity-unread-mark = Ungelesen
activity-mentioned = { $name } hat dich in { $channel } erwähnt
activity-reacted = { $name } hat mit { $emoji } auf deine Nachricht in { $channel } reagiert
activity-replied = { $name } hat in einem Thread in { $channel } geantwortet

# Bereiche: Kanäle verschieben, stummschalten, Suche im Bereich, gruppierte Seitenleiste, Aufbewahrung
sidebar-all-spaces = Alle Bereiche

## Daily-use improvements (docs/ux-backlog.md)
sidebar-mark-all-read = Alle als gelesen markieren

## Getting started (empty workspace)
start-title = Willkommen, { $name }
start-empty-subtitle = Dein Workspace ist noch leer. Leg zuerst einen Space für dein Team an, dann Kanäle, und lade deine Kolleg:innen ein.
start-guest-subtitle = Du bist noch in keiner Unterhaltung. Wer dich eingeladen hat, fügt dich den passenden Kanälen hinzu.
start-space-subtitle = Nächste Schritte für { $name }:
start-create-space = Space anlegen
start-create-space-hint = Ein Space enthält die Kanäle eines Teams, einer Abteilung oder eines Projekts.
start-create-channel = Ersten Kanal anlegen
start-create-channel-hint = In Kanälen spricht das Team miteinander, zum Beispiel #general.
start-browse-channels = Kanäle durchsuchen
start-browse-channels-hint = Den Kanälen beitreten, die es schon gibt.
start-browse-spaces = Spaces durchsuchen
start-browse-spaces-hint = Offene Spaces anderer Teams finden und beitreten.
start-invite = Kolleg:innen einladen
start-invite-hint = Kolleg:innen aus dem Verzeichnis hinzufügen oder per E-Mail einladen, als Mitglied oder Gast.
start-find-people = Personen finden
start-find-people-hint = Alle, die sich schon angemeldet haben; eine Direktnachricht beginnen.
start-join-heading = Spaces, denen du beitreten kannst

## Settings: sections, search and rows (#327)
# Startseiten (Issue #585)
page-today = Heute
page-yesterday = Gestern
saved-count = { $count ->
    [one] { $count } gespeicherte Nachricht
   *[other] { $count } gespeicherte Nachrichten
  }

# Shell (Issue #299)
sidebar-filter = Unterhaltungen filtern
rail-create = Erstellen
rail-create-short = Neu
rail-focus = Fokusmodus: Benachrichtigungen pausieren
rail-focus-short = Fokus
focus-30-minutes = Für 30 Minuten

# Dateien-Ansicht und Einrichtungs-Checkliste (Issue #301)
sidebar-files = Dateien
sidebar-shortcuts = Schnellzugriffe
setup-title = Arbeitsbereich einrichten
setup-progress = { $done } von { $total }
setup-dismiss = Checkliste ausblenden
setup-picture = Profilbild hinzufügen
setup-picture-hint = Damit dich dein Team erkennt.
setup-app = Mobile App holen
setup-app-hint = Anrufe und Nachrichten auf dem Handy.
setup-channels = Kanäle durchstöbern
setup-channels-hint = Tritt denen bei, die dich interessieren.
setup-invite = Personen einladen
setup-invite-hint = Hol dein Team dazu.
notes-title = Mein Speicher
notes-empty-title = Noch nichts notiert
notes-empty = Halte hier Ideen, Links und Dateien fest. Nur deine eigenen Geräte können sie lesen.
