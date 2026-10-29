//! The messages the notetaker posts into the call's conversation: the notice when it joins, and
//! the result when it leaves (the summary as text, the transcript as a Markdown attachment).

use teamcraft_core::client::OutgoingFile;
use teamcraft_proto::NotetakerPolicy;
use time::OffsetDateTime;
use time::macros::format_description;

use crate::session::StopReason;
use crate::texts::{TextArg, Texts};
use crate::transcript::{Names, Transcript};

/// What a finished call needs for its result message.
pub struct ReportInput<'a> {
  pub texts: &'a Texts,
  pub policy: &'a NotetakerPolicy,
  /// When the notetaker joined (Unix milliseconds).
  pub started_at_ms: i64,
  pub minutes: u64,
  pub names: &'a Names,
  pub transcript: &'a Transcript,
  pub reason: &'a StopReason,
  /// The summary, or why there is none.
  pub summary: Result<&'a str, &'a str>,
}

/// One message with an optional file.
pub struct Report {
  pub body: String,
  pub maybe_file: Option<OutgoingFile>,
}

/// The notice when the notetaker joins: that it transcribes, where the audio goes, who can send it
/// away (e2ee.md §8.2, "Everybody sees it").
pub fn joined_notice(texts: &Texts, policy: &NotetakerPolicy) -> String {
  let key = if policy.can_anyone_stop {
    "joined-anyone-may-stop"
  } else {
    "joined-some-may-stop"
  };
  texts.format(key, &[("model", TextArg::Text(&policy.transcription_model))])
}

/// The result of a call.
pub fn compose(input: &ReportInput<'_>) -> Report {
  let date = date_time(input.started_at_ms);
  let heading = input
    .texts
    .format("result-heading", &[("date", TextArg::Text(&date))]);
  if input.transcript.is_empty() {
    return Report {
      body: format!("**{heading}**\n\n{}", input.texts.get("result-no-speech")),
      maybe_file: None,
    };
  }
  let participants = input
    .transcript
    .speakers()
    .into_iter()
    .map(|user| input.names.name(user).to_owned())
    .collect::<Vec<_>>()
    .join(", ");
  let details = input.texts.format(
    "result-details",
    &[
      ("minutes", TextArg::Number(input.minutes.max(1))),
      ("participants", TextArg::Text(&participants)),
    ],
  );
  let mut body = format!("**{heading}**\n{details}\n");
  if let Some(reason_key) = early_reason(input.reason) {
    let reason = input.texts.get(reason_key);
    body.push_str(&format!(
      "\n{}\n",
      input
        .texts
        .format("result-partial", &[("reason", TextArg::Text(&reason))])
    ));
  }
  match input.summary {
    Ok(summary) => body.push_str(&format!("\n{summary}\n")),
    Err(reason) => body.push_str(&format!(
      "\n{}\n",
      input
        .texts
        .format("result-summary-failed", &[("reason", TextArg::Text(reason))])
    )),
  }
  if input.transcript.failed_pieces > 0 {
    body.push_str(&format!(
      "\n{}\n",
      input.texts.format(
        "result-gaps",
        &[(
          "count",
          TextArg::Number(u64::from(input.transcript.failed_pieces))
        )]
      )
    ));
  }
  body.push_str(&format!(
    "\n_{}_",
    input.texts.format(
      "result-footer",
      &[
        ("summary_model", TextArg::Text(&input.policy.summary_model)),
        (
          "transcription_model",
          TextArg::Text(&input.policy.transcription_model)
        ),
      ],
    )
  ));
  Report {
    body,
    maybe_file: Some(transcript_file(input, &date)),
  }
}

fn transcript_file(input: &ReportInput<'_>, date: &str) -> OutgoingFile {
  let title = input
    .texts
    .format("transcript-title", &[("date", TextArg::Text(date))]);
  let note = input.texts.format(
    "transcript-note",
    &[("model", TextArg::Text(&input.policy.transcription_model))],
  );
  let file_date = file_date(input.started_at_ms);
  OutgoingFile {
    file_name: input
      .texts
      .format("transcript-file-name", &[("date", TextArg::Text(&file_date))]),
    media_type: "text/markdown".to_owned(),
    content: input
      .transcript
      .to_markdown(input.names, &title, &note)
      .into_bytes(),
  }
}

/// The text key of why the notetaker left before the call ended, if it did.
fn early_reason(reason: &StopReason) -> Option<&'static str> {
  match reason {
    StopReason::Alone | StopReason::EndedByHost | StopReason::JoinTimeout => None,
    StopReason::Requested => Some("reason-requested"),
    StopReason::RemovedByHost => Some("reason-removed-by-host"),
    StopReason::Disabled => Some("reason-disabled"),
    StopReason::MaxDuration => Some("reason-max-duration"),
    StopReason::ConnectionLost(_) => Some("reason-connection-lost"),
    StopReason::Shutdown => Some("reason-shutdown"),
  }
}

/// `2026-10-29 14:05 UTC`.
pub fn date_time(unix_ms: i64) -> String {
  format_unix(
    unix_ms,
    format_description!("[year]-[month]-[day] [hour]:[minute] UTC"),
  )
}

/// `2026-10-29-1405`, for the file name.
fn file_date(unix_ms: i64) -> String {
  format_unix(
    unix_ms,
    format_description!("[year]-[month]-[day]-[hour][minute]"),
  )
}

fn format_unix(unix_ms: i64, description: &[time::format_description::BorrowedFormatItem<'_>]) -> String {
  OffsetDateTime::from_unix_timestamp(unix_ms.div_euclid(1000))
    .ok()
    .and_then(|moment| moment.format(description).ok())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use teamcraft_proto::{Id, Snowflake, UserId};

  use super::*;
  use crate::transcript::Segment;

  /// 2026-10-29 14:05:00 UTC.
  const STARTED_AT_MS: i64 = 1_793_282_700_000;

  #[test]
  fn the_join_notice_names_the_model_and_who_may_stop_it() {
    let texts = Texts::for_language("en");
    let mut policy = NotetakerPolicy::default();
    let notice = joined_notice(&texts, &policy);
    assert!(notice.contains("OpenRouter (mistralai/voxtral-mini-transcribe-3-0)"));
    assert!(notice.contains("Anyone in the call"));
    policy.can_anyone_stop = false;
    assert!(joined_notice(&texts, &policy).contains("Whoever asked it in and admins"));
  }

  #[test]
  fn the_result_has_the_summary_details_footer_and_transcript() {
    let texts = Texts::for_language("en");
    let policy = NotetakerPolicy::default();
    let transcript = transcript();
    let names = names(&transcript, &texts);
    let report = compose(&ReportInput {
      texts: &texts,
      policy: &policy,
      started_at_ms: STARTED_AT_MS,
      minutes: 42,
      names: &names,
      transcript: &transcript,
      reason: &StopReason::Alone,
      summary: Ok("## Summary\nWe agreed."),
    });
    assert!(
      report
        .body
        .starts_with("**Notes of the call on 2026-10-29 14:05 UTC**\n42 min · Alice, Bob\n")
    );
    assert!(report.body.contains("\n## Summary\nWe agreed.\n"));
    assert!(report.body.ends_with("The full transcript is attached._"));
    assert!(!report.body.contains("left early"));
    let file = report.maybe_file.unwrap();
    assert_eq!(file.file_name, "transcript-2026-10-29-1405.md");
    assert_eq!(file.media_type, "text/markdown");
    let markdown = String::from_utf8(file.content).unwrap();
    assert!(markdown.starts_with("# Transcript of the call on 2026-10-29 14:05 UTC\n"));
    assert!(markdown.contains("**00:00:01 Alice:** Hello."));
  }

  #[test]
  fn an_early_end_a_failed_summary_and_gaps_are_said() {
    let texts = Texts::for_language("de");
    let policy = NotetakerPolicy::default();
    let mut transcript = transcript();
    transcript.failed_pieces = 2;
    let names = names(&transcript, &texts);
    let report = compose(&ReportInput {
      texts: &texts,
      policy: &policy,
      started_at_ms: STARTED_AT_MS,
      minutes: 3,
      names: &names,
      transcript: &transcript,
      reason: &StopReason::Requested,
      summary: Err("OpenRouter refused the API key"),
    });
    assert!(
      report
        .body
        .contains("Der Notetaker ist früher gegangen (er wurde entfernt)")
    );
    assert!(report.body.contains("(OpenRouter refused the API key)"));
    assert!(report.body.contains("2 Redeabschnitte"));
    assert!(report.maybe_file.is_some());
  }

  #[test]
  fn without_speech_there_is_a_short_note_and_no_file() {
    let texts = Texts::for_language("en");
    let transcript = Transcript::default();
    let names = names(&transcript, &texts);
    let report = compose(&ReportInput {
      texts: &texts,
      policy: &NotetakerPolicy::default(),
      started_at_ms: STARTED_AT_MS,
      minutes: 1,
      names: &names,
      transcript: &transcript,
      reason: &StopReason::Alone,
      summary: Err("unused"),
    });
    assert!(report.body.contains("heard no speech"));
    assert!(report.maybe_file.is_none());
  }

  fn transcript() -> Transcript {
    let mut transcript = Transcript::default();
    for (user, start_ms, text) in [(1, 1_000, "Hello."), (2, 4_000, "Hi.")] {
      transcript.add(Segment {
        user: user_id(user),
        start_ms,
        end_ms: start_ms + 900,
        text: text.to_owned(),
      });
    }
    transcript
  }

  fn names(transcript: &Transcript, texts: &Texts) -> Names {
    Names::new(
      HashMap::from([(user_id(1), "Alice".to_owned()), (user_id(2), "Bob".to_owned())]),
      transcript,
      texts,
    )
  }

  fn user_id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
