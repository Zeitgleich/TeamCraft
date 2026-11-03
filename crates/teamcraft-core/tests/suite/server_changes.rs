//! Lines about changes the server reports of a channel (issue #818): archived, restored, made
//! private or public, moved. Every member's device derives them from the server's record as it
//! sees it change; a member's encrypted announcement only names who made the change, when it
//! matches.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

#[cfg(feature = "ui")]
use teamcraft_client_api::actions::WorkspaceEvent;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::records::{self, Draft, OutboxEntry};
use teamcraft_core::store::{
  ConversationEvent, LocalStore, MessageContent, MessageFilter, StoredMessage, Table, nobody,
};
use teamcraft_proto::{
  ClientMessageId, ConversationId, ConversationVisibility, GatewayEvent, UpdateConversationRequest, UserId,
};

use crate::common::{Peer, settle};

/// A line as members read it: what the server reports, and who the line names.
type Line = (ConversationEvent, Option<UserId>);

mod naming {
  use super::*;

  /// Whoever made the change still announces it; the line names them on every member's device,
  /// on their own as the user's change.
  #[tokio::test]
  async fn the_line_names_who_announced_the_change() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    let carol_id = carol.client.user_id();

    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    for peer in [&carol, &dave, &erin] {
      assert_eq!(
        lines(peer, channel),
        vec![(ConversationEvent::Archived, Some(carol_id))],
        "{}",
        peer.name
      );
    }
    // Still sent as before, for older versions; hidden here.
    assert_eq!(announcements(&dave, channel), 1);
    #[cfg(feature = "ui")]
    assert_eq!(
      shown(&dave, channel),
      vec![WorkspaceEvent::Archived {
        maybe_by: Some(carol_id)
      }]
    );
  }

  /// A change nobody announced (a server that drops the message, a sender that crashed before
  /// sending it, a change on the server alone) still shows, as the server's statement.
  #[tokio::test]
  async fn a_change_without_its_announcement_shows_without_a_name() {
    let (server, mut carol, mut dave, mut erin, channel) = setup().await;
    server.drop_messages_of(carol.client.user_id(), true);

    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(lines(&dave, channel), vec![(ConversationEvent::Archived, None)]);
    assert_eq!(announcements(&dave, channel), 0, "the message never arrived");
    let line = line_message(&dave, channel);
    assert_eq!(line.sender_user_id, nobody(), "nobody is the line's author");
    #[cfg(feature = "ui")]
    assert_eq!(
      shown(&dave, channel),
      vec![WorkspaceEvent::Archived { maybe_by: None }]
    );

    // The server changes the channel on its own: no client was involved.
    convert(&carol, channel, ConversationVisibility::Public).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    assert_eq!(
      lines(&erin, channel),
      vec![
        (ConversationEvent::Archived, None),
        (ConversationEvent::MadePublic, None)
      ]
    );
    // Carol's device didn't make that change either: it names nobody there too.
    assert_eq!(lines(&carol, channel)[1], (ConversationEvent::MadePublic, None));
  }

  /// Somebody else's change the user's device hadn't seen yet when the user changed something else:
  /// the server's answer shows both, but only the user's own change is theirs, and only that one
  /// is announced.
  #[tokio::test]
  async fn only_what_the_user_asked_for_is_theirs() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    let carol_id = carol.client.user_id();
    // On the server alone, while Carol's device hears nothing of it.
    dave
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut dave, &mut erin]).await;

    convert_on_client(&mut carol, channel, ConversationVisibility::Public).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    let expected = vec![
      (ConversationEvent::Archived, None),
      (ConversationEvent::MadePublic, Some(carol_id)),
    ];
    assert_eq!(lines(&carol, channel), expected);
    assert_eq!(lines(&erin, channel), expected);
    assert_eq!(announcements(&erin, channel), 1, "only the change Carol made");
  }

  /// An announcement of another change than the one the server reports names nobody.
  #[tokio::test]
  async fn an_announcement_of_another_change_names_nobody() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    // The server archives the channel; Dave claims he made it public.
    carol
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    announce(&mut dave, channel, ConversationEvent::MadePublic).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
  }

  /// A move names the mover only for the space the server says; the line names the space as this
  /// device knows it, not as the announcement does.
  #[tokio::test]
  async fn a_move_to_another_space_names_the_mover_only_for_that_space() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    let target = carol
      .client
      .create_space(&teamcraft_proto::CreateSpaceRequest {
        name: "Project B".into(),
        maybe_icon: None,
        maybe_color: None,
        maybe_description: None,
        join_policy: teamcraft_proto::JoinPolicy::Open,
        maybe_idempotency_key: None,
      })
      .await
      .unwrap()
      .id;
    dave.client.join_space(target).await.unwrap();
    erin.client.join_space(target).await.unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    // On the server alone; Dave claims he moved it to a space of his own invention.
    carol
      .api
      .move_conversation(
        channel,
        &teamcraft_proto::MoveChannelRequest {
          space_id: target,
          maybe_category_id: None,
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    announce(
      &mut dave,
      channel,
      ConversationEvent::Moved {
        space: "Project C".into(),
      },
    )
    .await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    let moved = ConversationEvent::Moved {
      space: "Project B".into(),
    };
    assert_eq!(lines(&erin, channel), vec![(moved.clone(), None)]);
    // The same space, as Carol's client announces it: her name.
    announce(
      &mut carol,
      channel,
      ConversationEvent::Moved {
        space: "Project B".into(),
      },
    )
    .await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    assert_eq!(lines(&erin, channel), vec![(moved, Some(carol.client.user_id()))]);
  }
}

mod forgery {
  use super::*;

  /// A member who announces a change the server doesn't report tells nobody anything.
  #[tokio::test]
  async fn a_made_up_announcement_shows_nothing() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;

    for event in [
      ConversationEvent::Archived,
      ConversationEvent::MadePublic,
      ConversationEvent::Moved {
        space: "Project B".into(),
      },
    ] {
      announce(&mut dave, channel, event).await;
    }
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(announcements(&erin, channel), 3, "the messages arrived");
    assert!(lines(&erin, channel).is_empty(), "and show nothing");
    #[cfg(feature = "ui")]
    assert!(shown(&erin, channel).is_empty());

    // A real change later isn't taken by the earlier made-up claim.
    carol
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
  }

  /// A claim from before the device saw the change, which it only loads afterwards (jumping to a
  /// date in a channel too big to hold), can't name anybody on the change.
  #[tokio::test]
  async fn an_older_claim_loaded_later_names_nobody() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    let claim = send(
      &mut dave,
      channel,
      MessageContent::Event {
        event: ConversationEvent::Archived,
      },
    )
    .await;
    carol.client.send_text(channel, None, "later").await.unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    // Erin's device hasn't loaded that part of the history (yet).
    let sent_ms = erin.store.message(channel, claim).unwrap().unwrap().sent_at_ms;
    erin.store.delete_message(channel, claim).unwrap();

    carol
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    erin.client.load_messages_from(channel, sent_ms).await.unwrap();

    assert_eq!(announcements(&erin, channel), 1, "the claim is back");
    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
  }

  /// Two members claim the same change: the line names neither.
  #[tokio::test]
  async fn a_second_claim_leaves_the_line_without_a_name() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;

    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    announce(&mut dave, channel, ConversationEvent::Archived).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
    // The line sits at Carol's announcement, which shares its row's ID: the line's text wins.
    #[cfg(feature = "ui")]
    assert_eq!(
      shown(&erin, channel),
      vec![WorkspaceEvent::Archived { maybe_by: None }]
    );
    // The user's own change stays theirs on their device.
    assert_eq!(
      lines(&carol, channel),
      vec![(ConversationEvent::Archived, Some(carol.client.user_id()))]
    );
  }

  /// A line caught up after a while away moves back to the claim that named somebody, and shares
  /// that hidden message's row ID; contested later, its row still says what the line says, also
  /// when the claim's ID sorts first.
  #[cfg(feature = "ui")]
  #[tokio::test]
  async fn a_line_moved_back_to_a_claim_keeps_its_own_text() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    // While Erin's device is away: the server archives the channel, Dave and Carol both claim it,
    // Dave's message under the smallest ID there is.
    carol
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    enqueue_as(
      &dave,
      channel,
      ClientMessageId::from_bytes([0; 16]),
      MessageContent::Event {
        event: ConversationEvent::Archived,
      },
    );
    dave.client.flush_outbox().await.unwrap();
    announce(&mut carol, channel, ConversationEvent::Archived).await;
    settle(&mut [&mut carol, &mut dave]).await;
    erin.client.full_sync().await.unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    let line = line_message(&erin, channel);
    let claim = erin
      .store
      .message(channel, ClientMessageId::from_bytes([0; 16]))
      .unwrap()
      .unwrap();
    assert_eq!(line.line_position(), claim.maybe_server_id, "at Dave's claim");
    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
    assert_eq!(
      shown(&erin, channel),
      vec![WorkspaceEvent::Archived { maybe_by: None }]
    );
  }

  /// An announcement long after the change can't claim it.
  #[tokio::test]
  async fn a_late_claim_names_nobody() {
    let (server, mut carol, mut dave, mut erin, channel) = setup().await;
    server.drop_messages_of(carol.client.user_id(), true);
    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    server.advance_clock(std::time::Duration::from_secs(11 * 60));
    announce(&mut dave, channel, ConversationEvent::Archived).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(lines(&erin, channel), vec![(ConversationEvent::Archived, None)]);
  }

  /// The line's ID can be guessed; still, no message replaces, edits, deletes or pins it.
  #[tokio::test]
  async fn nobody_replaces_or_deletes_the_line() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    let line = line_message(&erin, channel);

    send(
      &mut carol,
      channel,
      MessageContent::Delete {
        target: line.client_message_id,
      },
    )
    .await;
    send(
      &mut dave,
      channel,
      MessageContent::Pin {
        target: line.client_message_id,
        is_pinned: true,
      },
    )
    .await;
    enqueue_as(
      &dave,
      channel,
      line.client_message_id,
      MessageContent::Text {
        body: "nothing to see".into(),
        mentions: Vec::new(),
        meta: Default::default(),
      },
    );
    dave.client.flush_outbox().await.unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(line_message(&erin, channel), line);
    assert!(erin.texts(channel).is_empty());
  }
}

mod delivery {
  use super::*;

  /// The gateway's update is refetched again, the device syncs again and reconnects: the change
  /// is told once.
  #[tokio::test]
  async fn refetches_syncs_and_reconnects_leave_one_line() {
    let (server, mut carol, mut dave, mut erin, channel) = setup().await;
    archive(&mut carol, channel, true).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    server.publish_event(
      &[erin.client.user_id()],
      &GatewayEvent::ConversationUpdate {
        conversation_id: channel,
      },
    );
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    erin.client.full_sync().await.unwrap();
    let (gateway, _) = erin.client.connect_gateway().await.unwrap();
    erin.gateway = gateway;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    assert_eq!(
      lines(&erin, channel),
      vec![(ConversationEvent::Archived, Some(carol.client.user_id()))]
    );
  }

  /// A device that was away comes back with a full sync: the changes it missed show once each,
  /// named from the announcements it catches up (sent long after the newest message it held, but
  /// before the server's newest one), after what it held before.
  #[tokio::test]
  async fn a_device_back_from_away_tells_what_changed_meanwhile() {
    let (server, mut carol, mut dave, mut erin, channel) = setup().await;
    carol.client.send_text(channel, None, "before").await.unwrap();
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    // Erin's device is away for a while; then Carol archives the channel and makes it public.
    server.advance_clock(std::time::Duration::from_secs(60 * 60));
    archive(&mut carol, channel, true).await;
    convert_on_client(&mut carol, channel, ConversationVisibility::Public).await;
    settle(&mut [&mut carol, &mut dave]).await;
    erin.client.full_sync().await.unwrap();
    // The events the gateway kept for her meanwhile change nothing.
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;

    let carol_id = carol.client.user_id();
    assert_eq!(
      lines(&erin, channel),
      vec![
        (ConversationEvent::Archived, Some(carol_id)),
        (ConversationEvent::MadePublic, Some(carol_id))
      ]
    );
    let order: Vec<&str> = erin
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .iter()
      .filter_map(|message| match &message.content {
        _ if message.maybe_server_change.is_some() => Some("line"),
        MessageContent::Text { body, .. } if body == "before" => Some("before"),
        _ => None,
      })
      .collect();
    assert_eq!(
      order,
      ["before", "line", "line"],
      "the lines come after the message before"
    );
  }

  /// A 1.0.8 client changes the channel and announces it as it always did: the line names it. And
  /// this version still sends the announcement 1.0.8 shows.
  #[tokio::test]
  async fn mixed_versions_name_who_made_the_change() {
    let (_server, mut carol, mut dave, mut erin, channel) = setup().await;
    let (carol_id, dave_id) = (carol.client.user_id(), dave.client.user_id());

    // Dave's 1.0.8 client: the server's change, then its announcement.
    dave
      .api
      .update_conversation(
        channel,
        &UpdateConversationRequest {
          maybe_is_archived: Some(true),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
    announce(&mut dave, channel, ConversationEvent::Archived).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    assert_eq!(
      lines(&erin, channel),
      vec![(ConversationEvent::Archived, Some(dave_id))]
    );

    // Carol's change on this version reaches a 1.0.8 device as the message it shows.
    archive(&mut carol, channel, false).await;
    settle(&mut [&mut carol, &mut dave, &mut erin]).await;
    let unarchived: Vec<UserId> = erin
      .store
      .messages(channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .filter(|message| {
        message.maybe_server_id.is_some()
          && message.content
            == MessageContent::Event {
              event: ConversationEvent::Unarchived,
            }
      })
      .map(|message| message.sender_user_id)
      .collect();
    assert_eq!(unarchived, vec![carol_id]);
  }
}

/// `peer` archives (or restores) `channel` on this version's client.
async fn archive(peer: &mut Peer, channel: ConversationId, is_archived: bool) {
  peer
    .client
    .update_channel(
      channel,
      &UpdateConversationRequest {
        maybe_is_archived: Some(is_archived),
        ..UpdateConversationRequest::default()
      },
    )
    .await
    .unwrap();
}

/// `peer` makes `channel` public or private on this version's client.
async fn convert_on_client(peer: &mut Peer, channel: ConversationId, visibility: ConversationVisibility) {
  peer
    .client
    .update_channel(
      channel,
      &UpdateConversationRequest {
        maybe_visibility: Some(visibility),
        ..UpdateConversationRequest::default()
      },
    )
    .await
    .unwrap();
}

/// The server converts `channel` on its own (with `peer`'s rights), no client involved.
async fn convert(peer: &Peer, channel: ConversationId, visibility: ConversationVisibility) {
  peer
    .api
    .update_conversation(
      channel,
      &UpdateConversationRequest {
        maybe_visibility: Some(visibility),
        ..UpdateConversationRequest::default()
      },
    )
    .await
    .unwrap();
}

/// The lines `peer` lists in `channel`, oldest first. Each one stems from the server's record.
fn lines(peer: &Peer, channel: ConversationId) -> Vec<Line> {
  peer
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match &message.content {
      MessageContent::Event { event } if event.is_server_change() => {
        assert!(
          message.maybe_server_change.is_some(),
          "{}: a listed message",
          peer.name
        );
        Some((event.clone(), message.maybe_actor()))
      },
      _ => None,
    })
    .collect()
}

/// The only line `peer` lists in `channel`, as stored.
fn line_message(peer: &Peer, channel: ConversationId) -> StoredMessage {
  let mut found: Vec<StoredMessage> = peer
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|message| message.maybe_server_change.is_some())
    .collect();
  assert_eq!(found.len(), 1, "{}", peer.name);
  found.remove(0)
}

/// How many announcements of these changes (messages, not lines) `peer` has in `channel`.
fn announcements(peer: &Peer, channel: ConversationId) -> usize {
  peer
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|message| {
      message.maybe_server_change.is_none()
        && matches!(&message.content, MessageContent::Event { event } if event.is_server_change())
    })
    .count()
}

/// The change events the UI adapter hands `peer`'s app for `channel`. Their rows are no messages
/// waiting to be sent.
#[cfg(feature = "ui")]
fn shown(peer: &Peer, channel: ConversationId) -> Vec<WorkspaceEvent> {
  use teamcraft_client_api::backend::ChatBackend;
  use teamcraft_client_api::model::{MessageKind, is_pending};

  let backend = crate::common::backend_of(peer);
  let count = backend.message_count(channel);
  let rows = backend.messages(channel, 0, count);
  let ids: Vec<_> = rows.iter().map(|message| message.id).collect();
  backend
    .message_extras(channel, &ids)
    .into_iter()
    .filter(|extras| {
      matches!(
        extras.maybe_workspace_event,
        Some(
          WorkspaceEvent::Archived { .. }
            | WorkspaceEvent::Unarchived { .. }
            | WorkspaceEvent::MadePrivate { .. }
            | WorkspaceEvent::MadePublic { .. }
            | WorkspaceEvent::Moved { .. }
        )
      )
    })
    .map(|extras| {
      assert!(!is_pending(extras.message), "{}: shown as sending", peer.name);
      let row = rows.iter().find(|row| row.id == extras.message).unwrap();
      assert_eq!(
        row.kind,
        MessageKind::Change,
        "{}: a line, not a bubble",
        peer.name
      );
      extras.maybe_workspace_event.unwrap()
    })
    .collect()
}

/// `peer` announces `event` in `channel`, as a client does after changing the channel.
async fn announce(peer: &mut Peer, channel: ConversationId, event: ConversationEvent) {
  send(peer, channel, MessageContent::Event { event }).await;
}

/// `peer` sends `content` to `channel` as written.
async fn send(peer: &mut Peer, channel: ConversationId, content: MessageContent) -> ClientMessageId {
  let id = records::enqueue_message(
    &*peer.store,
    peer.client.user_id(),
    1_800_000_000_000,
    Draft {
      conversation_id: channel,
      maybe_thread_id: None,
      content,
    },
  )
  .unwrap();
  peer.client.flush_outbox().await.unwrap();
  id
}

/// Puts `content` into `peer`'s outbox under the ID `id`, as a client that picks its IDs would.
fn enqueue_as(peer: &Peer, channel: ConversationId, id: ClientMessageId, content: MessageContent) {
  peer
    .store
    .upsert_message(&StoredMessage {
      conversation_id: channel,
      client_message_id: id,
      maybe_server_id: None,
      sender_user_id: peer.client.user_id(),
      maybe_thread_id: None,
      sent_at_ms: 1_800_000_000_000,
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    })
    .unwrap();
  records::put_json(
    &*peer.store,
    Table::Outbox,
    &records::outbox_key(channel, id),
    &OutboxEntry {
      conversation_id: channel,
      client_message_id: id,
      maybe_thread_id: None,
      attempts: 0,
      maybe_last_error: None,
    },
  )
  .unwrap();
}

/// Carol's private channel with Dave and Erin, everybody's devices in its group.
async fn setup() -> (FakeServer, Peer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let mut erin = Peer::enroll(&server, "Erin").await;
  let channel = carol
    .client
    .create_channel(carol.space().await, "ops")
    .await
    .unwrap();
  for peer in [&dave, &erin] {
    carol
      .client
      .add_member(channel, peer.client.user_id())
      .await
      .unwrap();
  }
  settle(&mut [&mut carol, &mut dave, &mut erin]).await;
  (server, carol, dave, erin, channel)
}
