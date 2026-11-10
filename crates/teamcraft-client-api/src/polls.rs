//! Polls in conversations (issue #205, e2ee.md §4.12): the view a poll message shows, what the user
//! sends to make one, and the rules every platform shares (what pressing an option sends, the text
//! older versions show, the shares of the bars).

use serde::{Deserialize, Serialize};
pub use teamcraft_proto::{PollChoice, PollError, PollLimits, PollSpec, PollVoters};

use crate::model::UserId;

/// A poll as the lists show it, counted by this device from the encrypted votes it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollView {
  pub question: String,
  pub options: Vec<PollOptionView>,
  pub choice: PollChoice,
  pub voters: PollVoters,
  /// People whose vote counts.
  pub voter_count: u32,
  pub is_closed: bool,
  /// The user made it, so may close it.
  pub is_mine: bool,
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

/// A poll the user sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPoll {
  pub spec: PollSpec,
  /// The message's text, which only versions before polls show: [`fallback_text`] in the sender's
  /// language.
  pub fallback: String,
}

impl PollView {
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
    if self.voter_count == 0 {
      return 0;
    }
    let share = u64::from(option.count) * 100 / u64::from(self.voter_count);
    u32::try_from(share.min(100)).unwrap_or(100)
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

  #[test]
  fn older_versions_read_the_poll_as_text() {
    let spec = PollSpec {
      question: " Lunch on Thursday? ".into(),
      options: vec!["Pizza".into(), "Salad".into()],
      choice: PollChoice::Single,
      voters: PollVoters::Shown,
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
      voter_count: 1,
      is_closed: false,
      is_mine: false,
    }
  }
}
