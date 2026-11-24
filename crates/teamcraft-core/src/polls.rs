//! Counting polls (issue #205, e2ee.md §4.12). A poll is a text message carrying a
//! [`PollSpec`](teamcraft_proto::PollSpec); votes and the close are hidden encrypted messages
//! naming it, like reactions. Every device counts for itself from the messages it holds, so the
//! server learns nothing it doesn't learn from any message.
//!
//! The count goes by the server's order (message IDs), never by arrival or a sender's clock, so
//! every device that holds the same messages shows the same result:
//!
//! - Each person's newest valid vote counts; no choices withdraws it. A vote the poll doesn't take
//!   (an option it lacks, several for a single choice) is skipped, and the earlier one stands.
//! - The first close by the poll's sender, or by a moderator or admin of the channel by its signed
//!   roles (issue #2120), ends it: votes after it don't count. A close by anybody else changes
//!   nothing.
//! - A poll with a closing time takes only votes the server took at or before it (the time in the
//!   vote's server ID): the server's clock decides, never a device's. This device shows the poll
//!   closed once its own clock passed the time or it holds an event the server took later.
//! - A vote still in this device's outbox counts as the newest, until the server orders it, unless
//!   the poll is closed.
//! - Counts show as the poll's [`PollResults`](teamcraft_proto::PollResults) promise; while they
//!   don't, the view carries none.

use std::collections::{BTreeSet, HashMap};

use teamcraft_client_api::polls::{PollOptionView, PollView};
use teamcraft_proto::{ClientMessageId, Id, PollLimits, PollResults, PollSpec, PollVoters, UserId};

pub use crate::anonymous_poll::{
  AnonymousPoll, BallotBoxRecord, anonymous_poll_of, ballot_scope, count_ballots, counted_ballot_box,
  store_ballot_box, stored_ballot_box,
};
use crate::store::{LocalStore, MessageContent, MessageOrder, StoreError, StoredMessage};

/// The poll limits in force: what the server published, or the built-in ones. See
/// [`crate::client::stored_poll_limits`].
pub fn stored_limits(store: &impl LocalStore) -> PollLimits {
  crate::client::stored_poll_limits(store)
}

/// The poll `message` carries, if it is one this device counts: a readable text message with a
/// poll within the bounds every receiver applies, named (`poll`) or anonymous (`anonymous_poll`,
/// [`anonymous_poll_of`]). Anything else shows as its text. A named poll claiming to be anonymous
/// isn't one: it has no ballot key, and its signed votes would name their voters.
pub fn poll_of(message: &StoredMessage) -> Option<&PollSpec> {
  if message.maybe_unreadable.is_some() {
    return None;
  }
  let MessageContent::Text { meta, .. } = &message.content else {
    return None;
  };
  match (&meta.maybe_poll, &meta.maybe_anonymous_poll) {
    (Some(spec), _) => {
      Some(spec.as_ref()).filter(|spec| spec.is_receivable() && spec.voters != PollVoters::Anonymous)
    },
    (None, Some(anonymous)) => anonymous.counted_spec(),
    (None, None) => None,
  }
}

/// The poll `poll` (whose spec is `spec`, see [`poll_of`]) as `me` sees it at `now_ms` (this
/// device's clock, only for showing a closing time as passed), counted from `related`: any
/// messages of the conversation; those that aren't a vote or close of this poll are passed over.
/// `moderators` are the people besides the poll's sender whose close counts: the channel's
/// moderators and admins by the roles this device verified (`ChannelRoleState::moderators`), never
/// the server's list.
pub fn tally<'a>(
  poll: &StoredMessage,
  spec: &PollSpec,
  related: impl IntoIterator<Item = &'a StoredMessage>,
  me: UserId,
  moderators: &BTreeSet<UserId>,
  now_ms: i64,
) -> PollView {
  let mut events: Vec<&StoredMessage> = related
    .into_iter()
    .filter(|message| message.maybe_unreadable.is_none() && is_event_of(message, poll.client_message_id))
    .collect();
  events.sort_by_key(|message| message.order());
  let maybe_closed_at = closed_at(poll, &events, moderators);
  let is_past_closing = spec.maybe_closes_at_ms.is_some_and(|closes_at| {
    closes_at <= now_ms
      || events
        .iter()
        .any(|message| server_time_ms(message).is_some_and(|taken_at| taken_at > closes_at))
  });
  let cut = Cut {
    maybe_closed_at,
    maybe_closes_at_ms: spec.maybe_closes_at_ms,
    is_past_closing,
  };
  let votes = counted_votes(spec, &events, &cut);
  let is_closed = maybe_closed_at.is_some() || is_past_closing;
  let has_voted = votes.iter().any(|(voter, _)| *voter == me);
  let are_results_shown = spec.results.are_shown(has_voted, is_closed);
  let mut options: Vec<PollOptionView> = spec
    .options
    .iter()
    .map(|text| PollOptionView {
      text: text.clone(),
      count: 0,
      is_chosen: false,
      voters: Vec::new(),
    })
    .collect();
  for (voter, choices) in &votes {
    for choice in choices {
      // Invariant: `counted_votes` keeps only choices of existing options.
      let Some(option) = usize::try_from(*choice).ok().and_then(|at| options.get_mut(at)) else {
        continue;
      };
      option.is_chosen |= *voter == me;
      if !are_results_shown {
        continue;
      }
      option.count += 1;
      if spec.voters.are_shown() {
        option.voters.push(*voter);
      }
    }
  }
  PollView {
    question: spec.question.clone(),
    options,
    choice: spec.choice.clone(),
    voters: spec.voters.clone(),
    results: spec.results.clone(),
    are_results_shown,
    maybe_closes_at_ms: spec.maybe_closes_at_ms,
    voter_count: u32::try_from(votes.len()).unwrap_or(u32::MAX),
    is_closed,
    is_mine: poll.sender_user_id == me,
    can_close: poll.sender_user_id == me || moderators.contains(&me),
  }
}

/// The anonymous poll `poll` as `me` sees it at `now_ms`, from what this device read of its box.
/// Counts only once the box is closed; never any names.
pub fn anonymous_tally(
  poll: &StoredMessage,
  spec: &PollSpec,
  maybe_box: Option<&BallotBoxRecord>,
  me: UserId,
  moderators: &BTreeSet<UserId>,
  now_ms: i64,
) -> PollView {
  let record = maybe_box.cloned().unwrap_or_default();
  let is_past_closing = spec
    .maybe_closes_at_ms
    .is_some_and(|closes_at| closes_at <= now_ms);
  let is_closed = record.is_closed || is_past_closing;
  let are_results_shown = record.is_closed && record.maybe_counts.is_some();
  let counts = record.maybe_counts.clone().unwrap_or_default();
  let options = spec
    .options
    .iter()
    .enumerate()
    .map(|(at, text)| PollOptionView {
      text: text.clone(),
      count: if are_results_shown {
        counts.get(at).copied().unwrap_or(0)
      } else {
        0
      },
      is_chosen: u32::try_from(at).is_ok_and(|at| record.my_choices.contains(&at)),
      voters: Vec::new(),
    })
    .collect();
  PollView {
    question: spec.question.clone(),
    options,
    // A vote before the sender's device opened the box waits in the queue until it is open.
    choice: spec.choice.clone(),
    voters: PollVoters::Anonymous,
    results: PollResults::AfterClose,
    are_results_shown,
    maybe_closes_at_ms: spec.maybe_closes_at_ms,
    voter_count: if are_results_shown {
      record.counted
    } else {
      record.ballot_count
    },
    is_closed,
    is_mine: poll.sender_user_id == me,
    can_close: poll.sender_user_id == me || moderators.contains(&me),
  }
}

/// Where a poll stops taking votes.
struct Cut {
  /// The creator's close among the events.
  maybe_closed_at: Option<MessageOrder>,
  /// The poll's closing time.
  maybe_closes_at_ms: Option<i64>,
  /// This device takes the closing time as passed.
  is_past_closing: bool,
}

impl Cut {
  /// Whether the vote `message` came too late: after the creator's close, taken by the server
  /// after the closing time, or still unsent when the closing time passed.
  fn excludes(&self, message: &StoredMessage) -> bool {
    if self
      .maybe_closed_at
      .is_some_and(|closed| message.order() > closed)
    {
      return true;
    }
    match (self.maybe_closes_at_ms, server_time_ms(message)) {
      (Some(closes_at), Some(taken_at)) => taken_at > closes_at,
      (Some(_), None) => self.is_past_closing,
      (None, _) => false,
    }
  }
}

/// When the server took `message`: the time in its server ID. `None` while it is in the outbox.
fn server_time_ms(message: &StoredMessage) -> Option<i64> {
  message.maybe_server_id.map(|id| id.snowflake().unix_millis())
}

/// The poll `poll` of `conversation` as `me` sees it at `now_ms`, from what `store` holds (its
/// index of reactions holds the votes and closes too). `None` when the message isn't held or isn't
/// a poll.
pub fn stored_poll(
  store: &impl LocalStore,
  conversation: teamcraft_proto::ConversationId,
  poll: ClientMessageId,
  me: UserId,
  moderators: &BTreeSet<UserId>,
  now_ms: i64,
) -> Result<Option<PollView>, StoreError> {
  let Some(message) = store.message(conversation, poll)? else {
    return Ok(None);
  };
  let Some(spec) = poll_of(&message) else {
    return Ok(None);
  };
  let related = store.reactions_to(conversation, &[poll])?;
  Ok(Some(tally(&message, spec, &related, me, moderators, now_ms)))
}

/// Whether `message` votes in or closes the poll `poll`.
fn is_event_of(message: &StoredMessage, poll: ClientMessageId) -> bool {
  matches!(
    &message.content,
    MessageContent::PollVote { target, .. } | MessageContent::PollClose { target } if *target == poll
  )
}

/// Where the first close by the poll's sender or a moderator is among `events` (sorted), if any.
fn closed_at(
  poll: &StoredMessage,
  events: &[&StoredMessage],
  moderators: &BTreeSet<UserId>,
) -> Option<MessageOrder> {
  events
    .iter()
    .find(|message| {
      matches!(message.content, MessageContent::PollClose { .. })
        && (message.sender_user_id == poll.sender_user_id || moderators.contains(&message.sender_user_id))
    })
    .map(|message| message.order())
}

/// Each voter's counted choices, in the order of their first counted vote (so names list the
/// earliest voters first).
fn counted_votes(spec: &PollSpec, events: &[&StoredMessage], cut: &Cut) -> Vec<(UserId, Vec<u32>)> {
  let mut votes: Vec<(UserId, Vec<u32>)> = Vec::new();
  let mut at_of: HashMap<UserId, usize> = HashMap::new();
  for message in events {
    let MessageContent::PollVote { choices, .. } = &message.content else {
      continue;
    };
    if cut.excludes(message) || !spec.choice.allows(choices, spec.options.len()) {
      continue;
    }
    let voter = message.sender_user_id;
    match at_of.get(&voter) {
      Some(&at) => votes[at].1.clone_from(choices),
      None => {
        at_of.insert(voter, votes.len());
        votes.push((voter, choices.clone()));
      },
    }
  }
  votes.retain(|(_, choices)| !choices.is_empty());
  votes
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{PollChoice, PollResults, PollVoters, SNOWFLAKE_EPOCH_MS};

  use super::*;
  use crate::store::{MessageMeta, Unreadable};

  const CONVERSATION: u64 = 10;
  const ALICE: u64 = 1;
  const BOB: u64 = 2;
  const CAROL: u64 = 3;
  /// This device's clock in the tests: an hour after the snowflake epoch.
  const NOW_MS: i64 = SNOWFLAKE_EPOCH_MS + HOUR_MS;
  const HOUR_MS: i64 = 3_600_000;

  mod counting {
    use super::*;

    #[test]
    fn each_persons_newest_vote_counts() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(CAROL, 101, &[1]), vote(BOB, 102, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![0, 1, 1]);
      assert_eq!(view.voter_count, 2);
      assert!(view.options[2].is_chosen && !view.options[0].is_chosen);
      assert_eq!(view.options[2].voters, vec![id(BOB)]);
    }

    #[test]
    fn no_choices_withdraw_a_vote() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(BOB, 101, &[])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![0, 0, 0]);
      assert_eq!(view.voter_count, 0);
    }

    #[test]
    fn a_multiple_choice_counts_each_option_once_per_person() {
      let poll = poll(PollChoice::Multiple, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0, 2]), vote(CAROL, 101, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![1, 0, 2]);
      assert_eq!(view.voter_count, 2);
      assert_eq!(view.options[2].voters, vec![id(BOB), id(CAROL)]);
    }

    #[test]
    fn a_vote_the_poll_does_not_take_leaves_the_earlier_one() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [
        vote(BOB, 100, &[1]),
        vote(BOB, 101, &[0, 2]),
        vote(BOB, 102, &[7]),
        vote(BOB, 103, &[1, 1]),
      ];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![0, 1, 0]);
    }

    #[test]
    fn the_result_is_the_same_in_any_order_of_arrival() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let mut events = vec![
        vote(BOB, 100, &[0]),
        vote(CAROL, 101, &[1]),
        vote(BOB, 102, &[1]),
        close(ALICE, 103),
        vote(CAROL, 104, &[2]),
      ];
      let expected = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert_eq!(counts(&expected), vec![0, 2, 0]);
      events.reverse();
      assert_eq!(
        tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS),
        expected
      );
      events.swap(0, 3);
      assert_eq!(
        tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS),
        expected
      );
    }

    #[test]
    fn a_replayed_vote_does_not_count_twice() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(BOB, 100, &[0]), vote(BOB, 101, &[0])];
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![1, 0, 0]);
    }

    #[test]
    fn votes_of_other_polls_and_unreadable_ones_are_passed_over() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let mut other = vote(BOB, 100, &[0]);
      other.content = MessageContent::PollVote {
        target: ClientMessageId::from_bytes([9; 16]),
        choices: vec![0],
      };
      let mut unreadable = vote(CAROL, 101, &[1]);
      unreadable.maybe_unreadable = Some(Unreadable::Failed);
      let view = tally(
        &poll,
        spec(&poll),
        &[other, unreadable],
        id(ALICE),
        &no_moderators(),
        NOW_MS,
      );
      assert_eq!(view.voter_count, 0);
    }
  }

  mod closing {
    use super::*;

    #[test]
    fn votes_after_the_creators_close_do_not_count() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [
        vote(BOB, 100, &[0]),
        close(ALICE, 101),
        vote(BOB, 102, &[1]),
        vote(CAROL, 103, &[2]),
      ];
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert!(view.is_closed);
      assert_eq!(counts(&view), vec![1, 0, 0]);
    }

    #[test]
    fn only_the_creator_closes() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [close(BOB, 100), vote(CAROL, 101, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert!(!view.is_closed);
      assert!(!view.is_mine);
      assert_eq!(counts(&view), vec![0, 0, 1]);
    }

    #[test]
    fn a_moderators_close_counts_as_the_creators_does() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), close(CAROL, 101), vote(BOB, 102, &[1])];
      let moderators = BTreeSet::from([id(CAROL)]);
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &moderators, NOW_MS);
      assert!(view.is_closed);
      assert_eq!(counts(&view), vec![1, 0, 0]);
      let without = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert!(!without.is_closed, "the same close from a member changes nothing");
    }

    #[test]
    fn only_the_creator_and_moderators_are_offered_the_close() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let moderators = BTreeSet::from([id(CAROL)]);
      let can_close = |me: u64| tally(&poll, spec(&poll), &[], id(me), &moderators, NOW_MS).can_close;
      assert!(can_close(ALICE), "the creator");
      assert!(can_close(CAROL), "a moderator");
      assert!(!can_close(BOB), "any other member");
    }

    #[test]
    fn the_first_close_by_either_decides_where_votes_stop() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [
        vote(BOB, 100, &[0]),
        close(BOB, 101),
        close(CAROL, 102),
        vote(BOB, 103, &[1]),
        close(ALICE, 104),
      ];
      let moderators = BTreeSet::from([id(CAROL)]);
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &moderators, NOW_MS);
      assert_eq!(
        counts(&view),
        vec![1, 0, 0],
        "Bob's close was nobody's, Carol's counts"
      );
    }

    #[test]
    fn a_vote_still_in_the_outbox_counts_until_a_close_comes_first() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let mut pending = vote(BOB, 0, &[1]);
      pending.maybe_server_id = None;
      let view = tally(
        &poll,
        spec(&poll),
        [&vote(BOB, 100, &[0]), &pending],
        id(BOB),
        &no_moderators(),
        NOW_MS,
      );
      assert_eq!(counts(&view), vec![0, 1, 0]);
      let view = tally(
        &poll,
        spec(&poll),
        [&close(ALICE, 101), &pending],
        id(BOB),
        &no_moderators(),
        NOW_MS,
      );
      assert_eq!(view.voter_count, 0);
    }
  }

  mod closing_time {
    use super::*;

    #[test]
    fn votes_the_server_took_after_the_closing_time_do_not_count() {
      let poll = closing(PollResults::Always, NOW_MS + HOUR_MS);
      let events = [
        vote_at(BOB, NOW_MS + HOUR_MS, &[0]),
        vote_at(CAROL, NOW_MS + HOUR_MS + 1, &[1]),
        vote_at(BOB, NOW_MS + HOUR_MS + 2, &[2]),
      ];
      let view = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![1, 0, 0]);
      assert!(
        view.is_closed,
        "a later event proves the server's clock passed the time"
      );
    }

    #[test]
    fn a_device_clock_ahead_closes_the_view_but_not_the_count() {
      let poll = closing(PollResults::Always, NOW_MS + HOUR_MS);
      let events = [vote_at(BOB, NOW_MS + 10, &[0])];
      let view = tally(
        &poll,
        spec(&poll),
        &events,
        id(ALICE),
        &no_moderators(),
        NOW_MS + 2 * HOUR_MS,
      );
      assert!(view.is_closed && !view.can_vote());
      assert_eq!(counts(&view), vec![1, 0, 0]);
    }

    #[test]
    fn a_device_clock_behind_does_not_count_a_late_vote() {
      let poll = closing(PollResults::Always, NOW_MS);
      let events = [vote_at(BOB, NOW_MS + 1, &[0])];
      let view = tally(
        &poll,
        spec(&poll),
        &events,
        id(BOB),
        &no_moderators(),
        NOW_MS - HOUR_MS,
      );
      assert_eq!(view.voter_count, 0);
      assert!(view.is_closed);
    }

    #[test]
    fn an_unsent_vote_counts_only_until_the_closing_time() {
      let poll = closing(PollResults::Always, NOW_MS + HOUR_MS);
      let mut pending = vote(BOB, 0, &[1]);
      pending.maybe_server_id = None;
      let open = tally(&poll, spec(&poll), [&pending], id(BOB), &no_moderators(), NOW_MS);
      assert_eq!(counts(&open), vec![0, 1, 0]);
      let closed = tally(
        &poll,
        spec(&poll),
        [&pending],
        id(BOB),
        &no_moderators(),
        NOW_MS + HOUR_MS,
      );
      assert_eq!(closed.voter_count, 0);
    }

    #[test]
    fn every_device_counts_the_same_whatever_its_clock() {
      let poll = closing(PollResults::Always, NOW_MS);
      let events = [vote_at(BOB, NOW_MS - 1, &[0]), vote_at(CAROL, NOW_MS + 1, &[1])];
      let behind = tally(
        &poll,
        spec(&poll),
        &events,
        id(ALICE),
        &no_moderators(),
        NOW_MS - HOUR_MS,
      );
      let ahead = tally(
        &poll,
        spec(&poll),
        &events,
        id(ALICE),
        &no_moderators(),
        NOW_MS + HOUR_MS,
      );
      assert_eq!(behind, ahead);
    }
  }

  mod results_after {
    use super::*;

    #[test]
    fn after_vote_shows_counts_once_the_user_voted() {
      let poll = with_results(PollResults::AfterVote);
      let events = [vote(BOB, 100, &[0]), vote(CAROL, 101, &[0])];
      let before = tally(&poll, spec(&poll), &events, id(ALICE), &no_moderators(), NOW_MS);
      assert!(!before.are_results_shown);
      assert_eq!(counts(&before), vec![0, 0, 0]);
      assert_eq!(before.voter_count, 2, "how many voted still shows");
      let voted = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert!(voted.are_results_shown);
      assert_eq!(counts(&voted), vec![2, 0, 0]);
      assert_eq!(voted.options[0].voters, vec![id(BOB), id(CAROL)]);
    }

    #[test]
    fn hidden_counts_keep_the_users_own_choice_and_name_nobody() {
      let poll = with_results(PollResults::AfterClose);
      let events = [vote(BOB, 100, &[1])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert!(!view.are_results_shown);
      assert!(view.options[1].is_chosen);
      assert!(
        view
          .options
          .iter()
          .all(|option| option.voters.is_empty() && option.count == 0)
      );
    }

    #[test]
    fn closing_shows_the_counts_to_everybody() {
      let poll = with_results(PollResults::AfterClose);
      let events = [vote(BOB, 100, &[1]), close(ALICE, 101)];
      let view = tally(&poll, spec(&poll), &events, id(CAROL), &no_moderators(), NOW_MS);
      assert!(view.are_results_shown);
      assert_eq!(counts(&view), vec![0, 1, 0]);
    }

    #[test]
    fn a_withdrawn_vote_hides_the_counts_again() {
      let poll = with_results(PollResults::AfterVote);
      let events = [vote(BOB, 100, &[1]), vote(BOB, 101, &[])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert!(!view.are_results_shown);
    }
  }

  mod hidden_voters {
    use super::*;

    #[test]
    fn a_hidden_poll_names_nobody_but_marks_the_users_own_choice() {
      let poll = poll(PollChoice::Single, PollVoters::Hidden);
      let events = [vote(BOB, 100, &[0]), vote(CAROL, 101, &[0])];
      let view = tally(&poll, spec(&poll), &events, id(BOB), &no_moderators(), NOW_MS);
      assert_eq!(counts(&view), vec![2, 0, 0]);
      assert!(view.options[0].is_chosen);
      assert!(view.options.iter().all(|option| option.voters.is_empty()));
    }

    #[test]
    fn a_mode_of_a_newer_version_names_nobody() {
      let poll = poll(PollChoice::Single, PollVoters::Unknown("blinded".into()));
      let view = tally(
        &poll,
        spec(&poll),
        &[vote(BOB, 100, &[0])],
        id(ALICE),
        &no_moderators(),
        NOW_MS,
      );
      assert!(view.options[0].voters.is_empty());
    }
  }

  mod recognizing {
    use super::*;

    #[test]
    fn only_a_readable_text_with_a_bounded_poll_is_one() {
      let mut message = poll(PollChoice::Single, PollVoters::Shown);
      assert!(poll_of(&message).is_some());
      message.maybe_unreadable = Some(Unreadable::Failed);
      assert!(poll_of(&message).is_none());
      let mut message = poll(PollChoice::Single, PollVoters::Shown);
      if let MessageContent::Text { meta, .. } = &mut message.content
        && let Some(spec) = meta.maybe_poll.as_mut()
      {
        spec.options.truncate(1);
      }
      assert!(poll_of(&message).is_none());
      let mut message = poll(PollChoice::Single, PollVoters::Shown);
      message.content = MessageContent::Deleted;
      assert!(poll_of(&message).is_none());
    }

    #[test]
    fn a_named_poll_claiming_to_be_anonymous_is_none() {
      // It has no ballot key, and its signed votes would name their voters (e2ee.md §4.12.1).
      let message = poll(PollChoice::Single, PollVoters::Anonymous);
      assert!(poll_of(&message).is_none());
      assert!(anonymous_poll_of(&message).is_none());
    }
  }

  fn poll(choice: PollChoice, voters: PollVoters) -> StoredMessage {
    let mut message = stored(ALICE, 50, MessageContent::text("Poll: Lunch?", Vec::new()));
    if let MessageContent::Text { meta, .. } = &mut message.content {
      *meta = MessageMeta {
        maybe_poll: Some(Box::new(PollSpec {
          question: "Lunch?".into(),
          options: vec!["Pizza".into(), "Salad".into(), "Soup".into()],
          choice,
          voters,
          maybe_closes_at_ms: None,
          results: PollResults::Always,
        })),
        ..MessageMeta::default()
      };
    }
    message
  }

  /// A poll closing at `closes_at_ms` (Unix milliseconds) showing counts as `results` says.
  fn closing(results: PollResults, closes_at_ms: i64) -> StoredMessage {
    let mut message = poll(PollChoice::Single, PollVoters::Shown);
    if let MessageContent::Text { meta, .. } = &mut message.content
      && let Some(spec) = meta.maybe_poll.as_mut()
    {
      spec.maybe_closes_at_ms = Some(closes_at_ms);
      spec.results = results;
    }
    message
  }

  fn with_results(results: PollResults) -> StoredMessage {
    let mut message = poll(PollChoice::Single, PollVoters::Shown);
    if let MessageContent::Text { meta, .. } = &mut message.content
      && let Some(spec) = meta.maybe_poll.as_mut()
    {
      spec.results = results;
    }
    message
  }

  /// A vote the server took at `taken_at_ms` (Unix milliseconds).
  fn vote_at(sender: u64, taken_at_ms: i64, choices: &[u32]) -> StoredMessage {
    let since_epoch = u64::try_from(taken_at_ms - SNOWFLAKE_EPOCH_MS).unwrap();
    vote(sender, (since_epoch << 22) + sender, choices)
  }

  fn no_moderators() -> BTreeSet<UserId> {
    BTreeSet::new()
  }

  fn spec(message: &StoredMessage) -> &PollSpec {
    poll_of(message).unwrap()
  }

  fn vote(sender: u64, server_id: u64, choices: &[u32]) -> StoredMessage {
    stored(
      sender,
      server_id,
      MessageContent::PollVote {
        target: poll_id(),
        choices: choices.to_vec(),
      },
    )
  }

  fn close(sender: u64, server_id: u64) -> StoredMessage {
    stored(sender, server_id, MessageContent::PollClose { target: poll_id() })
  }

  fn counts(view: &PollView) -> Vec<u32> {
    view.options.iter().map(|option| option.count).collect()
  }

  fn poll_id() -> ClientMessageId {
    ClientMessageId::from_bytes([50; 16])
  }

  fn stored(sender: u64, server_id: u64, content: MessageContent) -> StoredMessage {
    let client_message_id = if server_id == 50 {
      poll_id()
    } else {
      let mut bytes = [0; 16];
      bytes[..8].copy_from_slice(&server_id.to_be_bytes());
      bytes[8] = u8::try_from(sender).unwrap();
      ClientMessageId::from_bytes(bytes)
    };
    StoredMessage {
      conversation_id: id(CONVERSATION),
      client_message_id,
      maybe_server_id: Some(id(server_id)),
      sender_user_id: id(sender),
      maybe_thread_id: None,
      sent_at_ms: i64::try_from(server_id).unwrap(),
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  fn id<T: teamcraft_proto::Id>(raw: u64) -> T {
    teamcraft_client_api::model::id_from_raw(raw)
  }
}
