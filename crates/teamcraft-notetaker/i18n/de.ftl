# TeamCraft-Notetaker – Deutsch
# Was der Notetaker in die Unterhaltung eines Anrufs schreibt (e2ee.md §8.2).

## Wenn er einem Anruf beitritt
joined-anyone-may-stop = Der Notetaker ist in diesem Anruf und schreibt mit. Der Ton geht zur Transkription an OpenRouter ({ $model }) und wird nicht aufbewahrt; Zusammenfassung und Transkript erscheinen hier. Alle im Anruf können ihn mit „Notetaker entfernen“ hinausschicken.
joined-some-may-stop = Der Notetaker ist in diesem Anruf und schreibt mit. Der Ton geht zur Transkription an OpenRouter ({ $model }) und wird nicht aufbewahrt; Zusammenfassung und Transkript erscheinen hier. Wer ihn hinzugeholt hat und Admins können ihn mit „Notetaker entfernen“ hinausschicken.

## Das Ergebnis
result-heading = Notizen zum Anruf vom { $date }
result-details = { $minutes } Min. · { $participants }
result-footer = Automatisch geschrieben ({ $summary_model } aus einem Transkript von { $transcription_model }); Fehler sind möglich. Das vollständige Transkript ist angehängt.
result-gaps = { $count ->
    [one] Ein Redeabschnitt konnte nicht transkribiert werden.
   *[other] { $count } Redeabschnitte konnten nicht transkribiert werden.
}
result-no-speech = Der Notetaker hat in diesem Anruf niemanden sprechen hören, es gibt nichts zusammenzufassen.
result-summary-failed = Die Zusammenfassung konnte nicht geschrieben werden ({ $reason }). Das Transkript ist angehängt.
result-partial = Der Notetaker ist früher gegangen ({ $reason }); diese Notizen reichen bis dahin.
join-failed = Der Notetaker konnte diesem Anruf nicht beitreten: Die Apps im Anruf haben seine Schlüssel nicht angenommen. Er muss Mitglied dieser Unterhaltung sein, bevor der Anruf beginnt; wer schon vor seinem Hinzufügen im Anruf war, muss neu beitreten.

## Warum er früher gegangen ist
reason-requested = er wurde entfernt
reason-removed-by-host = ein Host hat ihn entfernt
reason-disabled = der Notetaker wurde abgeschaltet
reason-max-duration = die längste Anrufdauer war erreicht
reason-connection-lost = seine Verbindung ist abgerissen
reason-shutdown = er wurde neu gestartet

## Die Transkript-Datei
transcript-title = Transkript des Anrufs vom { $date }
transcript-note = Automatisch transkribiert mit { $model }; Fehler sind möglich. Sprecher sind nach ihrem Gerät im Anruf benannt.
transcript-file-name = transkript-{ $date }.md
participant-fallback = Teilnehmer { $number }
