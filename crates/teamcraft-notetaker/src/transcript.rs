//! The transcript of a call: what each speaker said and when, in the order it was said. It is kept
//! in the notetaker's encrypted store while the call runs (so a restart doesn't lose it), goes to
//! the summary model as plain lines, and to the conversation as a Markdown file.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::UserId;

use crate::texts::{TextArg, Texts};

/// Consecutive segments of one speaker closer than this are one paragraph in the file.
const MERGE_GAP_MS: u64 = 2_000;

/// What was said, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
  segments: Vec<Segment>,
  /// Pieces of speech that could not be transcribed (given up after retries).
  #[serde(default)]
  pub failed_pieces: u32,
}

/// One transcribed piece of one speaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
  pub user: UserId,
  /// From the start of the notetaker's session.
  pub start_ms: u64,
  pub end_ms: u64,
  pub text: String,
}

/// Speaker names: the names the notetaker knows, numbered placeholders for the others.
pub struct Names {
  known: HashMap<UserId, String>,
  numbered: HashMap<UserId, u64>,
}

impl Transcript {
  /// Adds a segment in time order; empty text is left out.
  pub fn add(&mut self, segment: Segment) {
    if segment.text.trim().is_empty() {
      return;
    }
    let position = self
      .segments
      .partition_point(|known| (known.start_ms, known.user) <= (segment.start_ms, segment.user));
    self.segments.insert(position, segment);
  }

  pub fn segments(&self) -> &[Segment] {
    &self.segments
  }

  pub fn is_empty(&self) -> bool {
    self.segments.is_empty()
  }

  /// The users who said something, in the order they first spoke.
  pub fn speakers(&self) -> Vec<UserId> {
    let mut speakers = Vec::new();
    for segment in &self.segments {
      if !speakers.contains(&segment.user) {
        speakers.push(segment.user);
      }
    }
    speakers
  }

  /// When the last segment ended.
  pub fn end_ms(&self) -> u64 {
    self
      .segments
      .iter()
      .map(|segment| segment.end_ms)
      .max()
      .unwrap_or(0)
  }

  /// One line per segment, `[hh:mm:ss] Name: text`, for the summary model.
  pub fn lines(&self, names: &Names) -> Vec<String> {
    self
      .segments
      .iter()
      .map(|segment| {
        format!(
          "[{}] {}: {}",
          clock(segment.start_ms),
          names.name(segment.user),
          single_line(&segment.text)
        )
      })
      .collect()
  }

  /// The transcript as a Markdown file: a title, a note on how it was made, and one paragraph per
  /// turn (a speaker's consecutive segments).
  pub fn to_markdown(&self, names: &Names, title: &str, note: &str) -> String {
    let mut markdown = format!("# {title}\n\n_{note}_\n");
    let mut maybe_turn: Option<(UserId, u64, u64, String)> = None;
    for segment in &self.segments {
      let text = single_line(&segment.text);
      match &mut maybe_turn {
        Some((user, _, end_ms, turn_text))
          if *user == segment.user && segment.start_ms <= *end_ms + MERGE_GAP_MS =>
        {
          turn_text.push(' ');
          turn_text.push_str(&text);
          *end_ms = (*end_ms).max(segment.end_ms);
        },
        _ => {
          if let Some(turn) = maybe_turn.take() {
            push_turn(&mut markdown, names, turn);
          }
          maybe_turn = Some((segment.user, segment.start_ms, segment.end_ms, text));
        },
      }
    }
    if let Some(turn) = maybe_turn {
      push_turn(&mut markdown, names, turn);
    }
    markdown
  }
}

impl Names {
  /// `known`: the names of the conversation's members; anybody else gets a number, in the order
  /// they first speak in `transcript`.
  pub fn new(known: HashMap<UserId, String>, transcript: &Transcript, texts: &Texts) -> Self {
    let mut numbered = HashMap::new();
    for user in transcript.speakers() {
      if !known.contains_key(&user) {
        let number = numbered.len() as u64 + 1;
        numbered.insert(user, number);
      }
    }
    let known = known
      .into_iter()
      .chain(numbered.iter().map(|(user, number)| {
        (
          *user,
          texts.format("participant-fallback", &[("number", TextArg::Number(*number))]),
        )
      }))
      .collect();
    Self { known, numbered }
  }

  pub fn name(&self, user: UserId) -> &str {
    self.known.get(&user).map_or("?", String::as_str)
  }

  /// Whether `user` has only a number.
  pub fn is_numbered(&self, user: UserId) -> bool {
    self.numbered.contains_key(&user)
  }
}

fn push_turn(markdown: &mut String, names: &Names, (user, start_ms, _, text): (UserId, u64, u64, String)) {
  markdown.push_str(&format!(
    "\n**{} {}:** {}\n",
    clock(start_ms),
    names.name(user),
    text
  ));
}

/// `hh:mm:ss` from the start.
pub fn clock(ms: u64) -> String {
  let seconds = ms / 1000;
  format!(
    "{:02}:{:02}:{:02}",
    seconds / 3600,
    seconds / 60 % 60,
    seconds % 60
  )
}

/// The text on one line, without Markdown that would change the file's structure.
fn single_line(text: &str) -> String {
  text
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
    .trim_start_matches('#')
    .trim()
    .to_owned()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn segments_are_kept_in_time_order_and_empty_ones_left_out() {
    let mut transcript = Transcript::default();
    transcript.add(segment(2, 5_000, "Second"));
    transcript.add(segment(1, 1_000, "First"));
    transcript.add(segment(1, 9_000, "  "));
    let texts: Vec<&str> = transcript
      .segments()
      .iter()
      .map(|segment| segment.text.as_str())
      .collect();
    assert_eq!(texts, ["First", "Second"]);
    assert_eq!(transcript.speakers(), [user(1), user(2)]);
    assert_eq!(transcript.end_ms(), 6_000);
  }

  #[test]
  fn lines_name_known_speakers_and_number_the_others() {
    let mut transcript = Transcript::default();
    transcript.add(segment(1, 61_000, "Hello\nall"));
    transcript.add(segment(9, 3_723_000, "# Hi"));
    let texts = Texts::for_language("en");
    let names = Names::new(
      HashMap::from([(user(1), "Alice".to_owned())]),
      &transcript,
      &texts,
    );
    assert_eq!(
      transcript.lines(&names),
      ["[00:01:01] Alice: Hello all", "[01:02:03] Participant 1: Hi"]
    );
    assert!(names.is_numbered(user(9)) && !names.is_numbered(user(1)));
  }

  #[test]
  fn the_file_joins_a_speakers_consecutive_segments() {
    let mut transcript = Transcript::default();
    transcript.add(segment(1, 0, "One."));
    transcript.add(segment(1, 1_500, "Two."));
    transcript.add(segment(2, 3_000, "Three."));
    transcript.add(segment(1, 4_000, "Four."));
    let names = Names::new(
      HashMap::from([(user(1), "Alice".to_owned()), (user(2), "Bob".to_owned())]),
      &transcript,
      &Texts::for_language("en"),
    );
    let markdown = transcript.to_markdown(&names, "Transcript", "Automatic.");
    assert_eq!(
      markdown,
      "# Transcript\n\n_Automatic._\n\n**00:00:00 Alice:** One. Two.\n\n**00:00:03 Bob:** Three.\n\n**00:00:04 Alice:** Four.\n"
    );
  }

  #[test]
  fn the_transcript_survives_the_store() {
    let mut transcript = Transcript::default();
    transcript.add(segment(1, 0, "Saved"));
    transcript.failed_pieces = 2;
    let json = serde_json::to_string(&transcript).unwrap();
    assert_eq!(serde_json::from_str::<Transcript>(&json).unwrap(), transcript);
  }

  fn segment(user_raw: i64, start_ms: u64, text: &str) -> Segment {
    Segment {
      user: user(user_raw),
      start_ms,
      end_ms: start_ms + 1_000,
      text: text.to_owned(),
    }
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
