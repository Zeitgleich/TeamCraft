# TeamCraft-Aufnahme-Bot – Deutsch
# Was der Aufnahme-Bot in die Unterhaltung eines Anrufs schreibt (e2ee.md §8.3, #2273).

## Eine Aufnahme
posted = Aufnahme des Anrufs ({ $minutes } Min.).
posted-part = Aufnahme des Anrufs, Teil { $part } ({ $minutes } Min.). Der nächste Teil folgt: Eine Aufnahme endet bei der längsten Dauer, die deine Organisation erlaubt.
earlier-lost = Der Aufnahme-Bot wurde während dieses Anrufs neu gestartet: Der Teil des Anrufs davor wurde nicht aufgenommen.

## Wenn er nicht aufnehmen kann
lost-at-restart = Der Aufnahme-Bot wurde neu gestartet, während er einen Anruf in dieser Unterhaltung aufnahm: Diese Aufnahme ist verloren.
disk-full = Der Aufnahme-Bot konnte diesen Anruf nicht aufnehmen: Der Datenträger, auf den er schreibt, ist fast voll. Sag deiner Administration Bescheid.
write-failed = Der Aufnahme-Bot hat die Aufnahme dieses Anrufs beendet: Er konnte die Aufnahme nicht schreiben.
join-failed = Der Aufnahme-Bot konnte diesem Anruf nicht beitreten: Die Apps im Anruf haben seine Schlüssel nicht angenommen. Wer mit einer älteren App im Anruf ist, muss neu beitreten oder die App aktualisieren.
notice-unconfirmed = Der Aufnahme-Bot hat die Aufnahme dieses Anrufs beendet: Der Server hat nicht bestätigt, dass alle im Anruf von der Aufnahme erfahren.
removed = Ein Host hat den Aufnahme-Bot aus diesem Anruf entfernt: Der Rest des Anrufs wird nicht aufgenommen.
