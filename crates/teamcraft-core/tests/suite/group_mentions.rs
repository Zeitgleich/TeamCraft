//! Group mentions (issue #206) as the sender's device expands them and receivers count them,
//! through the same adapter the desktop shell uses.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::activity::ActivityKind;
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::{
  ConversationId, DEFAULT_MAX_MENTIONS_WITHOUT_GROUP_MENTIONS, GatewayEvent, Id, ListMessagesQuery,
  Permission, Snowflake, UserGroupId, UserGroupRoster, UserId,
};

fn roster(raw: i64, handle: &str, members: &[UserId]) -> UserGroupRoster {
  UserGroupRoster {
    id: UserGroupId::from_snowflake(Snowflake::new(raw).unwrap()),
    handle: handle.to_owned(),
    name: format!("Group {handle}"),
    member_ids: members.to_vec(),
  }
}

/// Alice and Bob in a channel, Dave outside it.
struct Team {
  server: FakeServer,
  alice: Peer,
  bob: Peer,
  dave: Peer,
  channel: ConversationId,
}

async fn team() -> Team {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dave = Peer::enroll(&server, "Dave").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  Team {
    server,
    alice,
    bob,
    dave,
    channel,
  }
}

/// Everything the channel's messages name to the server (the channel also holds system messages).
async fn stored_mentions(team: &Team) -> Vec<UserId> {
  team
    .alice
    .client
    .api()
    .list_messages(team.channel, &ListMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .into_iter()
    .flat_map(|message| message.mentioned_user_ids)
    .collect()
}

mod sending {
  use super::*;

  #[tokio::test]
  async fn a_group_mention_names_the_members_here_and_shows_as_the_group() {
    let mut t = team().await;
    let (alice, bob, dave) = (
      t.alice.client.user_id(),
      t.bob.client.user_id(),
      t.dave.client.user_id(),
    );
    t.server
      .set_user_groups(vec![roster(1, "accounting", &[alice, bob, dave])]);
    t.alice.client.full_sync().await.unwrap();
    let (alice_ui, bob_ui) = (backend_of(&t.alice), backend_of(&t.bob));

    let offered = alice_ui.mentionable_groups(t.channel);
    assert_eq!(offered[0].handle, "accounting");
    assert_eq!(
      offered[0].notified,
      [bob],
      "the sender and non-members are never named"
    );
    assert_eq!(offered[0].outside_count, 1, "Dave is outside the channel");

    alice_ui.post_message(t.channel, None, "@Accounting please check");
    run_commands(&mut t.alice, &alice_ui).await;
    settle(&mut [&mut t.alice, &mut t.bob]).await;

    assert_eq!(
      stored_mentions(&t).await,
      [bob],
      "the server sees only IDs of members"
    );
    let seen = bob_ui.messages(t.channel, 0, 10);
    let received = seen.iter().find(|m| m.body.contains("please check")).unwrap();
    assert_eq!(
      received.group_mentions,
      ["accounting"],
      "the handle came in the ciphertext"
    );
    assert!(received.mentions_me);
    assert!(
      bob_ui
        .activity()
        .items
        .iter()
        .any(|item| item.kind == ActivityKind::Mention),
      "a group mention is an activity entry like any mention"
    );
  }

  #[tokio::test]
  async fn membership_that_changes_after_sending_does_not_change_who_was_mentioned() {
    let mut t = team().await;
    let (alice, bob) = (t.alice.client.user_id(), t.bob.client.user_id());
    t.server.set_user_groups(vec![roster(1, "oncall", &[alice, bob])]);
    t.alice.client.full_sync().await.unwrap();
    let (alice_ui, bob_ui) = (backend_of(&t.alice), backend_of(&t.bob));

    alice_ui.post_message(t.channel, None, "@oncall the build is red");
    run_commands(&mut t.alice, &alice_ui).await;
    settle(&mut [&mut t.alice, &mut t.bob]).await;

    // Bob leaves the group after the message; the list the sender attached still counts.
    t.server.set_user_groups(vec![roster(1, "oncall", &[alice])]);
    t.server.publish_event(
      &[alice, bob],
      &GatewayEvent::UserGroupUpdate {
        group_id: roster(1, "x", &[]).id,
      },
    );
    settle(&mut [&mut t.alice, &mut t.bob]).await;
    assert!(
      bob_ui
        .messages(t.channel, 0, 10)
        .iter()
        .find(|m| m.body.contains("build is red"))
        .is_some_and(|m| m.mentions_me && m.group_mentions == ["oncall"]),
    );
    // The new roster reached the sender: the next mention no longer names him.
    assert!(alice_ui.mentionable_groups(t.channel)[0].notified.is_empty());
  }

  #[tokio::test]
  async fn a_group_in_a_code_span_is_not_expanded() {
    let mut t = team().await;
    let (alice, bob) = (t.alice.client.user_id(), t.bob.client.user_id());
    t.server.set_user_groups(vec![roster(1, "oncall", &[alice, bob])]);
    t.alice.client.full_sync().await.unwrap();
    let ui = backend_of(&t.alice);
    ui.post_message(t.channel, None, "use `@oncall` for pages");
    run_commands(&mut t.alice, &ui).await;
    assert!(stored_mentions(&t).await.is_empty());
  }
}

mod permission {
  use super::*;

  #[tokio::test]
  async fn without_the_permission_nothing_is_offered_or_expanded() {
    let mut t = team().await;
    let (alice, bob) = (t.alice.client.user_id(), t.bob.client.user_id());
    t.server.set_user_groups(vec![roster(1, "oncall", &[alice, bob])]);
    t.server
      .set_my_permissions(t.channel, Some(vec![Permission::CreatePost]));
    t.alice.client.full_sync().await.unwrap();
    let ui = backend_of(&t.alice);
    assert!(ui.mentionable_groups(t.channel).is_empty(), "no suggestion");

    ui.post_message(t.channel, None, "@oncall please");
    run_commands(&mut t.alice, &ui).await;
    settle(&mut [&mut t.alice, &mut t.bob]).await;
    assert!(stored_mentions(&t).await.is_empty(), "no expansion");
    let seen = backend_of(&t.bob).messages(t.channel, 0, 10);
    assert!(
      seen.iter().all(|m| m.group_mentions.is_empty()),
      "@oncall stays text"
    );
  }

  #[tokio::test]
  async fn a_guest_who_may_not_read_groups_is_offered_none() {
    let mut t = team().await;
    let alice = t.alice.client.user_id();
    t.server.set_user_groups(vec![roster(1, "oncall", &[alice])]);
    t.server.refuse_user_groups_to(alice);
    t.alice.client.full_sync().await.unwrap();
    assert!(backend_of(&t.alice).mentionable_groups(t.channel).is_empty());
  }

  #[tokio::test]
  async fn people_named_directly_are_capped_without_the_permission() {
    let mut t = team().await;
    let bob = t.bob.client.user_id();
    t.server
      .set_my_permissions(t.channel, Some(vec![Permission::CreatePost]));
    t.alice.client.full_sync().await.unwrap();
    let ui = backend_of(&t.alice);
    let body = "@bob ".repeat(DEFAULT_MAX_MENTIONS_WITHOUT_GROUP_MENTIONS + 5);
    ui.post_message(t.channel, None, &body);
    run_commands(&mut t.alice, &ui).await;
    assert_eq!(stored_mentions(&t).await, [bob], "one person, named once");
  }
}

mod older_content {
  use teamcraft_core::store::MessageContent;

  #[test]
  fn a_text_message_from_before_group_mentions_still_reads() {
    let old = r#"{"type":"text","body":"hi @bob","mentions":["9"]}"#;
    let content: MessageContent = serde_json::from_str(old).unwrap();
    let MessageContent::Text { meta, mentions, .. } = &content else {
      panic!("{content:?}");
    };
    assert!(meta.group_mentions.is_empty());
    assert_eq!(mentions.len(), 1);
    assert!(meta.is_plain(), "absent field stays a plain message");
  }

  #[test]
  fn an_unknown_field_is_ignored_and_a_group_mention_round_trips() {
    let newer = r#"{"type":"text","body":"hi","meta":{"group_mentions":["accounting"],"from_the_future":1}}"#;
    let content: MessageContent = serde_json::from_str(newer).unwrap();
    let MessageContent::Text { meta, .. } = &content else {
      panic!("{content:?}");
    };
    assert_eq!(meta.group_mentions, ["accounting"]);
    let json = serde_json::to_string(&content).unwrap();
    assert!(json.contains("group_mentions"), "kept when written back");
    assert!(
      !serde_json::to_string(&MessageContent::Deleted)
        .unwrap()
        .contains("group")
    );
  }
}
