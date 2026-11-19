//! Polls for the phone apps (issue #205, e2ee.md §4.12): the rules of the client API, so Android
//! and iOS check, send and vote as web and desktop do. The apps read polls from the message JSON
//! (`Message::poll`) and send the core's `ChatAction` messages; these functions only shape data.

use teamcraft_client_api::polls::{
  NewPoll, PollChoice, PollDraft, PollError, PollField, PollLimits, PollOptionView, PollProblem, PollResults,
  PollView, PollVoters, fallback_text,
};
use teamcraft_client_api::remote::ChatAction;
use teamcraft_proto::ConversationId;

use crate::engine::parse_id;
use crate::error::TeamCraftError;

/// What the poll dialog holds, as typed: options in their fields (empty ones are left out), the
/// closing time already as Unix milliseconds.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PollDraftItem {
  pub question: String,
  pub options: Vec<String>,
  pub is_multiple_choice: bool,
  pub hides_voters: bool,
  pub results: PollResultsItem,
  pub maybe_closes_at_ms: Option<i64>,
}

/// When a poll shows its counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PollResultsItem {
  Always,
  AfterVote,
  AfterClose,
}

/// How large a poll may be, for the dialog's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PollLimitsItem {
  pub max_options: u32,
  pub max_question_chars: u32,
  pub max_option_chars: u32,
  pub max_open_days: u32,
}

/// One problem of a draft, at its field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PollProblemItem {
  pub field: PollFieldItem,
  pub kind: PollProblemKind,
}

/// A field of the poll dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PollFieldItem {
  Question,
  /// The option field at this position, counting empty fields.
  Option {
    index: u32,
  },
  Closing,
}

/// What is wrong, with the limit the message names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PollProblemKind {
  EmptyQuestion,
  QuestionTooLong { max: u32 },
  TooFewOptions { min: u32 },
  TooManyOptions { max: u32 },
  OptionTooLong { max: u32 },
  DuplicateOption,
  ClosingPassed,
  ClosingTooFar { max_days: u32 },
}

impl From<PollLimits> for PollLimitsItem {
  fn from(limits: PollLimits) -> Self {
    Self {
      max_options: to_u32(limits.max_options),
      max_question_chars: to_u32(limits.max_question_chars),
      max_option_chars: to_u32(limits.max_option_chars),
      max_open_days: limits.max_open_days,
    }
  }
}

impl From<PollLimitsItem> for PollLimits {
  fn from(item: PollLimitsItem) -> Self {
    Self {
      max_options: item.max_options as usize,
      max_question_chars: item.max_question_chars as usize,
      max_option_chars: item.max_option_chars as usize,
      max_open_days: item.max_open_days,
    }
    .bounded()
  }
}

/// Every problem of `draft` at once, at its field, in the order of the form, when it is `now_ms`,
/// under `limits` (the engine's `poll_limits`).
#[uniffi::export]
pub fn poll_problems(draft: PollDraftItem, limits: PollLimitsItem, now_ms: i64) -> Vec<PollProblemItem> {
  PollDraft::from(draft)
    .problems(&PollLimits::from(limits), now_ms)
    .into_iter()
    .map(PollProblemItem::from)
    .collect()
}

/// The `post_poll` action sending `draft` to `conversation`. `title` ("Poll") and `note` ("Update
/// TeamCraft to vote") make the text versions before polls show, in the user's language. Refused
/// when the draft has a problem at `now_ms` under `limits` (the engine's `poll_limits`).
#[uniffi::export]
pub fn poll_post_action(
  conversation: String,
  draft: PollDraftItem,
  limits: PollLimitsItem,
  title: String,
  note: String,
  now_ms: i64,
) -> Result<String, TeamCraftError> {
  let conversation: ConversationId = parse_id(&conversation)?;
  let spec = PollDraft::from(draft).spec();
  spec
    .check_at(&PollLimits::from(limits), now_ms)
    .map_err(|error| TeamCraftError::InvalidInput(error.to_string()))?;
  let fallback = fallback_text(&spec, &title, &note);
  let action = ChatAction::PostPoll {
    conversation,
    poll: NewPoll { spec, fallback },
  };
  serde_json::to_string(&action).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))
}

/// The choices pressing `option` sends in a poll of `choice` (its wire name) with `option_count`
/// options, of which the user chose `chosen`: picks, switches, adds, takes away or withdraws.
/// `None` when the poll takes no vote now (`is_closed`, an unknown choice or option).
#[uniffi::export]
pub fn poll_choices_after_pressing(
  choice: String,
  chosen: Vec<u32>,
  option_count: u32,
  is_closed: bool,
  option: u32,
) -> Option<Vec<u32>> {
  let options = (0..option_count)
    .map(|at| PollOptionView {
      text: String::new(),
      count: 0,
      is_chosen: chosen.contains(&at),
      voters: Vec::new(),
    })
    .collect();
  let poll = PollView {
    question: String::new(),
    options,
    choice: PollChoice::from(choice),
    voters: PollVoters::Hidden,
    results: PollResults::Always,
    are_results_shown: false,
    maybe_closes_at_ms: None,
    voter_count: 0,
    is_closed,
    is_mine: false,
    can_close: false,
  };
  poll.choices_after_pressing(option)
}

/// `count` people's share of `voter_count` in whole percent, rounded down.
#[uniffi::export]
pub fn poll_share_percent(count: u32, voter_count: u32) -> u32 {
  teamcraft_client_api::polls::share_percent(count, voter_count)
}

impl From<PollDraftItem> for PollDraft {
  fn from(item: PollDraftItem) -> Self {
    Self {
      question: item.question,
      options: item.options,
      choice: if item.is_multiple_choice {
        PollChoice::Multiple
      } else {
        PollChoice::Single
      },
      voters: if item.hides_voters {
        PollVoters::Hidden
      } else {
        PollVoters::Shown
      },
      results: match item.results {
        PollResultsItem::Always => PollResults::Always,
        PollResultsItem::AfterVote => PollResults::AfterVote,
        PollResultsItem::AfterClose => PollResults::AfterClose,
      },
      maybe_closes_at_ms: item.maybe_closes_at_ms,
    }
  }
}

impl From<PollProblem> for PollProblemItem {
  fn from(problem: PollProblem) -> Self {
    let field = match problem.field {
      PollField::Question => PollFieldItem::Question,
      PollField::Option(index) => PollFieldItem::Option { index: to_u32(index) },
      PollField::Closing => PollFieldItem::Closing,
    };
    let kind = match problem.error {
      PollError::EmptyQuestion => PollProblemKind::EmptyQuestion,
      PollError::QuestionTooLong { max } => PollProblemKind::QuestionTooLong { max: to_u32(max) },
      PollError::TooFewOptions { min } => PollProblemKind::TooFewOptions { min: to_u32(min) },
      // An empty field between filled ones is left out, so it only matters when too few remain.
      PollError::EmptyOption { .. } => PollProblemKind::TooFewOptions {
        min: to_u32(teamcraft_proto::MIN_POLL_OPTIONS),
      },
      PollError::TooManyOptions { max } => PollProblemKind::TooManyOptions { max: to_u32(max) },
      PollError::OptionTooLong { max, .. } => PollProblemKind::OptionTooLong { max: to_u32(max) },
      PollError::DuplicateOption { .. } => PollProblemKind::DuplicateOption,
      PollError::ClosingPassed => PollProblemKind::ClosingPassed,
      PollError::ClosingTooFar { max_days } => PollProblemKind::ClosingTooFar { max_days },
    };
    Self { field, kind }
  }
}

fn to_u32(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW_MS: i64 = 1_800_000_000_000;

  fn limits() -> PollLimitsItem {
    PollLimits::default().into()
  }

  #[test]
  fn the_dialogs_limits_decide_what_is_too_long_and_stay_within_what_receivers_count() {
    let tight = PollLimitsItem {
      max_options: 2,
      max_question_chars: 3,
      max_option_chars: 5,
      max_open_days: 1,
    };
    let problems = poll_problems(draft("Lunch?", &["A", "B"]), tight, NOW_MS);
    assert_eq!(problems[0].kind, PollProblemKind::QuestionTooLong { max: 3 });
    let wild = PollLimitsItem {
      max_options: 500,
      max_question_chars: 50_000,
      max_option_chars: 0,
      max_open_days: 0,
    };
    let bounded = PollLimits::from(wild);
    assert_eq!(bounded.max_options, teamcraft_proto::MAX_RECEIVED_POLL_OPTIONS);
    assert_eq!(
      bounded.max_question_chars,
      teamcraft_proto::MAX_RECEIVED_POLL_TEXT_CHARS
    );
    assert_eq!((bounded.max_option_chars, bounded.max_open_days), (1, 1));
  }

  #[test]
  fn problems_name_their_fields_in_form_order() {
    let mut typed = draft("", &["Pizza", "", "pizza"]);
    typed.maybe_closes_at_ms = Some(NOW_MS - 1);
    assert_eq!(
      poll_problems(typed, limits(), NOW_MS),
      [
        PollProblemItem {
          field: PollFieldItem::Question,
          kind: PollProblemKind::EmptyQuestion
        },
        PollProblemItem {
          field: PollFieldItem::Option { index: 2 },
          kind: PollProblemKind::DuplicateOption
        },
        PollProblemItem {
          field: PollFieldItem::Closing,
          kind: PollProblemKind::ClosingPassed
        },
      ]
    );
  }

  #[test]
  fn the_post_action_is_the_cores_post_poll() {
    let mut typed = draft("Lunch?", &["Pizza", "", "Salad"]);
    typed.results = PollResultsItem::AfterVote;
    typed.maybe_closes_at_ms = Some(NOW_MS + 1);
    let json = poll_post_action(
      "7".into(),
      typed,
      limits(),
      "Poll".into(),
      "Update to vote.".into(),
      NOW_MS,
    )
    .unwrap();
    assert_eq!(
      json,
      r#"{"action":"post_poll","conversation":"7","poll":{"spec":{"question":"Lunch?","options":["Pizza","Salad"],"choice":"single","voters":"shown","closes_at":1800000000001,"results":"after_vote"},"fallback":"**Poll:** Lunch?\n- Pizza\n- Salad\n\n_Update to vote._"}}"#
    );
    assert!(serde_json::from_str::<ChatAction>(&json).is_ok());
  }

  #[test]
  fn a_draft_with_a_problem_is_not_sent() {
    let refused = poll_post_action(
      "7".into(),
      draft("Q", &["A"]),
      limits(),
      "Poll".into(),
      "Note".into(),
      NOW_MS,
    );
    assert!(matches!(refused, Err(TeamCraftError::InvalidInput(_))));
    let typed = draft("Q", &["A", "B"]);
    assert!(poll_post_action("x".into(), typed, limits(), "Poll".into(), "Note".into(), NOW_MS).is_err());
  }

  #[test]
  fn pressing_follows_the_client_api() {
    assert_eq!(
      poll_choices_after_pressing("single".into(), vec![1], 3, false, 2),
      Some(vec![2])
    );
    assert_eq!(
      poll_choices_after_pressing("single".into(), vec![1], 3, false, 1),
      Some(Vec::new())
    );
    assert_eq!(
      poll_choices_after_pressing("multiple".into(), vec![2], 3, false, 0),
      Some(vec![0, 2])
    );
    assert_eq!(
      poll_choices_after_pressing("single".into(), vec![], 3, true, 0),
      None
    );
    assert_eq!(
      poll_choices_after_pressing("ranked".into(), vec![], 3, false, 0),
      None
    );
    assert_eq!(
      poll_choices_after_pressing("single".into(), vec![], 3, false, 3),
      None
    );
  }

  #[test]
  fn shares_round_down() {
    assert_eq!(poll_share_percent(2, 3), 66);
    assert_eq!(poll_share_percent(1, 0), 0);
  }

  fn draft(question: &str, options: &[&str]) -> PollDraftItem {
    PollDraftItem {
      question: question.to_owned(),
      options: options.iter().map(|option| (*option).to_owned()).collect(),
      is_multiple_choice: false,
      hides_voters: false,
      results: PollResultsItem::Always,
      maybe_closes_at_ms: None,
    }
  }
}
