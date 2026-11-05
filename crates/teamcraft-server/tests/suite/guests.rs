//! Guest lifecycle: expiry, default terms and e-mail domain restrictions.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration as StdDuration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::services::guests::{expire_once, send_notices_once};
use teamcraft_server::testing::{TestApp, TestUser};
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod notices_and_extending {
  use super::*;

  /// A guest in a space of `alice`, ending in `days`.
  async fn guest_ending_in(
    app: &TestApp,
    alice: &TestUser,
    ext: &TestUser,
    days: i64,
  ) -> (String, String, i64) {
    let ext_id = user_id(app, ext).await;
    let space = create_space(app, alice, "Client", "invite_only").await;
    let expires_at = (OffsetDateTime::now_utc() + Duration::days(days)).unix_timestamp() * 1000;
    let reply = app
      .post(
        alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "guest", "expires_at": expires_at}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    (space, ext_id, expires_at)
  }

  #[tokio::test]
  async fn each_lead_time_gives_one_notice_and_a_new_expiry_starts_over() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 30).await;
    let at = |days_before: i64| {
      OffsetDateTime::from_unix_timestamp(expires_at / 1000).unwrap() - Duration::days(days_before)
    };

    // Default lead times: 14 and 3 days.
    assert_eq!(
      send_notices_once(&app.state, at(15)).await.unwrap(),
      0,
      "too early"
    );
    assert_eq!(send_notices_once(&app.state, at(14)).await.unwrap(), 1, "14 days");
    assert_eq!(send_notices_once(&app.state, at(13)).await.unwrap(), 0, "once");
    assert_eq!(send_notices_once(&app.state, at(3)).await.unwrap(), 1, "3 days");
    assert_eq!(send_notices_once(&app.state, at(2)).await.unwrap(), 0, "once");

    let later = expires_at + 30 * 86_400_000;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/guests/{ext_id}/extend"),
        json!({"expires_at": later}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let again = OffsetDateTime::from_unix_timestamp(later / 1000).unwrap() - Duration::days(3);
    assert_eq!(
      send_notices_once(&app.state, again).await.unwrap(),
      1,
      "a new expiry starts over"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_past_two_lead_times_hears_once() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    guest_ending_in(&app, &alice, &ext, 2).await;
    let now = OffsetDateTime::now_utc();
    assert_eq!(send_notices_once(&app.state, now).await.unwrap(), 1);
    assert_eq!(send_notices_once(&app.state, now).await.unwrap(), 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn who_extends_how_far_and_what_they_see() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let (space, ext_id, expires_at) = guest_ending_in(&app, &alice, &ext, 5).await;
    let path = format!("/spaces/{space}/guests/{ext_id}/extend");
    let later = expires_at + 30 * 86_400_000;

    // The guest sees their own, without an offer; a member who isn't concerned sees nothing.
    let own = app.get(&ext, "/guests/expiring").await;
    assert_eq!(own.body["guests"][0]["can_extend"], false, "{}", own.body);
    assert_eq!(
      app.get(&carol, "/guests/expiring").await.body["guests"],
      json!([])
    );

    assert_eq!(
      app.post(&ext, &path, json!({"expires_at": later})).await.status,
      StatusCode::FORBIDDEN,
      "a guest doesn't extend themself"
    );
    assert_eq!(
      app.post(&carol, &path, json!({"expires_at": later})).await.status,
      StatusCode::NOT_FOUND,
      "somebody outside an invite-only space doesn't learn about it"
    );
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": carol_id, "role": "member"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(
      app.post(&carol, &path, json!({"expires_at": later})).await.status,
      StatusCode::FORBIDDEN,
      "a member who isn't the sponsor doesn't extend"
    );

    // Extending is audited, a retry changes nothing and it never shortens.
    let reply = app.post(&alice, &path, json!({"expires_at": later})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["expires_at"], later);
    let reply = app.post(&alice, &path, json!({"expires_at": later})).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let audit = app.get(&admin(), "/admin/audit").await;
    assert_eq!(
      audit.body.to_string().matches("guest_extended").count(),
      1,
      "one entry"
    );
    assert_eq!(
      app
        .post(&alice, &path, json!({"expires_at": expires_at}))
        .await
        .status,
      StatusCode::BAD_REQUEST,
      "extending doesn't shorten"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_overview_filters_and_is_for_admins() {
    let app = app!();
    let alice = owner();
    let ext = guest("ext");
    let (_, ext_id, _) = guest_ending_in(&app, &alice, &ext, 5).await;

    assert_eq!(
      app.get(&member("carol"), "/admin/guests").await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.get(&ext, "/admin/guests").await.status, StatusCode::FORBIDDEN);

    let all = app.get(&admin(), "/admin/guests").await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(all.body["guests"][0]["user"]["id"], ext_id);
    let soon = app.get(&admin(), "/admin/guests?filter=expiring_soon").await;
    assert_eq!(soon.body["guests"][0]["user"]["id"], ext_id);
    let unsponsored = app.get(&admin(), "/admin/guests?filter=no_sponsor").await;
    assert_eq!(unsponsored.body["guests"], json!([]), "alice sponsors");
    app.stop().await;
  }
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
