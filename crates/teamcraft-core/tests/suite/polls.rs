//! Polls (issue #205, e2ee.md §4.12) end to end: made, voted in, changed and closed through the UI
//! adapter the desktop shell uses, sent through the fake server as ordinary encrypted messages,
//! and counted by every device for itself.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Message;
use teamcraft_client_api::polls::{NewPoll, PollChoice, PollSpec, PollView, PollVoters, fallback_text};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter};
use teamcraft_proto::{ConversationId, ListMessagesQuery};

const QUESTION: &str = "Lunch on Thursday?";
const OPTIONS: [&str; 3] = ["Pizza place", "Salad bar", "Noodle shop"];

/// Alice, Bob and Carol in a channel.
struct Team {
  alice: Peer,
  bob: Peer,
  carol: Peer,
  channel: ConversationId,
}

async fn team() -> Team {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "lunch")
    .await
    .unwrap();
  for member in [bob.client.user_id(), carol.client.user_id()] {
    alice.client.add_member(channel, member).await.unwrap();
  }
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  Team {
    alice,
    bob,
    carol,
    channel,
  }
}

impl Team {
  async fn settle(&mut self) {
    settle(&mut [&mut self.alice, &mut self.bob, &mut self.carol]).await;
  }

  /// Alice makes a poll through her UI; it reaches everybody.
  async fn alice_polls(&mut self, voters: PollVoters) -> Message {
    let alice_ui = backend_of(&self.alice);
    let spec = PollSpec {
      question: QUESTION.to_owned(),
      options: OPTIONS.map(str::to_owned).to_vec(),
      choice: PollChoice::Single,
      voters,
    };
    let fallback = fallback_text(&spec, "Poll", "Update TeamCraft to vote in this poll.");
    alice_ui.post_poll(self.channel, NewPoll { spec, fallback });
    run_commands(&mut self.alice, &alice_ui).await;
    self.settle().await;
    poll_message(&self.alice, self.channel)
  }

  /// `voter` votes for `choices` in the poll `poll` through their UI.
  async fn vote(&mut self, voter: Who, poll: &Message, choices: &[u32]) {
    let channel = self.channel;
    let peer = self.peer(voter);
    let ui = backend_of(peer);
    let shown = poll_message(peer, channel);
    assert_eq!(shown.conversation, poll.conversation);
    ui.vote_in_poll(channel, shown.id, choices.to_vec());
    run_commands(peer, &ui).await;
    self.settle().await;
  }

  fn peer(&mut self, who: Who) -> &mut Peer {
    match who {
      Who::Bob => &mut self.bob,
      Who::Carol => &mut self.carol,
    }
  }

  /// The poll as each of the three sees it.
  fn views(&self) -> [PollView; 3] {
    [&self.alice, &self.bob, &self.carol].map(|peer| *poll_message(peer, self.channel).maybe_poll.unwrap())
  }
}

#[derive(Clone, Copy)]
enum Who {
  Bob,
  Carol,
}

mod voting {
  use super::*;

  #[tokio::test]
  async fn every_device_counts_the_same_votes_and_changes() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[0]).await;
    t.vote(Who::Carol, &poll, &[1]).await;
    t.vote(Who::Bob, &poll, &[1]).await;

    let [alice, bob, carol] = t.views();
    assert_eq!(counts(&alice), [0, 2, 0]);
    assert_eq!(alice.voter_count, 2);
    assert_eq!(
      alice.options[1].voters,
      [t.bob.client.user_id(), t.carol.client.user_id()],
      "a shown poll names its voters in the order they first voted"
    );
    assert!(bob.options[1].is_chosen && !bob.options[0].is_chosen);
    assert!(!alice.options[1].is_chosen);
    assert!(carol.options[1].is_chosen);
    for view in [&bob, &carol] {
      assert_eq!(counts(view), counts(&alice));
      assert_eq!(view.voter_count, alice.voter_count);
      assert_eq!(view.options[1].voters, alice.options[1].voters);
    }
  }

  #[tokio::test]
  async fn a_withdrawn_vote_no_longer_counts() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[2]).await;
    t.vote(Who::Bob, &poll, &[]).await;
    for view in t.views() {
      assert_eq!(view.voter_count, 0);
      assert_eq!(counts(&view), [0, 0, 0]);
    }
  }

  #[tokio::test]
  async fn the_ui_sends_no_vote_the_poll_does_not_take() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[0, 1]).await;
    t.vote(Who::Bob, &poll, &[9]).await;
    assert_eq!(t.views()[0].voter_count, 0);
    assert!(
      !stored_kinds(&t.bob, t.channel).contains(&"poll_vote"),
      "nothing was sent"
    );
  }

  #[tokio::test]
  async fn a_vote_a_modified_client_sends_for_a_missing_option_is_ignored_everywhere() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[0]).await;
    let target = poll_id(&t.carol, t.channel);
    t.carol
      .client
      .vote_in_poll(t.channel, target, vec![7])
      .await
      .unwrap();
    t.settle().await;
    for view in t.views() {
      assert_eq!(counts(&view), [1, 0, 0]);
    }
  }
}

mod closing {
  use super::*;

  #[tokio::test]
  async fn votes_after_the_creator_closes_do_not_count() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[0]).await;
    let alice_ui = backend_of(&t.alice);
    alice_ui.close_poll(t.channel, poll.id);
    run_commands(&mut t.alice, &alice_ui).await;
    t.settle().await;

    // Carol's app still offers nothing; a modified one sends a vote anyway.
    let target = poll_id(&t.carol, t.channel);
    t.carol
      .client
      .vote_in_poll(t.channel, target, vec![2])
      .await
      .unwrap();
    t.settle().await;
    for view in t.views() {
      assert!(view.is_closed);
      assert!(!view.can_vote());
      assert_eq!(counts(&view), [1, 0, 0]);
    }
  }

  #[tokio::test]
  async fn only_the_creator_closes() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    let bob_ui = backend_of(&t.bob);
    let shown = poll_message(&t.bob, t.channel);
    assert!(!shown.maybe_poll.as_ref().unwrap().is_mine);
    bob_ui.close_poll(t.channel, shown.id);
    run_commands(&mut t.bob, &bob_ui).await;
    assert!(
      !stored_kinds(&t.bob, t.channel).contains(&"poll_close"),
      "the UI doesn't send it"
    );
    // A modified client sends it anyway: nobody takes it.
    let target = poll_id(&t.bob, t.channel);
    t.bob.client.close_poll(t.channel, target).await.unwrap();
    t.settle().await;
    t.vote(Who::Carol, &poll, &[1]).await;
    for view in t.views() {
      assert!(!view.is_closed);
      assert_eq!(counts(&view), [0, 1, 0]);
    }
  }
}

mod privacy {
  use super::*;

  #[tokio::test]
  async fn the_server_holds_neither_question_options_nor_choices() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    t.vote(Who::Bob, &poll, &[1]).await;
    let stored = t
      .alice
      .client
      .api()
      .list_messages(t.channel, &ListMessagesQuery::default())
      .await
      .unwrap()
      .messages;
    let as_json = serde_json::to_string(&stored).unwrap();
    for plain in [QUESTION, OPTIONS[0], OPTIONS[1], "poll_vote", "choices"] {
      assert!(!as_json.contains(plain), "the server saw {plain:?}");
    }
    assert!(
      stored
        .iter()
        .all(|message| message.mentioned_user_ids.is_empty() && message.maybe_thread_id.is_none()),
      "a poll and a vote carry no plaintext beyond any message's"
    );
  }

  #[tokio::test]
  async fn a_hidden_poll_names_nobody_on_any_device() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Hidden).await;
    t.vote(Who::Bob, &poll, &[0]).await;
    t.vote(Who::Carol, &poll, &[0]).await;
    for view in t.views() {
      assert_eq!(view.voters, PollVoters::Hidden);
      assert_eq!(counts(&view), [2, 0, 0]);
      assert!(view.options.iter().all(|option| option.voters.is_empty()));
    }
    // Hidden is a promise of the display only: Alice's device holds each vote with its sender
    // (e2ee.md §4.12), as any member's does.
    let senders: Vec<_> = t
      .alice
      .store
      .messages(t.channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .filter(|message| matches!(message.content, MessageContent::PollVote { .. }))
      .map(|message| message.sender_user_id)
      .collect();
    assert_eq!(senders, [t.bob.client.user_id(), t.carol.client.user_id()]);
  }
}

mod older_versions {
  use super::*;

  #[tokio::test]
  async fn the_poll_is_a_text_message_with_the_question_and_options() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Shown).await;
    // What a version before polls renders: the text of a text message (it ignores the unknown
    // `poll` field of its meta).
    assert!(poll.body.contains(QUESTION));
    assert!(OPTIONS.iter().all(|option| poll.body.contains(option)));
    let texts = t.bob.texts(t.channel);
    assert!(
      texts
        .iter()
        .any(|text| text.starts_with("**Poll:** Lunch on Thursday?"))
    );
    // Votes are hidden messages, never rows of the feed.
    t.vote(Who::Bob, &poll, &[0]).await;
    let rows = backend_of(&t.carol).messages(t.channel, 0, 50);
    assert_eq!(
      rows
        .iter()
        .filter(|row| row.body.is_empty()
          && row.maybe_poll.is_none()
          && row.maybe_placeholder.is_none()
          && matches!(row.kind, teamcraft_client_api::model::MessageKind::User))
        .count(),
      0,
      "no empty rows"
    );
  }
}

/// The newest poll of `conversation` as `peer`'s UI shows it.
fn poll_message(peer: &Peer, conversation: ConversationId) -> Message {
  backend_of(peer)
    .messages(conversation, 0, 50)
    .into_iter()
    .rev()
    .find(|message| message.maybe_poll.is_some())
    .unwrap()
}

/// The client message ID of the newest poll `peer` holds.
fn poll_id(peer: &Peer, conversation: ConversationId) -> teamcraft_proto::ClientMessageId {
  peer
    .store
    .messages(conversation, MessageFilter::All)
    .unwrap()
    .into_iter()
    .rev()
    .find(|message| teamcraft_core::polls::poll_of(message).is_some())
    .unwrap()
    .client_message_id
}

/// The kinds of `peer`'s stored messages of `conversation`, as their wire names.
fn stored_kinds(peer: &Peer, conversation: ConversationId) -> Vec<&'static str> {
  peer
    .store
    .messages(conversation, MessageFilter::All)
    .unwrap()
    .into_iter()
    .map(|message| match message.content {
      MessageContent::PollVote { .. } => "poll_vote",
      MessageContent::PollClose { .. } => "poll_close",
      _ => "other",
    })
    .collect()
}

fn counts(view: &PollView) -> Vec<u32> {
  view.options.iter().map(|option| option.count).collect()
}
