//! Batching of log lines (ARCHITECTURE.md §4.5): lines of one conversation are collected and sent
//! as one message once the interval has passed or the batch reaches its size limit, so a chatty
//! service doesn't produce a message per line.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use teamcraft_proto::{ConversationId, UserId};

/// How batches are cut.
#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
  /// Flush this long after the first line of a batch arrived.
  pub interval: Duration,
  /// Flush as soon as the lines add up to this many bytes.
  pub max_bytes: usize,
  /// Longer lines are cut.
  pub max_line_bytes: usize,
}

/// One line to send, from a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
  pub text: String,
  /// Alerts mention people (`mentions` of the line's source).
  pub is_alert: bool,
}

/// A message ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingBatch {
  pub conversation: ConversationId,
  /// Markdown: a code block with the lines, so clients show them monospaced.
  pub text: String,
  pub mentions: Vec<UserId>,
  pub line_count: usize,
}

/// A source's mention settings travel with its lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mentions {
  pub users: Vec<UserId>,
  /// `@name` written in front of an alert so people see who is meant.
  pub names: Vec<String>,
}

#[derive(Debug)]
struct Pending {
  started_at: Instant,
  lines: Vec<String>,
  bytes: usize,
  mentions: Mentions,
  has_alert: bool,
}

#[derive(Debug)]
pub struct Batcher {
  limits: BatchLimits,
  pending: BTreeMap<ConversationId, Pending>,
}

impl Batcher {
  pub fn new(limits: BatchLimits) -> Self {
    Self {
      limits,
      pending: BTreeMap::new(),
    }
  }

  /// Adds a line; returns the batch if it is full now.
  pub fn push(
    &mut self,
    conversation: ConversationId,
    line: LogLine,
    mentions: &Mentions,
    now: Instant,
  ) -> Option<OutgoingBatch> {
    // Fences are broken before the cut, so the size limits count what is really sent.
    let text = cut(&break_fences(&line.text), self.limits.max_line_bytes);
    let entry = self.pending.entry(conversation).or_insert_with(|| Pending {
      started_at: now,
      lines: Vec::new(),
      bytes: 0,
      mentions: Mentions::default(),
      has_alert: false,
    });
    // A line that would overflow the batch closes the batch before it, so batches stay under the limit.
    let overflows = !entry.lines.is_empty() && entry.bytes + text.len() + 1 > self.limits.max_bytes;
    let closed = if overflows { self.take(conversation) } else { None };
    let entry = self.pending.entry(conversation).or_insert_with(|| Pending {
      started_at: now,
      lines: Vec::new(),
      bytes: 0,
      mentions: Mentions::default(),
      has_alert: false,
    });
    entry.bytes += text.len() + 1;
    entry.lines.push(text);
    if line.is_alert {
      entry.has_alert = true;
      entry.mentions = mentions.clone();
    }
    if closed.is_some() {
      return closed;
    }
    if entry.bytes >= self.limits.max_bytes {
      return self.take(conversation);
    }
    None
  }

  /// Batches whose interval has passed.
  pub fn take_due(&mut self, now: Instant) -> Vec<OutgoingBatch> {
    let due: Vec<ConversationId> = self
      .pending
      .iter()
      .filter(|(_, pending)| now.saturating_duration_since(pending.started_at) >= self.limits.interval)
      .map(|(conversation, _)| *conversation)
      .collect();
    due
      .into_iter()
      .filter_map(|conversation| self.take(conversation))
      .collect()
  }

  /// Everything that is pending, for shutdown.
  pub fn take_all(&mut self) -> Vec<OutgoingBatch> {
    let all: Vec<ConversationId> = self.pending.keys().copied().collect();
    all
      .into_iter()
      .filter_map(|conversation| self.take(conversation))
      .collect()
  }

  /// When the next batch becomes due, if anything is pending.
  pub fn next_due(&self) -> Option<Instant> {
    self
      .pending
      .values()
      .map(|pending| pending.started_at + self.limits.interval)
      .min()
  }

  fn take(&mut self, conversation: ConversationId) -> Option<OutgoingBatch> {
    let pending = self.pending.remove(&conversation)?;
    Some(render(conversation, pending))
  }
}

fn render(conversation: ConversationId, pending: Pending) -> OutgoingBatch {
  let line_count = pending.lines.len();
  let mut text = String::new();
  if pending.has_alert && !pending.mentions.names.is_empty() {
    let names: Vec<String> = pending
      .mentions
      .names
      .iter()
      .map(|name| format!("@{name}"))
      .collect();
    text.push_str(&names.join(" "));
    text.push('\n');
  }
  text.push_str("```\n");
  for line in &pending.lines {
    text.push_str(line);
    text.push('\n');
  }
  text.push_str("```");
  OutgoingBatch {
    conversation,
    text,
    mentions: if pending.has_alert {
      pending.mentions.users
    } else {
      Vec::new()
    },
    line_count,
  }
}

/// Never lets three backticks follow each other, so no line can close the code block: a zero-width
/// space goes in front of every third backtick of a run.
fn break_fences(line: &str) -> String {
  let mut out = String::with_capacity(line.len());
  let mut run = 0;
  for c in line.chars() {
    if c == '`' {
      if run == 2 {
        out.push('\u{200b}');
        run = 0;
      }
      run += 1;
    } else {
      run = 0;
    }
    out.push(c);
  }
  out
}

/// Cuts at a character boundary and marks the cut.
fn cut(line: &str, max_bytes: usize) -> String {
  let line = line.trim_end_matches(['\r', '\n']);
  if line.len() <= max_bytes {
    return line.to_owned();
  }
  let mut end = max_bytes.saturating_sub(3);
  while !line.is_char_boundary(end) {
    end -= 1;
  }
  format!("{}...", &line[..end])
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const LIMITS: BatchLimits = BatchLimits {
    interval: Duration::from_secs(1),
    max_bytes: 64,
    max_line_bytes: 40,
  };

  mod timing {
    use super::*;

    #[test]
    fn lines_are_sent_together_once_the_interval_has_passed() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      assert!(
        batcher
          .push(conversation(1), line("first"), &Mentions::default(), start)
          .is_none()
      );
      assert!(
        batcher
          .push(
            conversation(1),
            line("second"),
            &Mentions::default(),
            start + Duration::from_millis(400)
          )
          .is_none()
      );
      assert!(batcher.take_due(start + Duration::from_millis(900)).is_empty());

      let due = batcher.take_due(start + Duration::from_millis(1000));

      assert_eq!(due.len(), 1);
      assert_eq!(due[0].text, "```\nfirst\nsecond\n```");
      assert_eq!(due[0].line_count, 2);
      assert!(
        batcher.take_due(start + Duration::from_secs(10)).is_empty(),
        "nothing is sent twice"
      );
    }

    #[test]
    fn conversations_are_batched_separately() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      batcher.push(conversation(1), line("a"), &Mentions::default(), start);
      batcher.push(
        conversation(2),
        line("b"),
        &Mentions::default(),
        start + Duration::from_millis(600),
      );
      let due = batcher.take_due(start + Duration::from_millis(1100));
      assert_eq!(
        due.iter().map(|b| b.conversation).collect::<Vec<_>>(),
        [conversation(1)]
      );
      assert_eq!(batcher.next_due(), Some(start + Duration::from_millis(1600)));
    }
  }

  mod size {
    use super::*;

    #[test]
    fn a_batch_closes_before_a_line_that_would_overflow_it() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      let long = "x".repeat(40);
      assert!(
        batcher
          .push(conversation(1), line(&long), &Mentions::default(), start)
          .is_none()
      );
      let closed = batcher.push(conversation(1), line(&long), &Mentions::default(), start);
      assert_eq!(closed.unwrap().line_count, 1);
      assert_eq!(
        batcher.take_all()[0].line_count,
        1,
        "the second line starts a new batch"
      );
    }

    #[test]
    fn a_batch_at_the_limit_goes_out_at_once() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      let line_of_31 = "y".repeat(31);
      assert!(
        batcher
          .push(conversation(1), line(&line_of_31), &Mentions::default(), start)
          .is_none()
      );
      let sent = batcher.push(conversation(1), line(&line_of_31), &Mentions::default(), start);
      assert_eq!(sent.map(|batch| batch.line_count), Some(2));
    }

    #[test]
    fn long_lines_are_cut_on_character_boundaries_and_trailing_newlines_dropped() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      batcher.push(
        conversation(1),
        line(&format!("{}\n", "ä".repeat(60))),
        &Mentions::default(),
        start,
      );
      let text = batcher.take_all().remove(0).text;
      let body = text.lines().nth(1).unwrap();
      assert!(body.len() <= 40 && body.ends_with("..."), "{body}");
    }

    #[test]
    fn long_runs_of_backticks_cannot_end_the_block_either() {
      // Found by fuzzing: replacing "```" pieces by piece re-formed fences across the pieces.
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      for line_text in ["``````````", "`` ` `` ```` ``", "a```b````c`````d"] {
        batcher.push(conversation(1), line(line_text), &Mentions::default(), start);
      }
      let text = batcher.take_all().remove(0).text;
      assert_eq!(text.matches("```").count(), 2, "{text}");
    }

    #[test]
    fn code_fences_inside_lines_cannot_end_the_block() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      batcher.push(conversation(1), line("a ``` b"), &Mentions::default(), start);
      let text = batcher.take_all().remove(0).text;
      assert_eq!(text.matches("```").count(), 2, "{text}");
    }
  }

  mod alerts {
    use super::*;

    #[test]
    fn an_alert_in_the_batch_mentions_the_sources_people() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      let mentions = Mentions {
        users: vec![UserId::from_snowflake(Snowflake::new(7).unwrap())],
        names: vec!["alice".into()],
      };
      batcher.push(conversation(1), line("fine"), &mentions, start);
      batcher.push(
        conversation(1),
        LogLine {
          text: "disk full".into(),
          is_alert: true,
        },
        &mentions,
        start,
      );
      let batch = batcher.take_all().remove(0);
      assert_eq!(batch.mentions, mentions.users);
      assert!(batch.text.starts_with("@alice\n```\n"), "{}", batch.text);
    }

    #[test]
    fn batches_without_alerts_mention_nobody() {
      let (mut batcher, start) = (Batcher::new(LIMITS), Instant::now());
      let mentions = Mentions {
        users: vec![UserId::from_snowflake(Snowflake::new(7).unwrap())],
        names: vec!["alice".into()],
      };
      batcher.push(conversation(1), line("fine"), &mentions, start);
      let batch = batcher.take_all().remove(0);
      assert!(batch.mentions.is_empty() && !batch.text.contains('@'));
    }
  }

  fn line(text: &str) -> LogLine {
    LogLine {
      text: text.into(),
      is_alert: false,
    }
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
