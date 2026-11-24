//! Polls (issue #205, e2ee.md §4.12) end to end: made, voted in, changed and closed through the UI
//! adapter the desktop shell uses, sent through the fake server as ordinary encrypted messages,
//! and counted by every device for itself.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Message;
use teamcraft_client_api::polls::{
  NewPoll, PollChoice, PollResults, PollSpec, PollView, PollVoters, fallback_text,
};
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
    self.alice_polls_showing(voters, PollResults::Always).await
  }

  /// [`Self::alice_polls`] with counts shown as `results` says.
  async fn alice_polls_showing(&mut self, voters: PollVoters, results: PollResults) -> Message {
    let alice_ui = backend_of(&self.alice);
    let spec = PollSpec {
      question: QUESTION.to_owned(),
      options: OPTIONS.map(str::to_owned).to_vec(),
      choice: PollChoice::Single,
      voters,
      maybe_closes_at_ms: None,
      results,
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

mod results {
  use super::*;

  #[tokio::test]
  async fn counts_show_to_each_person_once_they_voted() {
    let mut t = team().await;
    let poll = t
      .alice_polls_showing(PollVoters::Shown, PollResults::AfterVote)
      .await;
    t.vote(Who::Bob, &poll, &[1]).await;

    let [alice, bob, carol] = t.views();
    assert!(bob.are_results_shown);
    assert_eq!(counts(&bob), [0, 1, 0]);
    for view in [&alice, &carol] {
      assert!(!view.are_results_shown);
      assert_eq!(counts(view), [0, 0, 0]);
      assert!(view.options.iter().all(|option| option.voters.is_empty()));
      assert_eq!(view.voter_count, 1, "how many voted still shows");
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

/// Anonymous polls (issue #2121, e2ee.md §4.12.1): ballots go sealed to the server's ballot box,
/// never as messages, and count only once the box is closed.
mod anonymous {
  use teamcraft_proto::{Base64Bytes, CastBallotRequest, MessageId};

  use super::*;

  impl Team {
    /// Every UI shows the poll and does what showing it asked for (reading the box).
    async fn look(&mut self) {
      let channel = self.channel;
      for peer in [&mut self.alice, &mut self.bob, &mut self.carol] {
        let ui = backend_of(peer);
        let _ = poll_message(peer, channel);
        run_commands(peer, &ui).await;
      }
      self.settle().await;
    }

    async fn alice_closes(&mut self, poll: &Message) {
      let alice_ui = backend_of(&self.alice);
      alice_ui.close_poll(self.channel, poll.id);
      run_commands(&mut self.alice, &alice_ui).await;
      self.settle().await;
      self.look().await;
    }

    /// The poll's server ID, which the ballot box goes by.
    fn poll_server_id(&self) -> MessageId {
      let id = poll_id(&self.alice, self.channel);
      self
        .alice
        .store
        .message(self.channel, id)
        .unwrap()
        .unwrap()
        .maybe_server_id
        .unwrap()
    }
  }

  #[tokio::test]
  async fn ballots_count_once_closed_and_name_nobody() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    t.vote(Who::Bob, &poll, &[0]).await;
    t.vote(Who::Carol, &poll, &[1]).await;
    t.vote(Who::Bob, &poll, &[1]).await;
    t.look().await;

    let [alice, bob, carol] = t.views();
    for view in [&alice, &bob, &carol] {
      assert_eq!(view.voters, PollVoters::Anonymous);
      assert_eq!(view.results, PollResults::AfterClose);
      assert_eq!(view.voter_count, 2, "the count of ballots shows while open");
      assert!(!view.are_results_shown);
      assert_eq!(counts(view), [0, 0, 0]);
      assert!(view.can_vote());
    }
    assert!(bob.options[1].is_chosen && !bob.options[0].is_chosen);
    assert!(carol.options[1].is_chosen);
    assert!(alice.options.iter().all(|option| !option.is_chosen));
    for peer in [&t.alice, &t.bob, &t.carol] {
      assert!(
        !stored_kinds(peer, t.channel).contains(&"poll_vote"),
        "no vote travels as a message naming its sender"
      );
    }

    t.alice_closes(&poll).await;
    let [alice, bob, carol] = t.views();
    for view in [&alice, &bob, &carol] {
      assert!(view.is_closed && !view.can_vote());
      assert!(view.are_results_shown);
      assert_eq!(counts(view), [0, 2, 0]);
      assert_eq!(view.voter_count, 2);
      assert!(view.options.iter().all(|option| option.voters.is_empty()));
    }
    assert!(
      bob.options[1].is_chosen,
      "the own choice stays after the server forgot it"
    );
  }

  #[tokio::test]
  async fn the_server_holds_sealed_ballots_without_choices_and_hands_them_out_only_closed() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    t.vote(Who::Bob, &poll, &[2]).await;
    let server_id = t.poll_server_id();

    let as_carol = t
      .carol
      .client
      .api()
      .ballot_box(t.channel, server_id)
      .await
      .unwrap();
    assert_eq!(as_carol.ballot_count, 1);
    assert!(
      as_carol.maybe_mine.is_none() && as_carol.ballots.is_empty(),
      "no ballot while open"
    );
    let as_bob = t.bob.client.api().ballot_box(t.channel, server_id).await.unwrap();
    let mine = as_bob.maybe_mine.unwrap();
    assert_eq!(mine.as_ref().len(), teamcraft_crypto::poll_ballot::BALLOT_LEN);

    let stored = t
      .alice
      .client
      .api()
      .list_messages(t.channel, &ListMessagesQuery::default())
      .await
      .unwrap()
      .messages;
    let as_json = serde_json::to_string(&stored).unwrap();
    for plain in [QUESTION, OPTIONS[0], "anonymous", "ballot_key", "choices"] {
      assert!(!as_json.contains(plain), "the server saw {plain:?}");
    }

    t.alice_closes(&poll).await;
    let closed = t
      .carol
      .client
      .api()
      .ballot_box(t.channel, server_id)
      .await
      .unwrap();
    assert!(closed.is_closed);
    assert_eq!(closed.ballots, vec![mine]);
    let bob_after = t.bob.client.api().ballot_box(t.channel, server_id).await.unwrap();
    assert!(
      bob_after.maybe_mine.is_none(),
      "closing forgets whose ballot is whose"
    );
  }

  #[tokio::test]
  async fn forged_and_replayed_ballots_do_not_count() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    t.vote(Who::Bob, &poll, &[0]).await;
    let server_id = t.poll_server_id();
    let bobs = t
      .bob
      .client
      .api()
      .ballot_box(t.channel, server_id)
      .await
      .unwrap()
      .maybe_mine
      .unwrap();
    // A modified client of Carol's hands in a copy of Bob's ballot, then Alice one with flipped
    // bits: the server takes both (it can't read them), nobody counts them.
    let replay = CastBallotRequest { ballot: bobs.clone() };
    t.carol
      .client
      .api()
      .set_ballot(t.channel, server_id, Some(&replay))
      .await
      .unwrap();
    let mut tampered = bobs.as_ref().to_vec();
    tampered[30] ^= 1;
    let forged = CastBallotRequest {
      ballot: Base64Bytes::from(tampered),
    };
    t.alice
      .client
      .api()
      .set_ballot(t.channel, server_id, Some(&forged))
      .await
      .unwrap();
    t.settle().await;

    t.alice_closes(&poll).await;
    for view in t.views() {
      assert_eq!(counts(&view), [1, 0, 0]);
      assert_eq!(view.voter_count, 1, "the copy and the forgery count nowhere");
    }
  }

  #[tokio::test]
  async fn each_person_has_one_ballot_however_often_they_vote() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    for choice in [0, 1, 2, 1] {
      t.vote(Who::Bob, &poll, &[choice]).await;
    }
    t.vote(Who::Carol, &poll, &[2]).await;
    t.vote(Who::Carol, &poll, &[]).await;
    t.alice_closes(&poll).await;
    for view in t.views() {
      assert_eq!(counts(&view), [0, 1, 0]);
      assert_eq!(view.voter_count, 1, "a withdrawn ballot is gone");
    }
  }

  #[tokio::test]
  async fn a_ballot_after_closing_is_refused_and_the_user_hears_of_it() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    t.vote(Who::Bob, &poll, &[0]).await;
    t.alice_closes(&poll).await;
    // The app offers no vote any more; a modified client queues one anyway.
    let target = poll_id(&t.carol, t.channel);
    t.carol
      .client
      .vote_in_poll(t.channel, target, vec![1])
      .await
      .unwrap();
    t.settle().await;
    let failures = t.carol.client.take_background_failures();
    assert!(
      failures.iter().any(|error| error.to_string().contains("closed")),
      "{failures:?}"
    );
    t.look().await;
    for view in t.views() {
      assert_eq!(counts(&view), [1, 0, 0]);
    }
  }

  #[tokio::test]
  async fn only_the_creator_closes_the_box() {
    let mut t = team().await;
    let poll = t.alice_polls(PollVoters::Anonymous).await;
    t.look().await;
    t.vote(Who::Bob, &poll, &[0]).await;
    let target = poll_id(&t.bob, t.channel);
    t.bob.client.close_poll(t.channel, target).await.unwrap();
    t.settle().await;
    t.look().await;
    for view in t.views() {
      assert!(!view.is_closed);
      assert!(!view.are_results_shown);
    }
  }

  #[tokio::test]
  async fn the_poll_travels_where_versions_knowing_only_named_polls_show_its_text() {
    let mut t = team().await;
    let _ = t.alice_polls(PollVoters::Anonymous).await;
    let id = poll_id(&t.bob, t.channel);
    let stored = t.bob.store.message(t.channel, id).unwrap().unwrap();
    let json = serde_json::to_value(&stored.content).unwrap();
    assert!(json["meta"].get("poll").is_none(), "{json}");
    assert_eq!(json["meta"]["anonymous_poll"]["voters"], "anonymous");
    assert_eq!(json["meta"]["anonymous_poll"]["results"], "after_close");
    assert!(json["meta"]["anonymous_poll"]["ballot_key"].is_string());
    let MessageContent::Text { body, .. } = &stored.content else {
      panic!("a poll is a text message");
    };
    assert!(body.contains(QUESTION) && body.contains("Update TeamCraft"));
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
