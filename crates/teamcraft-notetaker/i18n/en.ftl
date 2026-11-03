# TeamCraft notetaker – English (source language)
# What the notetaker posts into a call's conversation (e2ee.md §8.2).

## When it joins a call
joined-anyone-may-stop = The notetaker has joined the call and is making a transcript of the conversation. For this, the audio is sent to OpenRouter ({ $model }) and not kept; the summary and the transcript will be posted here afterwards. Anyone in the call can remove it with "Remove notetaker".
joined-some-may-stop = The notetaker has joined the call and is making a transcript of the conversation. For this, the audio is sent to OpenRouter ({ $model }) and not kept; the summary and the transcript will be posted here afterwards. Whoever added it and admins can remove it with "Remove notetaker".

## The result
result-heading = Notes of the call on { $date }
result-details = { $minutes } min · { $participants }
result-footer = Written automatically ({ $summary_model } from a transcript by { $transcription_model }); it may contain mistakes. The full transcript is attached.
result-gaps = { $count ->
    [one] One piece of speech could not be transcribed.
   *[other] { $count } pieces of speech could not be transcribed.
}
result-no-speech = The notetaker heard no speech in this call, so there is nothing to summarize.
result-summary-failed = The summary could not be written ({ $reason }). The transcript is attached.
result-partial = The notetaker left the call early ({ $reason }); these notes cover the call up to then.
join-failed = The notetaker could not join this call: the apps in the call did not take its keys. Whoever is in the call with an older app needs to join it again or update the app.

## Why it left early
reason-requested = it was removed
reason-removed-by-host = a host removed it
reason-disabled = the notetaker was turned off
reason-max-duration = the longest call time was reached
reason-connection-lost = its connection was lost
reason-shutdown = it was restarted

## The transcript file
transcript-title = Transcript of the call on { $date }
transcript-note = Transcribed automatically by { $model }; it may contain mistakes.
transcript-file-name = transcript-{ $date }.md
participant-fallback = Participant { $number }
