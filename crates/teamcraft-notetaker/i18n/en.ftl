# TeamCraft notetaker – English (source language)
# What the notetaker posts into a call's conversation (e2ee.md §8.2).

## When it joins a call
joined-anyone-may-stop = The notetaker is in this call and transcribes it. The audio goes to OpenRouter ({ $model }) for transcription and is not kept; the summary and the transcript will be posted here. Anyone in the call can send it away with "Remove notetaker".
joined-some-may-stop = The notetaker is in this call and transcribes it. The audio goes to OpenRouter ({ $model }) for transcription and is not kept; the summary and the transcript will be posted here. Whoever asked it in and admins can send it away with "Remove notetaker".

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
result-partial = The notetaker left early ({ $reason }); these notes cover the call up to then.
join-failed = The notetaker could not join this call: the apps in the call did not take its keys. It has to be a member of this conversation before the call starts; whoever was in the call before it was added needs to join again.

## Why it left early
reason-requested = it was removed
reason-removed-by-host = a host removed it
reason-disabled = the notetaker was turned off
reason-max-duration = the longest call time was reached
reason-connection-lost = its connection was lost
reason-shutdown = it was restarted

## The transcript file
transcript-title = Transcript of the call on { $date }
transcript-note = Transcribed automatically by { $model }; it may contain mistakes. Speakers are named by their device in the call.
transcript-file-name = transcript-{ $date }.md
participant-fallback = Participant { $number }
