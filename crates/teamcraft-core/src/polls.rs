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
//! - The first close by the poll's sender ends it: votes after it don't count. A close by anybody
//!   else changes nothing.
//! - A vote still in this device's outbox counts as the newest, until the server orders it.

use std::collections::HashMap;

use teamcraft_client_api::polls::{PollOptionView, PollView};
use teamcraft_proto::{ClientMessageId, PollSpec, UserId};

use crate::store::{LocalStore, MessageContent, MessageOrder, StoreError, StoredMessage};

/// The poll `message` carries, if it is one this device counts: a readable text message with a
/// poll within the bounds every receiver applies. Anything else shows as its text.
pub fn poll_of(message: &StoredMessage) -> Option<&PollSpec> {
  if message.maybe_unreadable.is_some() {
    return None;
  }
  match &message.content {
    MessageContent::Text { meta, .. } => meta.maybe_poll.as_deref().filter(|spec| spec.is_receivable()),
    _ => None,
  }
}

/// The poll `poll` (whose spec is `spec`, see [`poll_of`]) as `me` sees it, counted from
/// `related`: any messages of the conversation; those that aren't a vote or close of this poll are
/// passed over.
pub fn tally<'a>(
  poll: &StoredMessage,
  spec: &PollSpec,
  related: impl IntoIterator<Item = &'a StoredMessage>,
  me: UserId,
) -> PollView {
  let mut events: Vec<&StoredMessage> = related
    .into_iter()
    .filter(|message| message.maybe_unreadable.is_none() && is_event_of(message, poll.client_message_id))
    .collect();
  events.sort_by_key(|message| message.order());
  let maybe_closed_at = closed_at(poll, &events);
  let votes = counted_votes(spec, &events, maybe_closed_at);
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
      option.count += 1;
      option.is_chosen |= *voter == me;
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
    voter_count: u32::try_from(votes.len()).unwrap_or(u32::MAX),
    is_closed: maybe_closed_at.is_some(),
    is_mine: poll.sender_user_id == me,
  }
}

/// The poll `poll` of `conversation` as `me` sees it, from what `store` holds (its index of
/// reactions holds the votes and closes too). `None` when the message isn't held or isn't a poll.
pub fn stored_poll(
  store: &impl LocalStore,
  conversation: teamcraft_proto::ConversationId,
  poll: ClientMessageId,
  me: UserId,
) -> Result<Option<PollView>, StoreError> {
  let Some(message) = store.message(conversation, poll)? else {
    return Ok(None);
  };
  let Some(spec) = poll_of(&message) else {
    return Ok(None);
  };
  let related = store.reactions_to(conversation, &[poll])?;
  Ok(Some(tally(&message, spec, &related, me)))
}

/// Whether `message` votes in or closes the poll `poll`.
fn is_event_of(message: &StoredMessage, poll: ClientMessageId) -> bool {
  matches!(
    &message.content,
    MessageContent::PollVote { target, .. } | MessageContent::PollClose { target } if *target == poll
  )
}

/// Where the first close by the poll's sender is among `events` (sorted), if any.
fn closed_at(poll: &StoredMessage, events: &[&StoredMessage]) -> Option<MessageOrder> {
  events
    .iter()
    .find(|message| {
      matches!(message.content, MessageContent::PollClose { .. })
        && message.sender_user_id == poll.sender_user_id
    })
    .map(|message| message.order())
}

/// Each voter's counted choices, in the order of their first counted vote (so names list the
/// earliest voters first).
fn counted_votes(
  spec: &PollSpec,
  events: &[&StoredMessage],
  maybe_closed_at: Option<MessageOrder>,
) -> Vec<(UserId, Vec<u32>)> {
  let mut votes: Vec<(UserId, Vec<u32>)> = Vec::new();
  let mut at_of: HashMap<UserId, usize> = HashMap::new();
  for message in events {
    let MessageContent::PollVote { choices, .. } = &message.content else {
      continue;
    };
    if maybe_closed_at.is_some_and(|closed| message.order() > closed)
      || !spec.choice.allows(choices, spec.options.len())
    {
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
  use teamcraft_proto::{PollChoice, PollVoters};

  use super::*;
  use crate::store::{MessageMeta, Unreadable};

  const CONVERSATION: u64 = 10;
  const ALICE: u64 = 1;
  const BOB: u64 = 2;
  const CAROL: u64 = 3;

  mod counting {
    use super::*;

    #[test]
    fn each_persons_newest_vote_counts() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(CAROL, 101, &[1]), vote(BOB, 102, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(BOB));
      assert_eq!(counts(&view), vec![0, 1, 1]);
      assert_eq!(view.voter_count, 2);
      assert!(view.options[2].is_chosen && !view.options[0].is_chosen);
      assert_eq!(view.options[2].voters, vec![id(BOB)]);
    }

    #[test]
    fn no_choices_withdraw_a_vote() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(BOB, 101, &[])];
      let view = tally(&poll, spec(&poll), &events, id(BOB));
      assert_eq!(counts(&view), vec![0, 0, 0]);
      assert_eq!(view.voter_count, 0);
    }

    #[test]
    fn a_multiple_choice_counts_each_option_once_per_person() {
      let poll = poll(PollChoice::Multiple, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0, 2]), vote(CAROL, 101, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(ALICE));
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
      let view = tally(&poll, spec(&poll), &events, id(BOB));
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
      let expected = tally(&poll, spec(&poll), &events, id(ALICE));
      assert_eq!(counts(&expected), vec![0, 2, 0]);
      events.reverse();
      assert_eq!(tally(&poll, spec(&poll), &events, id(ALICE)), expected);
      events.swap(0, 3);
      assert_eq!(tally(&poll, spec(&poll), &events, id(ALICE)), expected);
    }

    #[test]
    fn a_replayed_vote_does_not_count_twice() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [vote(BOB, 100, &[0]), vote(BOB, 100, &[0]), vote(BOB, 101, &[0])];
      let view = tally(&poll, spec(&poll), &events, id(ALICE));
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
      let view = tally(&poll, spec(&poll), &[other, unreadable], id(ALICE));
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
      let view = tally(&poll, spec(&poll), &events, id(ALICE));
      assert!(view.is_closed);
      assert_eq!(counts(&view), vec![1, 0, 0]);
    }

    #[test]
    fn only_the_creator_closes() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let events = [close(BOB, 100), vote(CAROL, 101, &[2])];
      let view = tally(&poll, spec(&poll), &events, id(BOB));
      assert!(!view.is_closed);
      assert!(!view.is_mine);
      assert_eq!(counts(&view), vec![0, 0, 1]);
    }

    #[test]
    fn a_vote_still_in_the_outbox_counts_until_a_close_comes_first() {
      let poll = poll(PollChoice::Single, PollVoters::Shown);
      let mut pending = vote(BOB, 0, &[1]);
      pending.maybe_server_id = None;
      let view = tally(&poll, spec(&poll), [&vote(BOB, 100, &[0]), &pending], id(BOB));
      assert_eq!(counts(&view), vec![0, 1, 0]);
      let view = tally(&poll, spec(&poll), [&close(ALICE, 101), &pending], id(BOB));
      assert_eq!(view.voter_count, 0);
    }
  }

  mod hidden_voters {
    use super::*;

    #[test]
    fn a_hidden_poll_names_nobody_but_marks_the_users_own_choice() {
      let poll = poll(PollChoice::Single, PollVoters::Hidden);
      let events = [vote(BOB, 100, &[0]), vote(CAROL, 101, &[0])];
      let view = tally(&poll, spec(&poll), &events, id(BOB));
      assert_eq!(counts(&view), vec![2, 0, 0]);
      assert!(view.options[0].is_chosen);
      assert!(view.options.iter().all(|option| option.voters.is_empty()));
    }

    #[test]
    fn a_mode_of_a_newer_version_names_nobody() {
      let poll = poll(PollChoice::Single, PollVoters::Unknown("blinded".into()));
      let view = tally(&poll, spec(&poll), &[vote(BOB, 100, &[0])], id(ALICE));
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
        })),
        ..MessageMeta::default()
      };
    }
    message
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
