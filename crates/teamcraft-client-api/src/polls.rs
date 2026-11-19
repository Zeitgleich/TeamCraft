//! Polls in conversations (issue #205, e2ee.md §4.12): the view a poll message shows, what the user
//! sends to make one, and the rules every platform shares (what pressing an option sends, the text
//! older versions show, the shares of the bars).

use serde::{Deserialize, Serialize};
pub use teamcraft_proto::{PollChoice, PollError, PollLimits, PollResults, PollSpec, PollVoters};

use crate::model::UserId;

/// A poll as the lists show it, counted by this device from the encrypted votes it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollView {
  pub question: String,
  pub options: Vec<PollOptionView>,
  pub choice: PollChoice,
  pub voters: PollVoters,
  /// When people see the counts.
  #[serde(default, skip_serializing_if = "PollResults::is_always")]
  pub results: PollResults,
  /// Whether the user sees the counts now ([`PollResults::are_shown`]). While not, every option's
  /// `count` is 0 and `voters` empty, so hidden counts never reach the UI; `voter_count` and the
  /// user's own `is_chosen` still are.
  #[serde(default = "shown")]
  pub are_results_shown: bool,
  /// When the poll closes by itself (Unix milliseconds; the server's time decides).
  #[serde(rename = "closes_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_closes_at_ms: Option<i64>,
  /// People whose vote counts.
  pub voter_count: u32,
  /// Closed by its creator, or its closing time passed.
  pub is_closed: bool,
  /// The user made it.
  pub is_mine: bool,
  /// The user may close it: they made it, or they are a moderator or admin of the channel by its
  /// signed roles (issue #2120). The action is offered only then, and only while it is open.
  #[serde(default)]
  pub can_close: bool,
}

/// One option of a [`PollView`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOptionView {
  pub text: String,
  /// People who chose it.
  pub count: u32,
  /// The user chose it.
  pub is_chosen: bool,
  /// Who chose it, oldest vote first, when the poll shows voters ([`PollVoters::Shown`]); always
  /// empty otherwise, so a hidden poll's names never reach the UI.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub voters: Vec<UserId>,
}

/// What the poll dialog holds, as typed, on every platform: options in their fields (empty ones
/// are left out of the poll) and the closing time already turned into Unix milliseconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollDraft {
  pub question: String,
  pub options: Vec<String>,
  pub choice: PollChoice,
  pub voters: PollVoters,
  pub results: PollResults,
  #[serde(rename = "closes_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_closes_at_ms: Option<i64>,
}

/// A field of the poll dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PollField {
  Question,
  /// The option field at this position, as typed (counting empty fields).
  Option(usize),
  Closing,
}

/// One problem of a [`PollDraft`], at its field. The `error`'s option index is the field's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollProblem {
  pub field: PollField,
  pub error: PollError,
}

/// A poll the user sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPoll {
  pub spec: PollSpec,
  /// The message's text, which only versions before polls show: [`fallback_text`] in the sender's
  /// language.
  pub fallback: String,
}

impl PollView {
  /// Whether to offer closing the poll now: the user may ([`Self::can_close`]) and it is open.
  pub fn is_closable(&self) -> bool {
    self.can_close && !self.is_closed
  }

  /// Whether the user can vote (or change or withdraw a vote): the poll is open and its choice is
  /// one this version knows.
  pub fn can_vote(&self) -> bool {
    !self.is_closed && !matches!(self.choice, PollChoice::Unknown(_))
  }

  /// The options the user chose, by position.
  pub fn my_choices(&self) -> Vec<u32> {
    self
      .options
      .iter()
      .enumerate()
      .filter(|(_, option)| option.is_chosen)
      .filter_map(|(at, _)| u32::try_from(at).ok())
      .collect()
  }

  /// The vote pressing `option` sends: with a single choice it picks that option, or withdraws
  /// the vote when it was the one chosen; with several it adds or takes away that option. `None`
  /// when the poll takes no vote now.
  pub fn choices_after_pressing(&self, option: u32) -> Option<Vec<u32>> {
    if !self.can_vote() || usize::try_from(option).map_or(true, |at| at >= self.options.len()) {
      return None;
    }
    let mine = self.my_choices();
    let is_chosen = mine.contains(&option);
    Some(match self.choice {
      PollChoice::Single if is_chosen => Vec::new(),
      PollChoice::Single => vec![option],
      _ if is_chosen => mine.into_iter().filter(|chosen| *chosen != option).collect(),
      _ => {
        let mut choices = mine;
        choices.push(option);
        choices.sort_unstable();
        choices
      },
    })
  }

  /// An option's share of the voters, in whole percent (rounded down, so shares never add up to
  /// more than 100 with a single choice). With several choices shares may add up to more.
  pub fn share_percent(&self, option: &PollOptionView) -> u32 {
    share_percent(option.count, self.voter_count)
  }

  /// The option with the most votes, if exactly one has the most (and any vote at all).
  pub fn leader(&self) -> Option<usize> {
    let most = self
      .options
      .iter()
      .map(|option| option.count)
      .max()
      .filter(|most| *most > 0)?;
    let mut leaders = self
      .options
      .iter()
      .enumerate()
      .filter(|(_, option)| option.count == most);
    let (at, _) = leaders.next()?;
    leaders.next().is_none().then_some(at)
  }
}

impl PollDraft {
  /// The poll to send: question and options trimmed, empty option fields left out.
  pub fn spec(&self) -> PollSpec {
    PollSpec {
      maybe_closes_at_ms: self.maybe_closes_at_ms,
      results: self.results.clone(),
      ..PollSpec::from_input(
        &self.question,
        &self.options,
        self.choice.clone(),
        self.voters.clone(),
      )
    }
  }

  /// Every problem at once, at its field, in the order of the form (forms.md): the question, the
  /// option fields top down, the closing time. `now_ms` is the sender's best idea of the time.
  pub fn problems(&self, limits: &PollLimits, now_ms: i64) -> Vec<PollProblem> {
    let mut problems = self.text_problems(limits);
    if let Some(error) = self
      .maybe_closes_at_ms
      .and_then(|closes_at| teamcraft_proto::closing_problem(closes_at, limits, now_ms))
    {
      problems.push(PollProblem {
        field: PollField::Closing,
        error,
      });
    }
    problems.sort_by_key(|problem| problem.field);
    problems
  }

  /// The question's and options' problems. [`PollSpec::check`] names the first one; setting it
  /// right and checking again finds the next.
  fn text_problems(&self, limits: &PollLimits) -> Vec<PollProblem> {
    let mut problems = Vec::new();
    // The typed field each option of the spec came from (empty fields are left out of it).
    let fields: Vec<usize> = self
      .options
      .iter()
      .enumerate()
      .filter(|(_, option)| !option.trim().is_empty())
      .map(|(at, _)| at)
      .collect();
    let field_of = |index: usize| fields.get(index).copied().unwrap_or(index);
    let mut checked = self.spec();
    for _ in 0..=self.options.len() + 1 {
      let Err(found) = checked.check(limits) else {
        break;
      };
      let (field, error) = match found {
        PollError::EmptyQuestion | PollError::QuestionTooLong { .. } => (PollField::Question, found),
        PollError::TooFewOptions { .. } => {
          let first_empty = self
            .options
            .iter()
            .position(|option| option.trim().is_empty())
            .unwrap_or(self.options.len().saturating_sub(1));
          (PollField::Option(first_empty), found)
        },
        PollError::TooManyOptions { .. } => (PollField::Option(self.options.len().saturating_sub(1)), found),
        PollError::EmptyOption { index } => {
          let field = field_of(index);
          (PollField::Option(field), PollError::EmptyOption { index: field })
        },
        PollError::OptionTooLong { index, max } => {
          let field = field_of(index);
          (
            PollField::Option(field),
            PollError::OptionTooLong { index: field, max },
          )
        },
        PollError::DuplicateOption { index } => {
          let field = field_of(index);
          (
            PollField::Option(field),
            PollError::DuplicateOption { index: field },
          )
        },
        PollError::ClosingPassed | PollError::ClosingTooFar { .. } => (PollField::Closing, found),
      };
      problems.push(PollProblem { field, error });
      // The spec's own index, not the field's: settling sets right the option the check named.
      if !settle(&mut checked, found) {
        break;
      }
    }
    problems
  }
}

/// Sets right what `error` names in `spec`, so the next check finds the next problem. `false` when
/// nothing further can be found.
fn settle(spec: &mut PollSpec, error: PollError) -> bool {
  match error {
    PollError::EmptyQuestion | PollError::QuestionTooLong { .. } => {
      "?".clone_into(&mut spec.question);
      true
    },
    PollError::OptionTooLong { index, .. }
    | PollError::DuplicateOption { index }
    | PollError::EmptyOption { index } => {
      if let Some(option) = spec.options.get_mut(index) {
        *option = format!("\u{0}{index}");
      }
      true
    },
    PollError::TooFewOptions { .. }
    | PollError::TooManyOptions { .. }
    | PollError::ClosingPassed
    | PollError::ClosingTooFar { .. } => false,
  }
}

/// `count` people's share of `voter_count`, in whole percent, rounded down (so shares never add up
/// to more than 100 with a single choice). 0 without voters.
pub fn share_percent(count: u32, voter_count: u32) -> u32 {
  if voter_count == 0 {
    return 0;
  }
  let share = u64::from(count) * 100 / u64::from(voter_count);
  u32::try_from(share.min(100)).unwrap_or(100)
}

/// Serde's default of [`PollView::are_results_shown`]: a view without the field shows counts.
fn shown() -> bool {
  true
}

/// The text versions before polls show for `spec`, as Markdown: `title` ("Poll") and the
/// question, the options as a list, and `note` (where to vote) below.
pub fn fallback_text(spec: &PollSpec, title: &str, note: &str) -> String {
  let mut text = format!("**{title}:** {}\n", spec.question.trim());
  for option in &spec.options {
    text.push_str("- ");
    text.push_str(option.trim());
    text.push('\n');
  }
  text.push('\n');
  text.push('_');
  text.push_str(note);
  text.push('_');
  text
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  mod pressing {
    use super::*;

    #[test]
    fn a_single_choice_picks_switches_and_withdraws() {
      let mut poll = view(PollChoice::Single, &[false, false, false]);
      assert_eq!(poll.choices_after_pressing(1), Some(vec![1]));
      poll.options[1].is_chosen = true;
      assert_eq!(poll.choices_after_pressing(2), Some(vec![2]));
      assert_eq!(poll.choices_after_pressing(1), Some(Vec::new()));
    }

    #[test]
    fn a_multiple_choice_adds_and_takes_away() {
      let mut poll = view(PollChoice::Multiple, &[false, true, false]);
      assert_eq!(poll.choices_after_pressing(2), Some(vec![1, 2]));
      assert_eq!(poll.choices_after_pressing(0), Some(vec![0, 1]));
      poll.options[2].is_chosen = true;
      assert_eq!(poll.choices_after_pressing(1), Some(vec![2]));
    }

    #[test]
    fn a_closed_poll_an_unknown_choice_or_option_sends_nothing() {
      let mut poll = view(PollChoice::Single, &[false, false]);
      assert_eq!(poll.choices_after_pressing(2), None);
      poll.is_closed = true;
      assert_eq!(poll.choices_after_pressing(0), None);
      let ranked = view(PollChoice::Unknown("ranked".into()), &[false, false]);
      assert!(!ranked.can_vote());
      assert_eq!(ranked.choices_after_pressing(0), None);
    }
  }

  mod results {
    use super::*;

    #[test]
    fn shares_are_whole_percent_of_the_voters() {
      let mut poll = view(PollChoice::Single, &[false, false, false]);
      poll.voter_count = 3;
      poll.options[0].count = 2;
      poll.options[1].count = 1;
      assert_eq!(poll.share_percent(&poll.options[0]), 66);
      assert_eq!(poll.share_percent(&poll.options[1]), 33);
      assert_eq!(poll.share_percent(&poll.options[2]), 0);
      poll.voter_count = 0;
      assert_eq!(poll.share_percent(&poll.options[0]), 0);
    }

    #[test]
    fn a_leader_is_only_named_without_a_tie() {
      let mut poll = view(PollChoice::Multiple, &[false, false, false]);
      assert_eq!(poll.leader(), None);
      poll.options[1].count = 2;
      assert_eq!(poll.leader(), Some(1));
      poll.options[2].count = 2;
      assert_eq!(poll.leader(), None);
    }

    #[test]
    fn a_hidden_poll_serializes_without_names() {
      let mut poll = view(PollChoice::Single, &[true, false]);
      poll.voters = PollVoters::Hidden;
      poll.options[0].count = 1;
      let json = serde_json::to_string(&poll).unwrap();
      assert!(!json.contains("\"voters\":["), "{json}");
      poll.voters = PollVoters::Shown;
      poll.options[0].voters = vec![id_from_raw(7)];
      let json = serde_json::to_string(&poll).unwrap();
      assert!(json.contains("\"voters\":[\"7\"]"), "{json}");
    }
  }

  mod drafts {
    use super::*;

    const NOW_MS: i64 = 1_800_000_000_000;

    #[test]
    fn a_complete_draft_has_no_problems() {
      assert!(
        draft("Lunch?", &["Pizza", "", "Salad"])
          .problems(&PollLimits::default(), NOW_MS)
          .is_empty()
      );
    }

    #[test]
    fn problems_land_on_the_fields_as_typed() {
      let problems = draft(" ", &["Pizza", "", "pizza"]).problems(&PollLimits::default(), NOW_MS);
      assert_eq!(
        problems,
        [
          PollProblem {
            field: PollField::Question,
            error: PollError::EmptyQuestion
          },
          PollProblem {
            field: PollField::Option(2),
            error: PollError::DuplicateOption { index: 2 }
          },
        ]
      );
    }

    #[test]
    fn too_few_options_point_at_the_first_empty_field() {
      let problems = draft("Lunch?", &["Pizza", " "]).problems(&PollLimits::default(), NOW_MS);
      assert_eq!(problems[0].field, PollField::Option(1));
      assert_eq!(problems[0].error, PollError::TooFewOptions { min: 2 });
    }

    #[test]
    fn each_long_field_is_named() {
      let limits = PollLimits {
        max_option_chars: 3,
        ..PollLimits::default()
      };
      let problems = draft("Q", &["Long", "No", "Longer"]).problems(&limits, NOW_MS);
      let fields: Vec<PollField> = problems.iter().map(|problem| problem.field).collect();
      assert_eq!(fields, [PollField::Option(0), PollField::Option(2)]);
    }

    #[test]
    fn a_closing_time_in_the_past_is_named_last_with_the_others() {
      let mut typed = draft(" ", &["A", "B"]);
      typed.maybe_closes_at_ms = Some(NOW_MS - 1);
      let fields: Vec<PollField> = typed
        .problems(&PollLimits::default(), NOW_MS)
        .iter()
        .map(|problem| problem.field)
        .collect();
      assert_eq!(fields, [PollField::Question, PollField::Closing]);
    }

    #[test]
    fn the_spec_carries_every_choice_of_the_dialog() {
      let mut typed = draft("Q", &["A", "", "B"]);
      typed.choice = PollChoice::Multiple;
      typed.voters = PollVoters::Hidden;
      typed.results = PollResults::AfterVote;
      typed.maybe_closes_at_ms = Some(NOW_MS + 1);
      let spec = typed.spec();
      assert_eq!(spec.options, ["A", "B"]);
      assert_eq!(spec.choice, PollChoice::Multiple);
      assert_eq!(spec.voters, PollVoters::Hidden);
      assert_eq!(spec.results, PollResults::AfterVote);
      assert_eq!(spec.maybe_closes_at_ms, Some(NOW_MS + 1));
    }

    fn draft(question: &str, options: &[&str]) -> PollDraft {
      PollDraft {
        question: question.to_owned(),
        options: options.iter().map(|option| (*option).to_owned()).collect(),
        choice: PollChoice::Single,
        voters: PollVoters::Shown,
        results: PollResults::Always,
        maybe_closes_at_ms: None,
      }
    }
  }

  #[test]
  fn a_view_without_the_newer_fields_shows_counts() {
    let json = r#"{"question":"Q","options":[],"choice":"single","voters":"shown","voter_count":0,"is_closed":false,"is_mine":false}"#;
    let poll: PollView = serde_json::from_str(json).unwrap();
    assert!(poll.are_results_shown);
    assert_eq!(poll.results, PollResults::Always);
    assert_eq!(poll.maybe_closes_at_ms, None);
  }

  #[test]
  fn older_versions_read_the_poll_as_text() {
    let spec = PollSpec {
      question: " Lunch on Thursday? ".into(),
      options: vec!["Pizza".into(), "Salad".into()],
      choice: PollChoice::Single,
      voters: PollVoters::Shown,
      maybe_closes_at_ms: None,
      results: PollResults::Always,
    };
    assert_eq!(
      fallback_text(&spec, "Poll", "Update TeamCraft to vote."),
      "**Poll:** Lunch on Thursday?\n- Pizza\n- Salad\n\n_Update TeamCraft to vote._"
    );
  }

  fn view(choice: PollChoice, chosen: &[bool]) -> PollView {
    PollView {
      question: "Q".into(),
      options: chosen
        .iter()
        .enumerate()
        .map(|(at, is_chosen)| PollOptionView {
          text: format!("Option {at}"),
          count: u32::from(*is_chosen),
          is_chosen: *is_chosen,
          voters: Vec::new(),
        })
        .collect(),
      choice,
      voters: PollVoters::Shown,
      results: PollResults::Always,
      are_results_shown: true,
      maybe_closes_at_ms: None,
      voter_count: 1,
      is_closed: false,
      is_mine: false,
      can_close: false,
    }
  }
}
