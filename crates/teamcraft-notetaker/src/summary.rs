//! The summary of a call (e2ee.md §8.2): the policy's instructions with the meeting's date,
//! participants and language filled in, and the transcript as the input. A transcript longer than
//! one part (`limits.summary_chunk_chars`) is summarized part by part first, and the notes of the
//! parts together then; notes that are still too long go round once more.

use teamcraft_proto::NotetakerPolicy;

use crate::openrouter::{ChatRequest, OpenRouterClient, OpenRouterError, Usage};

/// How often notes of parts are summarized again before the last step takes what it has.
const MAX_ROUNDS: u32 = 3;
/// Instructions for one part of a long transcript.
const PART_PROMPT: &str = "You take notes on part {part} of {parts} of a meeting transcript, so \
that the minutes can be written from the notes of all parts later. Write in {language}. Use the \
headings \"Key points\", \"Decisions\", \"Action items\" (what, who, when if a date was said) and \
\"Open questions\", with Markdown bullets under each, and \"None\" under a heading without anything. \
Keep the speakers' names. Only write what the transcript says.";
/// Added to the policy's instructions when the input is notes of parts.
const FROM_NOTES: &str = "\n\nThe meeting was long: instead of its transcript you get notes on its \
parts, in order. Combine them into one set of minutes.";

/// What the summary is about.
#[derive(Debug, Clone, Copy)]
pub struct SummaryInput<'a> {
  /// The meeting's date and time, as people read it.
  pub date: &'a str,
  pub participants: &'a [String],
  /// The transcript, one line per segment.
  pub lines: &'a [String],
}

/// How the summary is made.
#[derive(Debug, Clone, Copy)]
pub struct SummarySettings {
  pub chunk_chars: usize,
  pub max_tokens: u32,
}

/// Writes the minutes. Returns them with what the requests cost.
pub async fn summarize(
  client: &OpenRouterClient,
  policy: &NotetakerPolicy,
  input: SummaryInput<'_>,
  settings: SummarySettings,
) -> Result<(String, Usage), OpenRouterError> {
  let language = language_name(policy);
  let mut usage = Usage::default();
  let mut parts = chunk_lines(input.lines, settings.chunk_chars);
  let mut is_from_notes = false;
  let mut round = 0;
  while parts.len() > 1 && round < MAX_ROUNDS {
    let mut notes = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
      let system = part_prompt(index + 1, parts.len(), &language);
      let answer = client
        .complete(ChatRequest {
          model: &policy.summary_model,
          system: &system,
          user: part,
          max_tokens: settings.max_tokens,
          is_zero_data_retention: policy.is_zero_data_retention,
        })
        .await?;
      usage.add(&answer.usage);
      notes.push(format!(
        "### Part {} of {}\n\n{}",
        index + 1,
        parts.len(),
        answer.text
      ));
    }
    parts = chunk_lines(&notes, settings.chunk_chars);
    is_from_notes = true;
    round += 1;
  }
  let mut system = final_prompt(policy, input, &language);
  if is_from_notes {
    system.push_str(FROM_NOTES);
  }
  let user = parts.join("\n\n");
  let answer = client
    .complete(ChatRequest {
      model: &policy.summary_model,
      system: &system,
      user: &user,
      max_tokens: settings.max_tokens,
      is_zero_data_retention: policy.is_zero_data_retention,
    })
    .await?;
  usage.add(&answer.usage);
  Ok((answer.text, usage))
}

/// The policy's instructions with `{language}`, `{date}` and `{participants}` filled in.
pub fn final_prompt(policy: &NotetakerPolicy, input: SummaryInput<'_>, language: &str) -> String {
  let participants = if input.participants.is_empty() {
    "not known".to_owned()
  } else {
    input.participants.join(", ")
  };
  policy
    .prompt()
    .replace("{language}", language)
    .replace("{date}", input.date)
    .replace("{participants}", &participants)
}

fn part_prompt(part: usize, parts: usize, language: &str) -> String {
  PART_PROMPT
    .replace("{part}", &part.to_string())
    .replace("{parts}", &parts.to_string())
    .replace("{language}", language)
}

/// The language to write in, as the prompt names it.
pub fn language_name(policy: &NotetakerPolicy) -> String {
  let name = match policy.maybe_language_code() {
    None => return "the language most of the transcript is in".to_owned(),
    Some("de") => "German",
    Some("en") => "English",
    Some("fr") => "French",
    Some("es") => "Spanish",
    Some("it") => "Italian",
    Some("nl") => "Dutch",
    Some("pl") => "Polish",
    Some("pt") => "Portuguese",
    Some(other) => return format!("the language with the ISO 639-1 code \"{other}\""),
  };
  name.to_owned()
}

/// Splits lines into parts of at most `max_chars` characters (with line breaks), at line
/// boundaries; a single longer line is cut. Never returns an empty list.
pub fn chunk_lines(lines: &[String], max_chars: usize) -> Vec<String> {
  let max_chars = max_chars.max(1);
  let mut parts = Vec::new();
  let mut current = String::new();
  for line in lines {
    let pieces = cut(line, max_chars);
    for piece in pieces {
      let needed = piece.chars().count() + usize::from(!current.is_empty());
      if !current.is_empty() && current.chars().count() + needed > max_chars {
        parts.push(std::mem::take(&mut current));
      }
      if !current.is_empty() {
        current.push('\n');
      }
      current.push_str(&piece);
    }
  }
  if !current.is_empty() || parts.is_empty() {
    parts.push(current);
  }
  parts
}

/// `line` in pieces of at most `max_chars` characters.
fn cut(line: &str, max_chars: usize) -> Vec<String> {
  let chars: Vec<char> = line.chars().collect();
  if chars.len() <= max_chars {
    return vec![line.to_owned()];
  }
  chars
    .chunks(max_chars)
    .map(|chunk| chunk.iter().collect())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  mod chunking {
    use super::*;

    #[test]
    fn short_transcripts_stay_in_one_part() {
      let lines = lines(&["a", "bb", "ccc"]);
      assert_eq!(chunk_lines(&lines, 100), ["a\nbb\nccc"]);
      assert_eq!(chunk_lines(&[], 100), [""]);
    }

    #[test]
    fn parts_break_at_lines_and_respect_the_limit() {
      let lines = lines(&["aaaa", "bbbb", "cccc", "dddd"]);
      let parts = chunk_lines(&lines, 9);
      assert_eq!(parts, ["aaaa\nbbbb", "cccc\ndddd"]);
      assert!(parts.iter().all(|part| part.chars().count() <= 9));
    }

    #[test]
    fn an_overlong_line_is_cut_and_characters_count_not_bytes() {
      let lines = lines(&["äääääää"]);
      assert_eq!(chunk_lines(&lines, 3), ["äää", "äää", "ä"]);
    }

    #[test]
    fn a_long_meeting_gives_several_parts_without_losing_text() {
      let lines: Vec<String> = (0..2_000)
        .map(|index| {
          format!(
            "[00:{:02}:00] Speaker {}: point number {index}",
            index % 60,
            index % 7
          )
        })
        .collect();
      let parts = chunk_lines(&lines, 4_000);
      assert!(parts.len() > 10);
      assert_eq!(parts.join("\n"), lines.join("\n"));
    }
  }

  mod prompts {
    use super::*;

    #[test]
    fn the_policy_prompt_gets_language_date_and_participants() {
      let policy = NotetakerPolicy {
        maybe_prompt: Some("Minutes in {language} of {date} with {participants}.".into()),
        language: "de".into(),
        ..NotetakerPolicy::default()
      };
      let participants = ["Alice".to_owned(), "Bob".to_owned()];
      let input = SummaryInput {
        date: "2026-10-29 14:00 UTC",
        participants: &participants,
        lines: &[],
      };
      assert_eq!(
        final_prompt(&policy, input, &language_name(&policy)),
        "Minutes in German of 2026-10-29 14:00 UTC with Alice, Bob."
      );
    }

    #[test]
    fn the_default_prompt_asks_for_the_four_sections() {
      let policy = NotetakerPolicy::default();
      let input = SummaryInput {
        date: "today",
        participants: &[],
        lines: &[],
      };
      let prompt = final_prompt(&policy, input, &language_name(&policy));
      for section in [
        "## Summary",
        "## Decisions",
        "## Action items",
        "## Open questions",
      ] {
        assert!(prompt.contains(section), "{section}");
      }
      assert!(prompt.contains("the language most of the transcript is in"));
      assert!(prompt.contains("participants were not known"));
      assert!(!prompt.contains('{'));
    }

    #[test]
    fn part_prompts_count_the_parts() {
      let prompt = part_prompt(2, 5, "English");
      assert!(prompt.contains("part 2 of 5") && prompt.contains("Write in English."));
    }
  }

  mod requests {
    use super::*;
    use crate::test_support::{FakeOpenRouter, Reply};

    #[tokio::test]
    async fn a_short_transcript_is_one_request_with_the_policy_prompt() {
      let server = FakeOpenRouter::start(vec![Reply::chat("## Summary\nShort.")]).await;
      let policy = NotetakerPolicy::default();
      let lines = lines(&["[00:00:01] Alice: Hello", "[00:00:03] Bob: Hi"]);
      let (text, usage) = summarize(&server.client(0), &policy, input(&lines), settings(10_000))
        .await
        .unwrap();
      assert_eq!((text.as_str(), usage.requests), ("## Summary\nShort.", 1));
      let request = server.requests().remove(0);
      assert_eq!(request.body["model"], "mistralai/mistral-large-2512");
      assert!(
        request.body["messages"][0]["content"]
          .as_str()
          .unwrap()
          .contains("on 2026-10-29 14:05 UTC")
      );
      assert_eq!(
        request.body["messages"][1]["content"],
        "[00:00:01] Alice: Hello\n[00:00:03] Bob: Hi"
      );
    }

    #[tokio::test]
    async fn a_long_transcript_is_summarized_in_parts_then_together() {
      let lines: Vec<String> = (0..300)
        .map(|index| format!("[00:{:02}:00] Alice: point {index} of a long meeting", index % 60))
        .collect();
      let parts = chunk_lines(&lines, 4_000).len();
      assert!(parts > 2);
      let mut replies: Vec<Reply> = (0..parts)
        .map(|part| Reply::chat(&format!("notes {part}")))
        .collect();
      replies.push(Reply::chat("## Summary\nAll of it."));
      let server = FakeOpenRouter::start(replies).await;
      let (text, usage) = summarize(
        &server.client(0),
        &NotetakerPolicy::default(),
        input(&lines),
        settings(4_000),
      )
      .await
      .unwrap();
      assert_eq!(text, "## Summary\nAll of it.");
      assert_eq!(usage.requests as usize, parts + 1);
      let requests = server.requests();
      let first_system = requests[0].body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
      assert!(first_system.contains(&format!("part 1 of {parts}")));
      let last = requests.last().unwrap();
      let last_system = last.body["messages"][0]["content"].as_str().unwrap();
      assert!(last_system.contains("notes on its parts"));
      let last_user = last.body["messages"][1]["content"].as_str().unwrap();
      assert!(last_user.starts_with("### Part 1 of") && last_user.contains("notes 0"));
    }

    #[tokio::test]
    async fn a_failed_part_fails_the_summary() {
      let lines: Vec<String> = (0..300)
        .map(|index| format!("line {index} {}", "x".repeat(40)))
        .collect();
      let server = FakeOpenRouter::start(vec![Reply::json(400, r#"{"error":{"message":"bad"}}"#)]).await;
      let result = summarize(
        &server.client(0),
        &NotetakerPolicy::default(),
        input(&lines),
        settings(4_000),
      )
      .await;
      assert!(matches!(result, Err(OpenRouterError::Status { status: 400, .. })));
    }

    fn input(lines: &[String]) -> SummaryInput<'_> {
      SummaryInput {
        date: "2026-10-29 14:05 UTC",
        participants: &[],
        lines,
      }
    }

    fn settings(chunk_chars: usize) -> SummarySettings {
      SummarySettings {
        chunk_chars,
        max_tokens: 1_000,
      }
    }
  }

  fn lines(texts: &[&str]) -> Vec<String> {
    texts.iter().map(|text| (*text).to_owned()).collect()
  }
}
