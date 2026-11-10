//! What a poll is inside a message's ciphertext (issue #205, e2ee.md §4.12). The server never sees
//! these types: a poll is the plaintext of an encrypted text message (`MessageMeta::poll` in
//! `teamcraft-core`), and votes and closes are encrypted, hidden messages that name it.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::wire_enum::wire_enum;

/// Fewest options a poll has.
pub const MIN_POLL_OPTIONS: usize = 2;
/// Most options a received poll may have. A message claiming more isn't taken as a poll (it shows
/// its text), so a hostile sender can't make every member's device count thousands of options.
pub const MAX_RECEIVED_POLL_OPTIONS: usize = 32;
/// Longest received question and option, in characters; longer ones aren't taken as a poll either.
pub const MAX_RECEIVED_POLL_TEXT_CHARS: usize = 1_000;
/// Default of [`PollLimits::max_options`].
pub const DEFAULT_MAX_POLL_OPTIONS: usize = 12;
/// Default of [`PollLimits::max_question_chars`].
pub const DEFAULT_MAX_POLL_QUESTION_CHARS: usize = 300;
/// Default of [`PollLimits::max_option_chars`].
pub const DEFAULT_MAX_POLL_OPTION_CHARS: usize = 100;

/// A poll as its creator sent it. It never changes: an edit of the message replaces only the text
/// older versions show, and options are named by their position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollSpec {
  pub question: String,
  pub options: Vec<String>,
  pub choice: PollChoice,
  pub voters: PollVoters,
}

wire_enum! {
  /// How many options a voter may choose.
  pub enum PollChoice {
    /// One option.
    Single => "single",
    /// Any number of options, at least one.
    Multiple => "multiple",
  }
}

wire_enum! {
  /// Whether the poll shows who voted for what. Hidden is a promise of the clients' display, not
  /// anonymity: every member's device receives each vote signed by its sender (e2ee.md §4.12).
  pub enum PollVoters {
    /// Everybody sees who chose each option.
    Shown => "shown",
    /// The poll shows only counts. A member's device still knows who voted for what.
    Hidden => "hidden",
  }
}

/// How large a poll a sender may make. Receivers only apply the wider
/// [`MAX_RECEIVED_POLL_OPTIONS`] and [`MAX_RECEIVED_POLL_TEXT_CHARS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollLimits {
  pub max_options: usize,
  pub max_question_chars: usize,
  pub max_option_chars: usize,
}

/// Why a poll can't be sent as it is. `index` counts options from 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PollError {
  #[error("the question is empty")]
  EmptyQuestion,
  #[error("the question is longer than {max} characters")]
  QuestionTooLong { max: usize },
  #[error("a poll needs at least {min} options")]
  TooFewOptions { min: usize },
  #[error("a poll has at most {max} options")]
  TooManyOptions { max: usize },
  #[error("option {index} is empty")]
  EmptyOption { index: usize },
  #[error("option {index} is longer than {max} characters")]
  OptionTooLong { index: usize, max: usize },
  #[error("option {index} repeats an earlier one")]
  DuplicateOption { index: usize },
}

impl Default for PollLimits {
  fn default() -> Self {
    Self {
      max_options: DEFAULT_MAX_POLL_OPTIONS,
      max_question_chars: DEFAULT_MAX_POLL_QUESTION_CHARS,
      max_option_chars: DEFAULT_MAX_POLL_OPTION_CHARS,
    }
  }
}

impl PollSpec {
  /// A poll from what the user typed: question and options trimmed, empty options left out.
  pub fn from_input(question: &str, options: &[String], choice: PollChoice, voters: PollVoters) -> Self {
    Self {
      question: question.trim().to_owned(),
      options: options
        .iter()
        .map(|option| option.trim())
        .filter(|option| !option.is_empty())
        .map(str::to_owned)
        .collect(),
      choice,
      voters,
    }
  }

  /// Whether this may be sent under `limits`; the first problem found otherwise.
  pub fn check(&self, limits: &PollLimits) -> Result<(), PollError> {
    let question = self.question.trim();
    if question.is_empty() {
      return Err(PollError::EmptyQuestion);
    }
    if question.chars().count() > limits.max_question_chars {
      return Err(PollError::QuestionTooLong {
        max: limits.max_question_chars,
      });
    }
    if self.options.len() < MIN_POLL_OPTIONS {
      return Err(PollError::TooFewOptions {
        min: MIN_POLL_OPTIONS,
      });
    }
    let max_options = limits.max_options.min(MAX_RECEIVED_POLL_OPTIONS);
    if self.options.len() > max_options {
      return Err(PollError::TooManyOptions { max: max_options });
    }
    for (index, option) in self.options.iter().enumerate() {
      let option = option.trim();
      if option.is_empty() {
        return Err(PollError::EmptyOption { index });
      }
      if option.chars().count() > limits.max_option_chars {
        return Err(PollError::OptionTooLong {
          index,
          max: limits.max_option_chars,
        });
      }
      let is_repeated = self.options[..index]
        .iter()
        .any(|earlier| earlier.trim().to_lowercase() == option.to_lowercase());
      if is_repeated {
        return Err(PollError::DuplicateOption { index });
      }
    }
    Ok(())
  }

  /// Whether a received poll is one this device counts: within the bounds every receiver applies,
  /// whatever the sender's limits were. A message failing this shows its text only.
  pub fn is_receivable(&self) -> bool {
    let is_short = |text: &str| text.chars().count() <= MAX_RECEIVED_POLL_TEXT_CHARS;
    !self.question.trim().is_empty()
      && is_short(&self.question)
      && (MIN_POLL_OPTIONS..=MAX_RECEIVED_POLL_OPTIONS).contains(&self.options.len())
      && self.options.iter().all(|option| is_short(option))
  }
}

impl PollChoice {
  /// Whether `choices` (option positions) is a vote this poll takes: distinct, each an option of
  /// the poll, one at most for a single choice. None withdraws a vote. A mode of a newer version
  /// takes no votes.
  pub fn allows(&self, choices: &[u32], option_count: usize) -> bool {
    let is_known_option = |choice: &u32| usize::try_from(*choice).is_ok_and(|at| at < option_count);
    let is_distinct = choices
      .iter()
      .enumerate()
      .all(|(at, choice)| !choices[..at].contains(choice));
    let fits = match self {
      Self::Single => choices.len() <= 1,
      Self::Multiple => true,
      Self::Unknown(_) => false,
    };
    fits && is_distinct && choices.iter().all(is_known_option)
  }
}

impl PollVoters {
  /// Whether the poll shows who voted for what. A mode of a newer version shows nobody: it may
  /// promise more than this version knows.
  pub fn are_shown(&self) -> bool {
    matches!(self, Self::Shown)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod wire {
    use super::*;

    #[test]
    fn a_poll_keeps_its_wire_names() {
      let poll = spec(&["Yes", "No"]);
      let json = serde_json::to_string(&poll).unwrap();
      assert_eq!(
        json,
        r#"{"question":"Lunch on Thursday?","options":["Yes","No"],"choice":"single","voters":"shown"}"#
      );
      assert_eq!(serde_json::from_str::<PollSpec>(&json).unwrap(), poll);
      let hidden = PollSpec {
        choice: PollChoice::Multiple,
        voters: PollVoters::Hidden,
        ..poll
      };
      let json = serde_json::to_string(&hidden).unwrap();
      assert!(
        json.contains(r#""choice":"multiple","voters":"hidden""#),
        "{json}"
      );
    }

    #[test]
    fn modes_of_a_newer_version_are_kept_and_promise_nothing() {
      let json = r#"{"question":"Q","options":["A","B"],"choice":"ranked","voters":"blinded","since":1}"#;
      let poll: PollSpec = serde_json::from_str(json).unwrap();
      assert_eq!(poll.choice, PollChoice::Unknown("ranked".into()));
      assert_eq!(poll.voters, PollVoters::Unknown("blinded".into()));
      assert!(!poll.voters.are_shown());
      assert!(!poll.choice.allows(&[0], 2));
    }
  }

  mod checks {
    use super::*;

    #[test]
    fn a_reasonable_poll_passes() {
      assert_eq!(spec(&["Yes", "No"]).check(&PollLimits::default()), Ok(()));
    }

    #[test]
    fn input_is_trimmed_and_empty_options_go() {
      let options = vec![
        "  Yes ".to_owned(),
        String::new(),
        " ".to_owned(),
        "No".to_owned(),
      ];
      let poll = PollSpec::from_input("  Q? ", &options, PollChoice::Single, PollVoters::Shown);
      assert_eq!(poll.question, "Q?");
      assert_eq!(poll.options, vec!["Yes", "No"]);
    }

    #[test]
    fn each_problem_is_named() {
      let limits = PollLimits {
        max_options: 3,
        max_question_chars: 5,
        max_option_chars: 4,
      };
      let mut poll = spec(&["A", "B"]);
      poll.question = " ".into();
      assert_eq!(poll.check(&limits), Err(PollError::EmptyQuestion));
      poll.question = "Longer".into();
      assert_eq!(poll.check(&limits), Err(PollError::QuestionTooLong { max: 5 }));
      poll.question = "Q".into();
      poll.options = vec!["A".into()];
      assert_eq!(poll.check(&limits), Err(PollError::TooFewOptions { min: 2 }));
      poll.options = ["A", "B", "C", "D"].map(String::from).to_vec();
      assert_eq!(poll.check(&limits), Err(PollError::TooManyOptions { max: 3 }));
      poll.options = ["A", "", "C"].map(String::from).to_vec();
      assert_eq!(poll.check(&limits), Err(PollError::EmptyOption { index: 1 }));
      poll.options = ["A", "Bbbbb"].map(String::from).to_vec();
      assert_eq!(
        poll.check(&limits),
        Err(PollError::OptionTooLong { index: 1, max: 4 })
      );
      poll.options = ["Yes", "yes "].map(String::from).to_vec();
      assert_eq!(poll.check(&limits), Err(PollError::DuplicateOption { index: 1 }));
    }

    #[test]
    fn options_count_characters_not_bytes() {
      let limits = PollLimits {
        max_option_chars: 3,
        ..PollLimits::default()
      };
      assert_eq!(spec(&["äöü", "日本語"]).check(&limits), Ok(()));
    }

    #[test]
    fn a_received_poll_is_bounded_whatever_the_sender_allowed() {
      assert!(spec(&["A", "B"]).is_receivable());
      assert!(!spec(&["A"]).is_receivable());
      let many: Vec<String> = (0..=MAX_RECEIVED_POLL_OPTIONS).map(|n| n.to_string()).collect();
      let mut poll = spec(&["A", "B"]);
      poll.options = many;
      assert!(!poll.is_receivable());
      let mut poll = spec(&["A", "B"]);
      poll.options[0] = "x".repeat(MAX_RECEIVED_POLL_TEXT_CHARS + 1);
      assert!(!poll.is_receivable());
      let mut poll = spec(&["A", "B"]);
      poll.question = String::new();
      assert!(!poll.is_receivable());
    }
  }

  mod votes {
    use super::*;

    #[test]
    fn a_single_choice_takes_one_option_or_none() {
      assert!(PollChoice::Single.allows(&[], 3));
      assert!(PollChoice::Single.allows(&[2], 3));
      assert!(!PollChoice::Single.allows(&[0, 1], 3));
    }

    #[test]
    fn a_multiple_choice_takes_distinct_options() {
      assert!(PollChoice::Multiple.allows(&[0, 2], 3));
      assert!(!PollChoice::Multiple.allows(&[1, 1], 3));
    }

    #[test]
    fn a_vote_for_an_option_the_poll_lacks_is_refused() {
      assert!(!PollChoice::Single.allows(&[3], 3));
      assert!(!PollChoice::Multiple.allows(&[0, u32::MAX], 3));
    }
  }

  fn spec(options: &[&str]) -> PollSpec {
    PollSpec {
      question: "Lunch on Thursday?".into(),
      options: options.iter().map(|option| (*option).to_owned()).collect(),
      choice: PollChoice::Single,
      voters: PollVoters::Shown,
    }
  }
}
