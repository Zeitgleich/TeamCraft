# TeamCraft-Notetaker – Deutsch
# Was der Notetaker in die Unterhaltung eines Anrufs schreibt (e2ee.md §8.2).

## Wenn er einem Anruf beitritt
joined-anyone-may-stop = Der Notetaker wurde dem Anruf hinzugefügt und erstellt ein Transkript des Gesprächs. Dazu wird der Ton an OpenRouter ({ $model }) gesendet und nicht gespeichert; Zusammenfassung und Transkript erscheinen danach hier. Über „Notetaker entfernen“ kann ihn jeder im Anruf wieder entfernen.
joined-some-may-stop = Der Notetaker wurde dem Anruf hinzugefügt und erstellt ein Transkript des Gesprächs. Dazu wird der Ton an OpenRouter ({ $model }) gesendet und nicht gespeichert; Zusammenfassung und Transkript erscheinen danach hier. Über „Notetaker entfernen“ können ihn die Person, die ihn hinzugefügt hat, und Admins wieder entfernen.

## Das Ergebnis
result-heading = Notizen zum Anruf vom { $date }
result-details = { $minutes } Min. · { $participants }
result-footer = Automatisch geschrieben ({ $summary_model } aus einem Transkript von { $transcription_model }); Fehler sind möglich. Das vollständige Transkript ist angehängt.
result-gaps = { $count ->
    [one] Ein Redeabschnitt konnte nicht transkribiert werden.
   *[other] { $count } Redeabschnitte konnten nicht transkribiert werden.
}
result-no-speech = Der Notetaker hat in diesem Anruf niemanden sprechen hören; es gibt nichts zusammenzufassen.
result-summary-failed = Die Zusammenfassung konnte nicht geschrieben werden ({ $reason }). Das Transkript ist angehängt.
result-partial = Der Notetaker hat den Anruf vorzeitig verlassen ({ $reason }); die Notizen reichen bis dahin.
join-failed = Der Notetaker konnte diesem Anruf nicht beitreten: Die Apps im Anruf haben seine Schlüssel nicht angenommen. Wer mit einer älteren App im Anruf ist, muss dem Anruf neu beitreten oder die App aktualisieren.

## Warum er früher gegangen ist
reason-requested = er wurde entfernt
reason-removed-by-host = ein Host hat ihn entfernt
reason-disabled = der Notetaker wurde ausgeschaltet
reason-max-duration = die maximale Anrufdauer war erreicht
reason-connection-lost = seine Verbindung wurde unterbrochen
reason-shutdown = er wurde neu gestartet

## Die Transkript-Datei
transcript-title = Transkript des Anrufs vom { $date }
transcript-note = Automatisch transkribiert mit { $model }; Fehler sind möglich.
transcript-file-name = transkript-{ $date }.md
participant-fallback = Teilnehmer { $number }
