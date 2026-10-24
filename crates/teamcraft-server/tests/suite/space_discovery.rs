//! Finding and joining spaces and inviting people (issue #317), against a real server and
//! Postgres: what the space list says about each space (member count, the caller's abilities),
//! open versus invite-only spaces, guest limits, the personal message of invitation e-mails, and
//! that space membership never puts anyone into a channel's encryption group.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{FakeMailer, TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod browsing {
  use super::*;

  #[tokio::test]
  async fn open_spaces_are_listed_with_their_member_count_and_joined_in_one_step() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let open = create_space(&app, &alice, "Project A", "open").await;
    let closed = create_space(&app, &alice, "Project B", "invite_only").await;

    let listed = spaces_of(&app, &carol).await;
    let entry = find(&listed, &open).expect("open spaces are listed for everyone");
    assert_eq!(entry["member_count"], json!(1));
    assert!(entry.get("my_role").is_none());
    assert!(find(&listed, &closed).is_none(), "invite-only spaces stay hidden");

    let reply = app.post(&carol, &format!("/spaces/{open}/join"), json!({})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["member_count"], json!(2));
    assert_eq!(reply.body["my_role"], json!("member"));

    let reply = app
      .post(&carol, &format!("/spaces/{closed}/join"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_neither_browse_nor_join_open_spaces() {
    let app = app!();
    let (alice, ext) = (owner(), guest("ext"));
    let open = create_space(&app, &alice, "Project A", "open").await;

    assert!(find(&spaces_of(&app, &ext).await, &open).is_none());
    let reply = app.post(&ext, &format!("/spaces/{open}/join"), json!({})).await;
    assert!(
      matches!(reply.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
      "{}: {}",
      reply.status,
      reply.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_list_tells_who_may_invite_and_archiving_ends_it() {
    let app = app!();
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    add_space_member(&app, &alice, &space, &user_id(&app, &carol).await, "space_admin").await;
    add_space_member(&app, &alice, &space, &user_id(&app, &dave).await, "member").await;

    let as_admin = space_of(&app, &carol, &space).await;
    assert_eq!(as_admin["can_add_members"], json!(true));
    assert_eq!(as_admin["can_invite_guests"], json!(true));
    assert_eq!(as_admin["member_count"], json!(3));
    let as_member = space_of(&app, &dave, &space).await;
    assert_eq!(as_member["can_add_members"], json!(false));
    assert_eq!(as_member["can_invite_guests"], json!(false));

    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
      space_of(&app, &carol, &space).await["can_add_members"],
      json!(false)
    );
    app.stop().await;
  }
}

mod inviting {
  use super::*;

  #[tokio::test]
  async fn plain_members_can_neither_add_people_nor_invite_guests() {
    let app = app!();
    let (alice, dave, erin) = (owner(), member("dave"), member("erin"));
    let space = create_space(&app, &alice, "Project A", "open").await;
    add_space_member(&app, &alice, &space, &user_id(&app, &dave).await, "member").await;
    let erin_id = user_id(&app, &erin).await;

    for role in ["member", "guest"] {
      let reply = app
        .post(
          &dave,
          &format!("/spaces/{space}/members"),
          json!({"user_id": erin_id, "role": role}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::FORBIDDEN, "{role}: {}", reply.body);
      let reply = app
        .post(
          &dave,
          &format!("/spaces/{space}/invites"),
          json!({"email": "new@example.com", "role": role}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::FORBIDDEN, "{role}: {}", reply.body);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn guest_accounts_join_only_as_guests() {
    let app = app!();
    let (alice, ext) = (owner(), guest("ext"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let ext_id = user_id(&app, &ext).await;

    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "member"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    assert_eq!(space_of(&app, &ext, &space).await["my_role"], json!("guest"));
    app.stop().await;
  }

  #[tokio::test]
  async fn joining_a_space_puts_nobody_into_a_channel_group() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "general", "public").await;
    let carol_id = user_id(&app, &carol).await;

    add_space_member(&app, &alice, &space, &carol_id, "member").await;

    // Channels are joined through their encryption group (a signed self-add), never by the server
    // on its own (e2ee.md §4.6).
    let members = app
      .get(&alice, &format!("/conversations/{channel}/members"))
      .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let ids: Vec<&str> = members.body["members"]
      .as_array()
      .unwrap()
      .iter()
      .filter_map(|member| member["user"]["id"].as_str())
      .collect();
    assert!(!ids.contains(&carol_id.as_str()), "{ids:?}");
    app.stop().await;
  }
}

mod invitation_mail {
  use super::*;

  #[tokio::test]
  async fn the_personal_message_is_quoted_in_the_invitation() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.mail.public_base_url = "https://chat.example.com".into(),
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "new@example.com", "role": "member", "message": "  Welcome aboard!\nSee you {url}  "}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert!(
      sent[0].text.contains("> Welcome aboard!\n> See you {url}"),
      "{}",
      sent[0].text
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_overlong_message_is_refused_before_anything_is_sent() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(|_| {}, mailer.clone()).await else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "new@example.com", "role": "member", "message": "x".repeat(100_000)}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(mailer.sent().is_empty());
    let pending = app.get(&alice, &format!("/spaces/{space}/invites")).await;
    assert_eq!(pending.body["invites"], json!([]));
    app.stop().await;
  }
}

async fn spaces_of(app: &TestApp, user: &TestUser) -> Vec<Value> {
  let reply = app.get(user, "/spaces").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["spaces"].as_array().unwrap().clone()
}

async fn space_of(app: &TestApp, user: &TestUser, space: &str) -> Value {
  let reply = app.get(user, &format!("/spaces/{space}")).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body
}

fn find<'a>(spaces: &'a [Value], id: &str) -> Option<&'a Value> {
  spaces.iter().find(|space| space["id"] == json!(id))
}
