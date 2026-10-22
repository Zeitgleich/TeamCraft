//! Log view of integration channels (PLAN.md, `docs/bridge.md`): a bridge sends a batch of log
//! lines as one message, a code block with one line per entry. The log view shows one row per
//! entry, coloured by level, and filters by minimum level and text.

use crate::model::{LogLevel, split_level};

/// One line of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
  pub level: LogLevel,
  pub text: String,
}

/// Which entries the log view shows. The default shows everything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFilter {
  pub min_level: LogLevel,
  /// Case-insensitive substring; empty matches every entry.
  pub text: String,
}

impl Default for LogFilter {
  fn default() -> Self {
    Self {
      min_level: LogLevel::Debug,
      text: String::new(),
    }
  }
}

impl LogLevel {
  pub fn from_slug(slug: &str) -> Option<Self> {
    [Self::Debug, Self::Info, Self::Warn, Self::Error]
      .into_iter()
      .find(|level| level.slug() == slug)
  }
}

impl LogFilter {
  /// Whether the filter hides anything.
  pub fn is_active(&self) -> bool {
    self.min_level > LogLevel::Debug || !self.text.trim().is_empty()
  }

  pub fn matches(&self, entry: &LogEntry) -> bool {
    entry.level >= self.min_level && {
      let needle = self.text.trim().to_lowercase();
      needle.is_empty() || entry.text.to_lowercase().contains(&needle)
    }
  }
}

/// The lines of a bridge batch: an optional line of `@mentions`, then the code block with one line
/// per entry. Anything else, e.g. text after the closing fence or a prefix that is not a mention,
/// makes the message a normal one, so no text is ever hidden. `None` if the body is no batch.
pub fn batch_lines(body: &str) -> Option<Vec<String>> {
  let mut lines = body.lines();
  let mut found = Vec::new();
  loop {
    let line = lines.next()?;
    if line.starts_with("```") {
      break;
    }
    let is_mention_line =
      !line.trim().is_empty() && line.split_whitespace().all(|word| word.starts_with('@'));
    if !is_mention_line {
      return None;
    }
    found.push(line.trim().to_owned());
  }
  for line in lines.by_ref() {
    if line.trim_end() == "```" {
      return lines.all(|rest| rest.trim().is_empty()).then_some(found);
    }
    found.push(line.to_owned());
  }
  None
}

/// The most severe level in `lines` (`Info` for lines without a level).
pub fn worst_level(lines: &[String]) -> LogLevel {
  lines
    .iter()
    .map(|line| entry(line, LogLevel::Info).level)
    .max()
    .unwrap_or(LogLevel::Info)
}

/// The entries of a log row's body. A multi-line body is a batch (see
/// [`as_log_line`](crate::model::as_log_line)): each line carries its own `[LEVEL]` prefix. A single
/// line already lost its prefix and has the level of the row.
pub fn log_entries(body: &str, row_level: LogLevel) -> Vec<LogEntry> {
  if body.contains('\n') {
    body.lines().map(|line| entry(line, LogLevel::Info)).collect()
  } else {
    vec![LogEntry {
      level: row_level,
      text: body.to_owned(),
    }]
  }
}

fn entry(line: &str, default_level: LogLevel) -> LogEntry {
  match split_level(line.trim_start()) {
    Some((level, text)) => LogEntry {
      level,
      text: text.to_owned(),
    },
    None => LogEntry {
      level: default_level,
      text: line.trim_start().to_owned(),
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{Message, MessageKind, as_log_line, id_from_raw};

  #[test]
  fn a_batch_becomes_one_row_with_an_entry_per_line() {
    let row = as_log_line(message(
      "@ops\n```\n[INFO] started\n[ERROR] disk full\nplain\n```",
    ));
    assert_eq!(row.kind, MessageKind::Log(LogLevel::Error));
    let entries = log_entries(&row.body, LogLevel::Error);
    assert_eq!(
      entries,
      vec![
        LogEntry {
          level: LogLevel::Info,
          text: "@ops".into()
        },
        LogEntry {
          level: LogLevel::Info,
          text: "started".into()
        },
        LogEntry {
          level: LogLevel::Error,
          text: "disk full".into()
        },
        LogEntry {
          level: LogLevel::Info,
          text: "plain".into()
        },
      ]
    );
  }

  #[test]
  fn a_one_line_batch_is_a_plain_log_line() {
    let row = as_log_line(message("```\n[WARN] slow query\n```"));
    assert_eq!(row.kind, MessageKind::Log(LogLevel::Warn));
    assert_eq!(row.body, "slow query");
    assert_eq!(log_entries(&row.body, LogLevel::Warn).len(), 1);
  }

  #[test]
  fn text_around_the_fence_is_never_hidden() {
    // Not written by the bridge: the whole message stays visible, line by line.
    let row = as_log_line(message("deploy notes\n```\n[ERROR] x\n```\nsee above"));
    assert_eq!(row.body, "deploy notes\n```\n[ERROR] x\n```\nsee above");
    let texts: Vec<String> = log_entries(&row.body, LogLevel::Info)
      .into_iter()
      .map(|entry| entry.text)
      .collect();
    assert_eq!(texts, vec!["deploy notes", "```", "x", "```", "see above"]);
    assert_eq!(batch_lines("```\na\n```\nafter"), None);
    assert_eq!(batch_lines("not a mention\n```\na\n```"), None);
  }

  #[test]
  fn unclosed_or_unfenced_text_is_no_batch() {
    assert_eq!(batch_lines("```\na\nb"), None);
    assert_eq!(batch_lines("a\nb"), None);
    assert_eq!(batch_lines("```\n```"), Some(Vec::new()));
  }

  #[test]
  fn the_filter_checks_level_and_text_case_insensitively() {
    let error = LogEntry {
      level: LogLevel::Error,
      text: "Disk FULL".into(),
    };
    let info = LogEntry {
      level: LogLevel::Info,
      text: "disk ok".into(),
    };
    assert!(!LogFilter::default().is_active());
    let by_level = LogFilter {
      min_level: LogLevel::Warn,
      ..LogFilter::default()
    };
    assert!(by_level.is_active() && by_level.matches(&error) && !by_level.matches(&info));
    let by_text = LogFilter {
      text: " full ".into(),
      ..LogFilter::default()
    };
    assert!(by_text.matches(&error) && !by_text.matches(&info));
    assert_eq!(LogLevel::from_slug("warn"), Some(LogLevel::Warn));
    assert_eq!(LogLevel::from_slug("nope"), None);
  }

  fn message(body: &str) -> Message {
    Message {
      id: id_from_raw(1),
      conversation: id_from_raw(2),
      author: id_from_raw(3),
      kind: MessageKind::User,
      sent_at: 0,
      body: body.to_owned(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }
}
