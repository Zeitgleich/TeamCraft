//! Channels, DMs, memberships and permissions against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_db::testing::sqlx;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn names(reply: &teamcraft_server::testing::Reply, key: &str) -> Vec<String> {
  reply.body[key]
    .as_array()
    .unwrap()
    .iter()
    .map(|item| item["name"].as_str().unwrap_or_default().to_owned())
    .collect()
}

mod channels {
  use super::*;

  #[tokio::test]
  async fn public_channels_are_browsable_and_joinable_by_space_members() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    create_channel(&app, &alice, &space, "general", "public").await;
    create_channel(&app, &alice, &space, "leads", "private").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;

    let browser = app
      .get(&carol, &format!("/conversations?space_id={space}&joinable=true"))
      .await;
    assert_eq!(names(&browser, "conversations"), vec!["general"]);
    assert!(browser.body["conversations"][0].get("my_membership").is_none());

    let general = browser.body["conversations"][0]["id"]
      .as_str()
      .unwrap()
      .to_owned();
    assert_eq!(
      app
        .post(&carol, &format!("/conversations/{general}/join"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    let mine = app.get(&carol, "/conversations").await;
    assert_eq!(names(&mine, "conversations"), vec!["general"]);
    assert_eq!(mine.body["conversations"][0]["my_membership"]["role"], "member");
    // Joined channels leave the browser.
    let browser = app
      .get(&carol, &format!("/conversations?space_id={space}&joinable=true"))
      .await;
    assert!(names(&browser, "conversations").is_empty());
    app.stop().await;
  }

  /// admin.md §2.4, issue #765: the browser lists a space's private channels to workspace admins
  /// (marked private, for a self-join members' apps check), never to members.
  #[tokio::test]
  async fn admins_find_private_channels_in_the_browser_and_members_do_not() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    create_channel(&app, &alice, &space, "general", "public").await;
    create_channel(&app, &alice, &space, "leads", "private").await;
    for user in [&bob, &carol] {
      app.post(user, &format!("/spaces/{space}/join"), json!({})).await;
    }
    let path = format!("/conversations?space_id={space}&joinable=true");
    let for_admin = app.get(&bob, &path).await;
    assert_eq!(names(&for_admin, "conversations"), vec!["general", "leads"]);
    assert_eq!(for_admin.body["conversations"][1]["visibility"], "private");
    assert_eq!(
      names(&app.get(&carol, &path).await, "conversations"),
      vec!["general"]
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn private_channels_are_invisible_to_outsiders_and_unreadable_for_admins() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    let private = create_channel(&app, &alice, &space, "leads", "private").await;
    send(&app, &alice, &private, &device, 1).await;

    assert_eq!(
      app
        .get(&carol, &format!("/conversations/{private}"))
        .await
        .error_code(),
      "channel_not_found"
    );
    assert_eq!(
      app
        .post(&carol, &format!("/conversations/{private}/join"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );

    // The server admin sees the metadata and the roster, but is not a member and can't read.
    assert_eq!(
      app.get(&bob, &format!("/conversations/{private}")).await.status,
      StatusCode::OK
    );
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{private}/members"))
        .await
        .status,
      StatusCode::OK
    );
    let messages = app.get(&bob, &format!("/conversations/{private}/messages")).await;
    assert_eq!(messages.status, StatusCode::FORBIDDEN);
    let post = app
      .post(
        &bob,
        &format!("/conversations/{private}/messages"),
        message_body(2, &register_device(&app, &bob).await),
      )
      .await;
    assert_eq!(post.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }

  #[tokio::test]
  async fn members_are_added_by_channel_admins_and_must_belong_to_the_space() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let channel = create_channel(&app, &alice, &space, "leads", "private").await;

    // Dave is not in the space yet.
    let outsider = app
      .post(
        &alice,
        &format!("/conversations/{channel}/members"),
        json!({"user_id": dave_id}),
      )
      .await;
    assert_eq!(outsider.status, StatusCode::BAD_REQUEST);
    add_channel_member(&app, &alice, &channel, &carol_id).await;

    // Carol is an ordinary member: she can't add anybody or change roles.
    add_space_member(&app, &alice, &space, &dave_id, "member").await;
    let denied = app
      .post(
        &carol,
        &format!("/conversations/{channel}/members"),
        json!({"user_id": dave_id}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let role = app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert_eq!(role.status, StatusCode::FORBIDDEN);

    // Alice promotes carol, who can then add dave.
    let promote = app
      .patch(
        &alice,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert_eq!(promote.status, StatusCode::NO_CONTENT, "{}", promote.body);
    add_channel_member(&app, &carol, &channel, &dave_id).await;
    assert_eq!(
      app.get(&dave, &format!("/conversations/{channel}")).await.status,
      StatusCode::OK
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_removal_is_checked_first_and_removing_twice_succeeds() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let (alice_id, carol_id, dave_id) = (
      user_id(&app, &alice).await,
      user_id(&app, &carol).await,
      user_id(&app, &dave).await,
    );
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "general", "private").await;
    for id in [&carol_id, &dave_id] {
      add_space_member(&app, &alice, &space, id, "member").await;
      add_channel_member(&app, &alice, &channel, id).await;
    }
    let check = |user: &str| format!("/conversations/{channel}/members/{user}/removal");
    let member = |user: &str| format!("/conversations/{channel}/members/{user}");

    // The check answers like the removal would, and changes nothing.
    assert_eq!(
      app.get(&carol, &check(&dave_id)).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app.get(&alice, &check(&alice_id)).await.status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      app.get(&carol, &check(&carol_id)).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&alice, &check(&dave_id)).await.status,
      StatusCode::NO_CONTENT
    );
    let members = app
      .get(&alice, &format!("/conversations/{channel}/members"))
      .await;
    assert_eq!(members.body["members"].as_array().unwrap().len(), 3);

    // Two admins (or a retry after a lost answer) removing the same person: both succeed.
    let dave_path = member(&dave_id);
    let (first, second) = tokio::join!(app.delete(&alice, &dave_path), app.delete(&alice, &dave_path));
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.body);
    assert_eq!(second.status, StatusCode::NO_CONTENT, "{}", second.body);
    assert_eq!(
      app.delete(&alice, &member(&dave_id)).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app.get(&alice, &check(&dave_id)).await.status,
      StatusCode::NO_CONTENT
    );
    // Still only for those who may remove members.
    assert_eq!(
      app.delete(&carol, &member(&dave_id)).await.status,
      StatusCode::FORBIDDEN
    );
    let members = app
      .get(&alice, &format!("/conversations/{channel}/members"))
      .await;
    assert_eq!(members.body["members"].as_array().unwrap().len(), 2);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_last_channel_admin_cannot_leave_or_step_down() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (alice_id, carol_id) = (user_id(&app, &alice).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Team", "open").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    let channel = create_channel(&app, &alice, &space, "general", "public").await;
    app
      .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
      .await;

    let leave = app
      .delete(&alice, &format!("/conversations/{channel}/members/{alice_id}"))
      .await;
    assert_eq!(leave.status, StatusCode::CONFLICT);
    let step_down = app
      .patch(
        &alice,
        &format!("/conversations/{channel}/members/{alice_id}"),
        json!({"role": "member"}),
      )
      .await;
    assert_eq!(step_down.status, StatusCode::CONFLICT);
    // Members can leave freely.
    assert_eq!(
      app
        .delete(&carol, &format!("/conversations/{channel}/members/{carol_id}"))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn moderation_can_turn_a_channel_into_an_announcement_channel() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (alice_device, carol_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &carol).await,
    );
    let space = create_space(&app, &alice, "Team", "open").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    let channel = create_channel(&app, &alice, &space, "news", "public").await;
    app
      .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
      .await;

    // Members can't change moderation; the channel admin can.
    let denied = app
      .patch(
        &carol,
        &format!("/conversations/{channel}"),
        json!({"moderation": {"member_denied": ["create_post"]}}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let updated = app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"moderation": {"member_denied": ["create_post"]}}),
      )
      .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(
      updated.body["moderation"]["member_denied"],
      json!(["create_post"])
    );

    let muted = app
      .post(
        &carol,
        &format!("/conversations/{channel}/messages"),
        message_body(1, &carol_device),
      )
      .await;
    assert_eq!(muted.status, StatusCode::FORBIDDEN);
    send(&app, &alice, &channel, &alice_device, 2).await;
    app.stop().await;
  }

  #[tokio::test]
  async fn archived_channels_are_read_only_and_converting_needs_the_permission() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (alice_device, carol_id) = (register_device(&app, &alice).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Team", "open").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    let channel = create_channel(&app, &alice, &space, "general", "public").await;
    app
      .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
      .await;
    send(&app, &alice, &channel, &alice_device, 1).await;

    let convert = app
      .patch(
        &carol,
        &format!("/conversations/{channel}"),
        json!({"visibility": "private"}),
      )
      .await;
    assert_eq!(convert.status, StatusCode::FORBIDDEN);
    let converted = app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"visibility": "private"}),
      )
      .await;
    assert_eq!(converted.body["visibility"], "private");

    app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": true}),
      )
      .await;
    let post = app
      .post(
        &alice,
        &format!("/conversations/{channel}/messages"),
        message_body(2, &alice_device),
      )
      .await;
    assert_eq!(post.status, StatusCode::FORBIDDEN);
    // History stays readable and the channel can be unarchived.
    assert_eq!(
      app
        .get(&alice, &format!("/conversations/{channel}/messages"))
        .await
        .status,
      StatusCode::OK
    );
    let restored = app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": false}),
      )
      .await;
    assert_eq!(restored.body["is_archived"], false);
    let _ = carol_id;
    app.stop().await;
  }

  #[tokio::test]
  async fn members_set_their_own_preferences_and_read_position() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (alice_device, alice_id, carol_id) = (
      register_device(&app, &alice).await,
      user_id(&app, &alice).await,
      user_id(&app, &carol).await,
    );
    let space = create_space(&app, &alice, "Team", "open").await;
    app
      .post(&carol, &format!("/spaces/{space}/join"), json!({}))
      .await;
    let channel = create_channel(&app, &alice, &space, "general", "public").await;
    app
      .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
      .await;
    let message = send(&app, &alice, &channel, &alice_device, 1).await;

    let prefs = app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"notification_level": "all", "is_favorite": true}),
      )
      .await;
    assert_eq!(prefs.status, StatusCode::NO_CONTENT, "{}", prefs.body);
    let foreign = app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{alice_id}"),
        json!({"is_favorite": true}),
      )
      .await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);

    let read = app
      .put(
        &carol,
        &format!("/conversations/{channel}/read-position"),
        json!({"message_id": message}),
      )
      .await;
    assert_eq!(read.status, StatusCode::NO_CONTENT, "{}", read.body);
    let view = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(view.body["my_membership"]["notification_level"], "all");
    assert_eq!(view.body["my_membership"]["is_favorite"], true);
    assert_eq!(view.body["my_membership"]["last_read_message_id"], message);
    app.stop().await;
  }
}

mod retried_creation {
  use super::*;

  const KEY: &str = "00112233445566778899aabbccddeeff";

  #[tokio::test]
  async fn a_repeated_space_or_channel_request_returns_the_original() {
    let app = app!();
    let (alice, bob) = (owner(), admin());
    let body = json!({"name": "Retried", "join_policy": "open", "idempotency_key": KEY});

    let first = app.post(&alice, "/spaces", body.clone()).await;
    let second = app.post(&alice, "/spaces", body.clone()).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.id(), second.id());
    // The key belongs to the user who made the request.
    let other = app.post(&bob, "/spaces", body).await;
    assert_ne!(other.id(), first.id());
    // Older clients send no key and get a new space every time.
    let keyless = json!({"name": "Retried", "join_policy": "open"});
    let (one, two) = (
      app.post(&alice, "/spaces", keyless.clone()).await,
      app.post(&alice, "/spaces", keyless).await,
    );
    assert_ne!(one.id(), two.id());
    let spaces: i64 = sqlx::query_scalar("SELECT count(*) FROM spaces WHERE name = 'Retried'")
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(spaces, 4);

    let space = first.id();
    let channel = json!({"name": "retried", "visibility": "public", "idempotency_key": KEY});
    let path = format!("/spaces/{space}/channels");
    let created = app.post(&alice, &path, channel.clone()).await;
    let repeated = app.post(&alice, &path, channel).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.id(), repeated.id());
    let channels: i64 = sqlx::query_scalar("SELECT count(*) FROM conversations WHERE name = 'retried'")
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(channels, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn concurrent_requests_with_one_key_create_one_space() {
    let app = app!();
    let alice = owner();
    let body = json!({"name": "Racing", "join_policy": "open", "idempotency_key": KEY});

    let (a, b, c) = tokio::join!(
      app.post(&alice, "/spaces", body.clone()),
      app.post(&alice, "/spaces", body.clone()),
      app.post(&alice, "/spaces", body.clone()),
    );

    assert_eq!(a.status, StatusCode::CREATED, "{}", a.body);
    assert_eq!((a.id(), a.id()), (b.id(), c.id()));
    let spaces: i64 = sqlx::query_scalar("SELECT count(*) FROM spaces WHERE name = 'Racing'")
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(spaces, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_malformed_key_is_rejected() {
    let app = app!();
    let alice = owner();
    let reply = app
      .post(
        &alice,
        "/spaces",
        json!({"name": "Bad", "join_policy": "open", "idempotency_key": "short"}),
      )
      .await;
    assert!(reply.status.is_client_error(), "{}", reply.body);
    app.stop().await;
  }
}

mod direct_messages {
  use super::*;

  #[tokio::test]
  async fn a_dm_is_created_once_and_is_private_to_its_participants() {
    let app = app!();
    let (carol, dave, bob) = (member("carol"), member("dave"), admin());
    let (dave_id, carol_id) = (user_id(&app, &dave).await, user_id(&app, &carol).await);

    let first = app.post(&carol, "/dms", json!({"user_id": dave_id})).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let second = app.post(&dave, "/dms", json!({"user_id": carol_id})).await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(first.id(), second.id());
    assert_eq!(first.body["kind"], "dm");
    assert_eq!(first.body["participants"].as_array().unwrap().len(), 2);

    // Not even server admins can see a DM.
    assert_eq!(
      app
        .get(&bob, &format!("/conversations/{}", first.id()))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    // A DM with oneself is the user's notes to self, with the user as its only participant.
    let notes = app.post(&carol, "/dms", json!({"user_id": carol_id})).await;
    assert_eq!(notes.status, StatusCode::CREATED, "{}", notes.body);
    assert_eq!(notes.body["participants"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn notes_to_self_opened_by_two_devices_at_once_are_one_conversation() {
    let app = app!();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;

    let body = json!({"user_id": carol_id});
    let (first, second) = tokio::join!(
      app.post(&carol, "/dms", body.clone()),
      app.post(&carol, "/dms", body)
    );
    assert!(first.status.is_success(), "{}", first.body);
    assert!(second.status.is_success(), "{}", second.body);
    assert_eq!(first.id(), second.id());
    let list = app.get(&carol, "/conversations").await;
    let dms = list.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|conversation| conversation["kind"] == "dm")
      .count();
    assert_eq!(dms, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_dm_keeps_its_participants_when_one_is_deactivated() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
    let dm = app.post(&carol, "/dms", json!({"user_id": dave_id})).await;
    assert_eq!(dm.status, StatusCode::CREATED, "{}", dm.body);

    let done = app
      .post(&bob, &format!("/admin/users/{dave_id}/deactivate"), json!({}))
      .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.body);

    // Issue #643: with Dave's membership gone the DM must not look like Carol's notes to self.
    let reply = app.get(&carol, &format!("/conversations/{}", dm.id())).await;
    let mut participants: Vec<String> = reply.body["participants"]
      .as_array()
      .unwrap()
      .iter()
      .map(|user| user["id"].as_str().unwrap().to_owned())
      .collect();
    participants.sort();
    let mut expected = vec![carol_id, dave_id];
    expected.sort();
    assert_eq!(participants, expected);
    app.stop().await;
  }

  #[tokio::test]
  async fn group_dms_have_size_limits_and_are_reused() {
    let app = app!();
    let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
    let (dave_id, erin_id) = (user_id(&app, &dave).await, user_id(&app, &erin).await);

    let group = app
      .post(&carol, "/group-dms", json!({"user_ids": [dave_id, erin_id]}))
      .await;
    assert_eq!(group.status, StatusCode::CREATED, "{}", group.body);
    assert_eq!(group.body["kind"], "group");
    let again = app
      .post(&carol, "/group-dms", json!({"user_ids": [erin_id, dave_id]}))
      .await;
    assert_eq!((again.status, again.id()), (StatusCode::OK, group.id()));

    let too_small = app
      .post(&carol, "/group-dms", json!({"user_ids": [dave_id]}))
      .await;
    assert_eq!(too_small.status, StatusCode::BAD_REQUEST);
    let unknown = app
      .post(&carol, "/group-dms", json!({"user_ids": [dave_id, "999999"]}))
      .await;
    assert_eq!(unknown.error_code(), "user_not_found");
    app.stop().await;
  }
}

mod guests {
  use super::*;

  /// alice owns a space with two public channels; carol is a member of both, dave of neither.
  async fn setup(app: &TestApp) -> (String, String, String) {
    let alice = owner();
    let (carol, dave) = (member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(app, &carol).await, user_id(app, &dave).await);
    let space = create_space(app, &alice, "Client Project", "invite_only").await;
    add_space_member(app, &alice, &space, &carol_id, "member").await;
    add_space_member(app, &alice, &space, &dave_id, "member").await;
    let shared = create_channel(app, &alice, &space, "client", "public").await;
    let internal = create_channel(app, &alice, &space, "internal", "public").await;
    add_channel_member(app, &alice, &shared, &carol_id).await;
    add_channel_member(app, &alice, &internal, &carol_id).await;
    (space, shared, internal)
  }

  fn external() -> teamcraft_server::testing::TestUser {
    guest("ext").with_email("ext@example.com", true)
  }

  #[tokio::test]
  async fn invited_guests_land_in_the_listed_channels_only() {
    let app = app!();
    let alice = owner();
    let (space, shared, internal) = setup(&app).await;
    let invite = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "Ext@Example.com", "role": "guest", "channel_ids": [shared]}),
      )
      .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    assert_eq!(invite.body["email"], "ext@example.com");
    assert_eq!(
      app.get(&alice, &format!("/spaces/{space}/invites")).await.body["invites"]
        .as_array()
        .unwrap()
        .len(),
      1
    );

    // First sign-in with the verified address applies the invite.
    let ext = external();
    let spaces = app.get(&ext, "/spaces").await;
    assert_eq!(names(&spaces, "spaces"), vec!["Client Project"]);
    assert_eq!(spaces.body["spaces"][0]["my_role"], "guest");
    let conversations = app.get(&ext, "/conversations").await;
    assert_eq!(names(&conversations, "conversations"), vec!["client"]);
    assert_eq!(conversations.body["conversations"][0]["contains_guests"], true);
    assert_eq!(
      app.get(&ext, &format!("/conversations/{internal}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.get(&alice, &format!("/spaces/{space}/invites")).await.body["invites"]
        .as_array()
        .unwrap()
        .len(),
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invites_need_a_verified_email_match() {
    let app = app!();
    let alice = owner();
    let (space, shared, _) = setup(&app).await;
    app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest", "channel_ids": [shared]}),
      )
      .await;

    let unverified = guest("ext").with_email("ext@example.com", false);
    assert!(names(&app.get(&unverified, "/spaces").await, "spaces").is_empty());
    let other_address = guest("other").with_email("other@example.com", true);
    assert!(names(&app.get(&other_address, "/spaces").await, "spaces").is_empty());
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_are_limited_in_what_they_can_do_and_find() {
    let app = app!();
    let alice = owner();
    let (space, shared, internal) = setup(&app).await;
    app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest", "channel_ids": [shared]}),
      )
      .await;
    let ext = external();
    app.get(&ext, "/me").await;

    // No channel creation, no roster, no channel browser, no joining, no inviting.
    let create = app
      .post(
        &ext,
        &format!("/spaces/{space}/channels"),
        json!({"name": "x", "visibility": "public"}),
      )
      .await;
    assert_eq!(create.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app.get(&ext, &format!("/spaces/{space}/members")).await.status,
      StatusCode::FORBIDDEN
    );
    let browser = app
      .get(&ext, &format!("/conversations?space_id={space}&joinable=true"))
      .await;
    assert_eq!(browser.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app
        .post(&ext, &format!("/conversations/{internal}/join"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let invite = app
      .post(
        &ext,
        &format!("/spaces/{space}/invites"),
        json!({"email": "x@example.com", "role": "guest"}),
      )
      .await;
    assert_eq!(invite.status, StatusCode::FORBIDDEN);

    // Directory: alice (channel creator) and carol share a channel with the guest, dave doesn't.
    let found = app.get(&ext, "/users?query=").await;
    assert_eq!(names_of_users(&found), vec!["alice", "carol", "ext"]);
    let dave_id = user_id(&app, &member("dave")).await;
    assert_eq!(
      app.get(&ext, &format!("/users/{dave_id}")).await.error_code(),
      "user_not_found"
    );
    let members_see_everyone = app.get(&member("carol"), "/users?query=").await;
    assert!(names_of_users(&members_see_everyone).len() >= 4);
    app.stop().await;
  }

  fn names_of_users(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
    let mut names: Vec<String> = reply.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .map(|user| user["username"].as_str().unwrap().to_owned())
      .collect();
    names.sort();
    names
  }

  #[tokio::test]
  async fn guests_only_open_dms_with_people_they_share_a_conversation_with() {
    let app = app!();
    let alice = owner();
    let (space, shared, _) = setup(&app).await;
    app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest", "channel_ids": [shared]}),
      )
      .await;
    let ext = external();
    let (carol_id, dave_id, ext_id) = (
      user_id(&app, &member("carol")).await,
      user_id(&app, &member("dave")).await,
      user_id(&app, &ext).await,
    );

    assert_eq!(
      app.post(&ext, "/dms", json!({"user_id": carol_id})).await.status,
      StatusCode::CREATED
    );
    assert_eq!(
      app
        .post(&ext, "/dms", json!({"user_id": dave_id}))
        .await
        .error_code(),
      "user_not_found"
    );
    // The other direction is restricted as well.
    assert_eq!(
      app
        .post(&member("dave"), "/dms", json!({"user_id": ext_id}))
        .await
        .error_code(),
      "user_not_found"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_dm_restriction_for_guests_is_a_policy_toggle() {
    let Some(app) =
      TestApp::start_with(|settings| settings.policy.guests_dm_only_with_shared_conversations = false).await
    else {
      return;
    };
    let guest = guest("ext");
    let dave_id = user_id(&app, &member("dave")).await;
    assert_eq!(
      app.post(&guest, "/dms", json!({"user_id": dave_id})).await.status,
      StatusCode::CREATED
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn guest_accounts_cannot_get_member_roles_or_channel_admin() {
    let app = app!();
    let alice = owner();
    let (space, shared, _) = setup(&app).await;
    let ext = guest("ext");
    let ext_id = user_id(&app, &ext).await;

    let as_member = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "member"}),
      )
      .await;
    assert_eq!(as_member.status, StatusCode::BAD_REQUEST);
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    add_channel_member(&app, &alice, &shared, &ext_id).await;
    let admin_role = app
      .patch(
        &alice,
        &format!("/conversations/{shared}/members/{ext_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert_eq!(admin_role.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }
}
