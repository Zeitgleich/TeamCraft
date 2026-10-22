//! Guest lifecycle: expiry, default terms and e-mail domain restrictions.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration as StdDuration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::services::guests::expire_once;
use teamcraft_server::testing::TestApp;
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod expiry {
  use super::*;

  #[tokio::test]
  async fn expired_guests_leave_the_space_and_the_sponsor_is_told() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "client", "public").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert!(reply.body["expires_at"].is_i64(), "default expiry applies");
    add_channel_member(&app, &alice, &channel, &ext_id).await;

    let device = register_device(&app, &alice).await;
    let mut socket = ws::connect(&app, &alice, &device).await;
    ws::events(&mut socket, StdDuration::from_millis(200)).await;

    // Nothing happens before the expiry.
    assert_eq!(
      expire_once(&app.state, OffsetDateTime::now_utc()).await.unwrap(),
      0
    );
    let later = OffsetDateTime::now_utc() + Duration::days(365);
    assert_eq!(expire_once(&app.state, later).await.unwrap(), 1);

    assert_eq!(
      app.get(&ext, &format!("/conversations/{channel}")).await.status,
      StatusCode::NOT_FOUND
    );
    let events = ws::events(&mut socket, StdDuration::from_millis(400)).await;
    assert!(events.iter().any(|event| matches!(
      event,
      GatewayEvent::GuestExpired { user, .. } if user.id.to_string() == ext_id
    )));
    let audit = app.get(&admin(), "/admin/audit").await;
    assert!(audit.body.to_string().contains("guest_expired"));
    app.stop().await;
  }

  #[tokio::test]
  async fn expiry_can_be_extended_and_cleared_but_only_for_guests() {
    let app = app!();
    let alice = owner();
    let (ext, carol) = (guest("ext"), member("carol"));
    let (ext_id, carol_id) = (user_id(&app, &ext).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;

    let soon = (OffsetDateTime::now_utc() + Duration::days(1)).unix_timestamp() * 1000;
    let path = format!("/spaces/{space}/members/{ext_id}");
    let reply = app
      .patch(&alice, &path, json!({"role": "guest", "expires_at": soon}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["expires_at"], soon);
    let reply = app
      .patch(&alice, &path, json!({"role": "guest", "clears_expiry": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["expires_at"].is_null());

    let far = OffsetDateTime::now_utc() + Duration::days(3650);
    assert_eq!(
      expire_once(&app.state, far).await.unwrap(),
      0,
      "cleared expiry never expires"
    );

    let reply = app
      .patch(
        &alice,
        &format!("/spaces/{space}/members/{carol_id}"),
        json!({"role": "member", "expires_at": soon}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn invited_guests_get_the_default_expiry() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let invite = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest"}),
      )
      .await;
    assert_eq!(invite.status, StatusCode::CREATED, "{}", invite.body);
    let ext = guest("ext").with_email("ext@example.com", true);
    assert_eq!(app.get(&ext, "/spaces").await.status, StatusCode::OK);
    let members = app.get(&alice, &format!("/spaces/{space}/members")).await;
    let entry = members.body["members"]
      .as_array()
      .unwrap()
      .iter()
      .find(|member| member["role"] == "guest")
      .expect("guest joined");
    assert!(entry["expires_at"].is_i64(), "{entry}");
    app.stop().await;
  }
}

mod domains {
  use super::*;

  #[tokio::test]
  async fn guest_invites_respect_the_allowed_domains() {
    let Some(app) = TestApp::start_with(|settings| {
      settings.guests.allowed_email_domains = vec!["partner.test".into()];
    })
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let path = format!("/spaces/{space}/invites");
    for (email, role, status) in [
      ("x@partner.test", "guest", StatusCode::CREATED),
      ("x@team.partner.test", "guest", StatusCode::CREATED),
      ("x@notpartner.test", "guest", StatusCode::BAD_REQUEST),
      ("x@other.test", "guest", StatusCode::BAD_REQUEST),
      ("y@other.test", "member", StatusCode::CREATED),
    ] {
      let reply = app
        .post(&alice, &path, json!({"email": email, "role": role}))
        .await;
      assert_eq!(reply.status, status, "{email}: {}", reply.body);
    }
    app.stop().await;
  }
}

mod mail {
  use std::sync::Arc;

  use teamcraft_server::testing::FakeMailer;

  use super::*;

  #[tokio::test]
  async fn invites_are_mailed_when_a_mail_server_is_configured() {
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
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "ext@example.com", "role": "guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);

    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to_address, "ext@example.com");
    assert!(sent[0].subject.contains("Client"), "{}", sent[0].subject);
    assert!(sent[0].text.contains("https://chat.example.com"));
    assert!(sent[0].text.contains("guest"));

    // A refusing mail server does not undo the invite.
    mailer.refuse("other@example.com");
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/invites"),
        json!({"email": "other@example.com", "role": "member"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    app.stop().await;
  }
}
