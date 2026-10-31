//! The import account's endpoints (admin.md §3.8, e2ee.md §4.10): only the account the signed policy
//! names may import, imported messages go before a conversation's live ones and count as unread
//! for nobody, a DM gets the importer as a member that is no participant, and the importer passes
//! the rate limits.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::fake_keycloak::{CLIENT_SECRET, FakeKeycloak, fake_user};
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{ImportPolicy, MessageId, UserId};
use teamcraft_server::testing::{TestApp, TestUser};

/// 2024-05-01, before the Snowflake epoch.
const OLD_MS: i64 = 1_714_521_600_000;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod policy {
  use super::*;

  #[tokio::test]
  async fn only_the_named_importer_may_import_and_only_until_the_date() {
    let app = app!();
    let importer = admin();
    let importer_id = user_id(&app, &importer).await;
    let carol = user_id(&app, &member("carol")).await;
    let request = json!({"participants": [carol]});

    let refused = app.post(&importer, "/admin/import/dms", request.clone()).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.body["error_code"], "import_not_allowed");

    allow_import(&app, &owner_as_importer(&app).await, i64::MAX);
    let other = app.post(&importer, "/admin/import/dms", request.clone()).await;
    assert_eq!(other.body["error_code"], "import_not_allowed", "{}", other.body);

    allow_import(&app, &importer_id, now_ms() - 1);
    let expired = app.post(&importer, "/admin/import/dms", request.clone()).await;
    assert_eq!(
      expired.body["error_code"], "import_not_allowed",
      "{}",
      expired.body
    );

    allow_import(&app, &importer_id, i64::MAX);
    let allowed = app.post(&importer, "/admin/import/dms", request).await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.body);
  }

  #[tokio::test]
  async fn members_never_reach_the_import_endpoints() {
    let app = app!();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    allow_import(&app, &carol_id, i64::MAX);
    let refused = app
      .post(&carol, "/admin/import/dms", json!({"participants": [carol_id]}))
      .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
  }
}

mod messages {
  use super::*;

  #[tokio::test]
  async fn imported_messages_go_before_the_live_ones_in_their_order() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let live = list(&app, &carol, &channel).await;
    let live_id: MessageId = live[0]["id"].as_str().unwrap().parse().unwrap();

    let ids = import(
      &app,
      &importer,
      &channel,
      &device,
      &[(10, OLD_MS), (11, OLD_MS - 5), (12, OLD_MS + 1)],
    )
    .await;
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    assert!(ids.iter().all(|id| *id < live_id));
    assert_eq!(
      ids[0].to_string().parse::<i64>().unwrap() >> 22,
      0,
      "pre-epoch times take the epoch"
    );

    let history = list(&app, &carol, &channel).await;
    let order: Vec<&str> = history.iter().rev().map(|m| m["id"].as_str().unwrap()).collect();
    let expected: Vec<String> = ids
      .iter()
      .map(ToString::to_string)
      .chain([live_id.to_string()])
      .collect();
    assert_eq!(order, expected);
  }

  #[tokio::test]
  async fn a_second_run_returns_the_same_ids_and_stores_nothing_new() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let first = import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    let again = import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    assert_eq!(first, again);
    assert_eq!(list(&app, &carol, &channel).await.len(), 3);
  }

  #[tokio::test]
  async fn two_conversations_never_get_the_same_id() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let carol_id = user_id(&app, &carol).await;
    let notes = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [carol_id]}),
      )
      .await
      .id();
    let in_channel = import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    let in_notes = import(&app, &importer, &notes, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    assert!(
      in_notes.iter().all(|id| !in_channel.contains(id)),
      "{in_channel:?} {in_notes:?}"
    );
    assert!(in_notes[0] > in_channel[1]);
    for id in in_channel.iter().chain(&in_notes) {
      assert_eq!(
        teamcraft_proto::Id::snowflake(*id).worker(),
        teamcraft_proto::IMPORT_WORKER
      );
    }
  }

  #[tokio::test]
  async fn even_a_later_original_time_goes_before_the_live_messages() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let live: MessageId = list(&app, &carol, &channel).await[0]["id"]
      .as_str()
      .unwrap()
      .parse()
      .unwrap();
    let ids = import(&app, &importer, &channel, &device, &[(10, now_ms() + 60_000)]).await;
    assert!(ids[0] < live);
  }

  #[tokio::test]
  async fn imported_messages_are_unread_for_nobody() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    mark_all_read(&app, &carol, &channel).await;
    import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    let unread = app.get(&carol, "/unread").await;
    assert!(
      !unread.body["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["conversation_id"] == channel.as_str()),
      "{}",
      unread.body
    );
  }

  #[tokio::test]
  async fn a_reply_needs_an_imported_root_of_the_conversation() {
    let app = app!();
    let (importer, device, channel, _) = channel_with_live_message(&app).await;
    let root = import(&app, &importer, &channel, &device, &[(10, OLD_MS)]).await[0];
    let mut reply = imported(11, OLD_MS + 1);
    reply["thread_id"] = json!(root.to_string());
    let answered = app
      .post(
        &importer,
        &format!("/admin/import/conversations/{channel}/messages"),
        json!({"device_id": device, "messages": [reply]}),
      )
      .await;
    assert_eq!(answered.status, StatusCode::OK, "{}", answered.body);

    let mut stray = imported(12, OLD_MS + 2);
    stray["thread_id"] = json!("12345");
    let refused = app
      .post(
        &importer,
        &format!("/admin/import/conversations/{channel}/messages"),
        json!({"device_id": device, "messages": [stray]}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
  }

  #[tokio::test]
  async fn finishing_announces_the_imported_history() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let ids = import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    let before = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert!(before.body.get("imported_through").is_none(), "{}", before.body);
    let finished = app
      .post(
        &importer,
        &format!("/admin/import/conversations/{channel}/finish"),
        json!({}),
      )
      .await;
    assert_eq!(finished.status, StatusCode::NO_CONTENT, "{}", finished.body);
    let after = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(
      after.body["imported_through"],
      ids[1].to_string().as_str(),
      "{}",
      after.body
    );
  }
}

mod dms {
  use super::*;

  #[tokio::test]
  async fn the_importer_is_a_member_but_no_participant_and_its_history_is_visible() {
    let app = app!();
    let (importer, device) = importer_with_device(&app).await;
    let importer_id = user_id(&app, &importer).await;
    let (carol, dave) = (member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);

    let dm = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [carol_id, dave_id]}),
      )
      .await;
    assert_eq!(dm.status, StatusCode::OK, "{}", dm.body);
    let dm_id = dm.id();
    assert_eq!(dm.body["kind"], "dm");
    assert_eq!(dm.body["importer_user_id"], importer_id.as_str());
    let participants: Vec<&str> = dm.body["participants"]
      .as_array()
      .unwrap()
      .iter()
      .map(|user| user["id"].as_str().unwrap())
      .collect();
    assert!(!participants.contains(&importer_id.as_str()), "{}", dm.body);

    // The participants' own DM is the same conversation.
    let opened = app.post(&carol, "/dms", json!({"user_id": dave_id})).await;
    assert_eq!(opened.id(), dm_id);
    // Opening it again for the import changes nothing.
    let again = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [dave_id, carol_id]}),
      )
      .await;
    assert_eq!(again.id(), dm_id);

    // DMs share history from the join on; imported history is older and still visible.
    import(&app, &importer, &dm_id, &device, &[(10, OLD_MS)]).await;
    assert_eq!(list(&app, &dave, &dm_id).await.len(), 1);

    let left = app
      .delete(
        &importer,
        &format!("/conversations/{dm_id}/members/{importer_id}"),
      )
      .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT, "{}", left.body);
    let after = app.get(&carol, &format!("/conversations/{dm_id}")).await;
    assert!(after.body.get("importer_user_id").is_none(), "{}", after.body);
    let gone = app.get(&importer, &format!("/conversations/{dm_id}")).await;
    assert_ne!(gone.status, StatusCode::OK, "{}", gone.body);
  }

  #[tokio::test]
  async fn one_participant_is_their_notes() {
    let app = app!();
    let (importer, _) = importer_with_device(&app).await;
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let notes = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [carol_id]}),
      )
      .await;
    assert_eq!(notes.status, StatusCode::OK, "{}", notes.body);
    let own = app.post(&carol, "/dms", json!({"user_id": carol_id})).await;
    assert_eq!(own.id(), notes.id());
  }

  #[tokio::test]
  async fn the_importer_is_no_participant_of_its_own_import() {
    let app = app!();
    let (importer, _) = importer_with_device(&app).await;
    let importer_id = user_id(&app, &importer).await;
    let refused = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [importer_id]}),
      )
      .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
  }
}

mod member_state {
  use super::*;

  #[tokio::test]
  async fn read_position_favourite_and_mute_come_from_the_old_system() {
    let app = app!();
    let (importer, device, channel, carol) = channel_with_live_message(&app).await;
    let carol_id = user_id(&app, &carol).await;
    let ids = import(&app, &importer, &channel, &device, &[(10, OLD_MS), (11, OLD_MS)]).await;
    let set = app
      .put(
        &importer,
        &format!("/admin/import/conversations/{channel}/members/{carol_id}"),
        json!({"read_through": ids[0].to_string(), "is_favorite": true, "is_muted": true}),
      )
      .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.body);
    let membership =
      app.get(&carol, &format!("/conversations/{channel}")).await.body["my_membership"].clone();
    assert_eq!(
      membership["last_read_message_id"],
      ids[0].to_string().as_str(),
      "{membership}"
    );
    assert_eq!(membership["is_favorite"], true);
    assert_eq!(membership["notification_level"], "none");
  }
}

mod provisioning {
  use super::*;

  #[tokio::test]
  async fn accounts_exist_before_their_first_sign_in_and_the_sign_in_finds_them() {
    let provider = FakeKeycloak::start(vec![
      fake_user("sub-bob", &["admin"]),
      fake_user("sub-erin", &["member"]),
      fake_user("sub-frank", &[]),
    ])
    .await;
    let settings = provider.sync_settings(CLIENT_SECRET, 50);
    let Some(app) = TestApp::start_with(|s| s.maybe_oidc_sync = Some(settings)).await else {
      return;
    };
    let importer = admin();
    let importer_id = user_id(&app, &importer).await;
    allow_import(&app, &importer_id, i64::MAX);

    let reply = app
      .post(
        &importer,
        "/admin/import/users",
        json!({"usernames": ["sub-erin", "sub-frank", "sub-nobody", "sub-bob"]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let statuses: Vec<&str> = reply.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .map(|user| user["status"].as_str().unwrap())
      .collect();
    assert_eq!(statuses, ["created", "not_found", "not_found", "existing"]);
    let erin_id = reply.body["users"][0]["user_id"].as_str().unwrap().to_owned();
    assert_eq!(reply.body["users"][3]["user_id"], importer_id.as_str());

    let again = app
      .post(
        &importer,
        "/admin/import/users",
        json!({"usernames": ["sub-erin"]}),
      )
      .await;
    assert_eq!(again.body["users"][0]["status"], "existing", "{}", again.body);
    assert_eq!(user_id(&app, &member("erin")).await, erin_id);
  }

  #[tokio::test]
  async fn without_the_provider_sync_nothing_is_provisioned() {
    let app = app!();
    let importer = admin();
    let importer_id = user_id(&app, &importer).await;
    allow_import(&app, &importer_id, i64::MAX);
    let reply = app
      .post(
        &importer,
        "/admin/import/users",
        json!({"usernames": ["sub-erin"]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.body);
  }
}

mod limits {
  use super::*;

  #[tokio::test]
  async fn the_importer_passes_the_rate_limits_others_do_not() {
    let Some(app) = TestApp::start_with(|s| {
      s.security.rate_limits.is_enabled = true;
      s.security.rate_limits.user_writes_per_minute = 2;
      s.security.rate_limits.ip_requests_per_minute = 30;
    })
    .await
    else {
      return;
    };
    let (importer, device) = importer_with_device(&app).await;
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let dm = app
      .post(
        &importer,
        "/admin/import/dms",
        json!({"participants": [carol_id]}),
      )
      .await
      .id();
    for n in 0..40u8 {
      let reply = import_reply(&app, &importer, &dm, &device, &[(n, OLD_MS)]).await;
      assert_eq!(reply.status, StatusCode::OK, "request {n}: {}", reply.body);
    }
    let carol_device = register_device(&app, &carol).await;
    let mut statuses = Vec::new();
    for n in 0..3u8 {
      statuses.push(
        app
          .post(
            &carol,
            &format!("/conversations/{dm}/messages"),
            message_body(100 + n, &carol_device),
          )
          .await
          .status,
      );
    }
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS), "{statuses:?}");
  }
}

/// A channel that shares its full history, with the importer (the admin), carol and one live
/// message of carol's; the import is allowed.
async fn channel_with_live_message(app: &TestApp) -> (TestUser, String, String, TestUser) {
  let (importer, device) = importer_with_device(app).await;
  let carol = member("carol");
  let alice = owner();
  let space = create_space(app, &alice, "Team", "invite_only").await;
  let created = app
    .post(
      &alice,
      &format!("/spaces/{space}/channels"),
      json!({"name": "history", "visibility": "private", "history_policy": "full"}),
    )
    .await;
  assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
  let channel = created.id();
  for user in [&importer, &carol] {
    let id = user_id(app, user).await;
    add_space_member(app, &alice, &space, &id, "member").await;
    add_channel_member(app, &alice, &channel, &id).await;
  }
  let carol_device = register_device(app, &carol).await;
  send(app, &carol, &channel, &carol_device, 1).await;
  (importer, device, channel, carol)
}

async fn importer_with_device(app: &TestApp) -> (TestUser, String) {
  let importer = admin();
  let importer_id = user_id(app, &importer).await;
  allow_import(app, &importer_id, i64::MAX);
  let device = register_device(app, &importer).await;
  (importer, device)
}

async fn owner_as_importer(app: &TestApp) -> String {
  user_id(app, &owner()).await
}

/// Puts the server's copy of the signed policy in force directly (the signature is the clients'
/// business; the server only reads the policy).
fn allow_import(app: &TestApp, importer: &str, until_ms: i64) {
  let mut policy = app.state.policy.get().as_ref().clone();
  policy.import = ImportPolicy {
    maybe_importer: Some(importer.parse::<UserId>().unwrap()),
    maybe_joins_until_ms: Some(until_ms),
  };
  app.state.policy.set(policy);
}

fn imported(n: u8, sent_at_ms: i64) -> Value {
  json!({
    "client_message_id": client_id(n),
    "content_key_id": "a2V5",
    "ciphertext": "Y2lwaGVydGV4dA==",
    "sent_at_ms": sent_at_ms,
  })
}

async fn import_reply(
  app: &TestApp,
  importer: &TestUser,
  conversation: &str,
  device: &str,
  messages: &[(u8, i64)],
) -> teamcraft_server::testing::Reply {
  app
    .post(
      importer,
      &format!("/admin/import/conversations/{conversation}/messages"),
      json!({
        "device_id": device,
        "messages": messages.iter().map(|(n, at)| imported(*n, *at)).collect::<Vec<_>>(),
      }),
    )
    .await
}

async fn import(
  app: &TestApp,
  importer: &TestUser,
  conversation: &str,
  device: &str,
  messages: &[(u8, i64)],
) -> Vec<MessageId> {
  let reply = import_reply(app, importer, conversation, device, messages).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .map(|entry| entry["id"].as_str().unwrap().parse().unwrap())
    .collect()
}

async fn list(app: &TestApp, user: &TestUser, conversation: &str) -> Vec<Value> {
  let reply = app
    .get(user, &format!("/conversations/{conversation}/messages"))
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["messages"].as_array().unwrap().clone()
}

async fn mark_all_read(app: &TestApp, user: &TestUser, conversation: &str) {
  let newest = list(app, user, conversation).await[0]["id"]
    .as_str()
    .unwrap()
    .to_owned();
  let reply = app
    .put(
      user,
      &format!("/conversations/{conversation}/read-position"),
      json!({"message_id": newest}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
}

fn now_ms() -> i64 {
  i64::try_from(
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_millis(),
  )
  .unwrap()
}
