//! Work that must reach the server after a mobile app leaves the foreground (issue #158): what is
//! pending survives the process being killed, goes out once after a restart (also when the server
//! stored a send whose answer was lost), and a runner tells the app when nothing waits any longer,
//! so the app knows how long to keep its background time.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::time::Duration;

use common::{Peer, TokioClock, backend_of, clock, run_commands, settings, settle};
use futures_util::future::join;
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::Client;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::runner::{RunnerCommand, RunnerSettings, SyncRunner};
use teamcraft_core::upkeep;
use teamcraft_proto::ConversationId;

mod restart {
  use super::*;

  #[tokio::test]
  async fn a_send_whose_answer_was_lost_before_the_kill_arrives_once_after_the_restart() {
    let (server, mut alice, mut bob, channel) = channel_with_two().await;
    let alice_ui = backend_of(&alice);
    // The server stores the message, but the app is suspended and killed before the answer.
    server.lose_send_answers(1);
    alice_ui.send_message(text(channel, "sent while leaving"));
    for command in alice_ui.drain_commands() {
      // The lost answer reads as a network error; the message stays queued.
      let _ = alice.client.run_ui_command(command).await;
    }
    assert_eq!(pending(&alice).sends, 1, "unanswered, it stays in the outbox");

    restart(&mut alice).await;
    assert_eq!(pending(&alice).sends, 1, "the outbox survived the kill");
    assert!(upkeep::flush(&mut alice.client).await, "sent again");
    assert!(pending(&alice).is_empty());

    settle(&mut [&mut alice, &mut bob]).await;
    let arrived = bob_bodies(&bob, channel)
      .iter()
      .filter(|body| *body == "sent while leaving")
      .count();
    assert_eq!(arrived, 1, "the client message ID made the retry no duplicate");
  }

  #[tokio::test]
  async fn an_upload_killed_on_its_way_goes_up_again_after_the_restart() {
    let (server, mut alice, mut bob, channel) = channel_with_two().await;
    let alice_ui = backend_of(&alice);
    let before = backend_of(&bob).message_count(channel);
    alice_ui.send_message(with_file(channel, "notes.txt", b"written on the train"));
    alice_ui.drain_commands();
    assert_eq!(
      pending(&alice),
      PendingWork {
        sends: 1,
        uploads: 1,
        actions: 0
      }
    );

    // The file is on its way when the OS ends the app's background time.
    let killed = alice.client.start_upload().await.unwrap().unwrap();
    drop(killed);
    restart(&mut alice).await;
    assert_eq!(pending(&alice).uploads, 1, "the upload survived the kill");

    assert!(upkeep::flush(&mut alice.client).await);
    assert!(pending(&alice).is_empty());
    assert_eq!(server.attachment_count(), 1, "the announced upload was reused");
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(
      backend_of(&bob).message_count(channel),
      before + 1,
      "arrived once"
    );
  }

  #[tokio::test]
  async fn a_message_the_server_refused_is_not_background_work() {
    let (server, mut alice, _bob, channel) = channel_with_two().await;
    server.remove_member_as_admin(channel, alice.client.user_id());
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(text(channel, "too late"));
    run_commands(&mut alice, &alice_ui).await;
    assert!(
      pending(&alice).is_empty(),
      "it waits for the user's retry or delete, not for background time"
    );
  }
}

mod drain {
  use super::*;

  #[tokio::test]
  async fn a_runner_answers_a_drain_once_the_outbox_went_out() {
    let (server, alice, _bob, channel) = channel_with_two().await;
    let me = alice.client.user_id();
    server.set_offline(me, true);
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(text(channel, "queued in the tunnel"));
    let mut alice = alice;
    run_commands(&mut alice, &alice_ui).await;
    assert_eq!(pending(&alice).sends, 1);
    server.set_offline(me, false);

    let (runner, handle) = SyncRunner::new(alice.client, RunnerSettings::default(), Box::new(|| {}));
    let (drained, is_drained) = futures_channel::oneshot::channel();
    assert!(handle.send(RunnerCommand::Drain(drained)));
    let (client, ()) = join(runner.run(), async {
      is_drained.await.unwrap();
      handle.send(RunnerCommand::Stop);
    })
    .await;
    assert!(PendingWork::read(client.store()).unwrap().is_empty());
  }

  #[tokio::test(start_paused = true)]
  async fn a_drain_stays_unanswered_while_the_server_cant_be_reached() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll_on_clock(
      server.add_user("Alice"),
      "Alice",
      settings(),
      Box::new(TokioClock::new().0),
    )
    .await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "offline")
      .await
      .unwrap();
    let me = alice.client.user_id();
    server.set_offline(me, true);
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(text(channel, "no signal"));
    run_commands(&mut alice, &alice_ui).await;

    let (runner, handle) = SyncRunner::new(alice.client, RunnerSettings::default(), Box::new(|| {}));
    let (drained, is_drained) = futures_channel::oneshot::channel();
    assert!(handle.send(RunnerCommand::Drain(drained)));
    let (client, ()) = join(runner.run(), async {
      // The app's background time runs out first; it gives up waiting and stops the runner.
      let waited = tokio::time::timeout(Duration::from_secs(30), is_drained).await;
      assert!(waited.is_err(), "nothing went out");
      handle.send(RunnerCommand::Stop);
    })
    .await;
    assert_eq!(
      PendingWork::read(client.store()).unwrap().sends,
      1,
      "still queued for the next run"
    );
  }
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "background")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}

/// The process is killed and the app started again on the same store.
async fn restart(peer: &mut Peer) {
  peer.client = Client::open(peer.api.clone(), peer.store.clone(), clock(), settings())
    .unwrap()
    .unwrap();
  let (gateway, _) = peer.client.connect_gateway().await.unwrap();
  peer.gateway = gateway;
}

fn pending(peer: &Peer) -> PendingWork {
  PendingWork::read(&peer.store).unwrap()
}

fn text(conversation: ConversationId, body: &str) -> OutgoingMessage {
  OutgoingMessage {
    conversation,
    maybe_thread: None,
    body: body.into(),
    maybe_reply_to: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: Vec::new(),
  }
}

fn with_file(conversation: ConversationId, name: &str, bytes: &[u8]) -> OutgoingMessage {
  OutgoingMessage {
    files: vec![OutgoingFile {
      file_name: name.into(),
      media_type: "text/plain".into(),
      source: FileSource::Bytes {
        bytes: bytes.to_vec(),
      },
      maybe_thumbnail: None,
      maybe_duration_ms: None,
    }],
    ..text(conversation, "")
  }
}

fn bob_bodies(bob: &Peer, conversation: ConversationId) -> Vec<String> {
  let backend = backend_of(bob);
  let count = backend.message_count(conversation);
  backend
    .messages(conversation, 0, count)
    .into_iter()
    .map(|message| message.body)
    .collect()
}
