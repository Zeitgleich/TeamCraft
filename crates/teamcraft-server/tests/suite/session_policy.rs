//! Session policy and directory privacy of the signed workspace policy, enforced by the server.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::testing::sqlx;
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// Signs (as far as the server cares) a policy with the edits applied on top of the default.
async fn publish_policy(app: &TestApp, version: u64, edit: impl FnOnce(&mut Value)) {
  use base64::Engine;
  let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
  // The owner signs in with a second factor, or a policy requiring one would lock them out.
  let alice = owner().with_authentication_methods(&["pwd", "otp"]);
  let device = register_device_once(app, &alice).await;
  let mut value = serde_json::to_value(PolicyDocument {
    version,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]);
  let reply = app
    .post(
      &alice,
      "/workspace/policy",
      json!({
        "expected_version": version - 1,
        "payload": b64(&serde_json::to_vec(&value).unwrap()),
        "signature": b64(b"sig"),
        "signer_device_id": device,
      }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

async fn register_device_once(app: &TestApp, user: &TestUser) -> String {
  let devices = app.get(user, "/devices").await;
  match devices.body["devices"].as_array().and_then(|list| list.first()) {
    Some(device) => device["id"].as_str().unwrap().to_owned(),
    None => register_device(app, user).await,
  }
}

async fn session_rows(app: &TestApp) -> i64 {
  sqlx::query_scalar("SELECT count(*) FROM session_activity")
    .fetch_one(app.sql_pool())
    .await
    .unwrap()
}

mod limits {
  use super::*;

  #[tokio::test]
  async fn without_limits_nothing_is_tracked() {
    let app = app!();
    let carol = member("carol").with_session("sid-1");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(session_rows(&app).await, 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_session_ends_after_its_maximum_age() {
    let app = app!();
    publish_policy(&app, 1, |p| p["session"] = json!({"max_age_hours": 8})).await;
    let carol = member("carol").with_session("sid-1");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(session_rows(&app).await, 1);

    sqlx::query("UPDATE session_activity SET started_at = now() - interval '9 hours'")
      .execute(app.sql_pool())
      .await
      .unwrap();
    let reply = app.get(&carol, "/me").await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::UNAUTHORIZED, "session_timed_out")
    );
    // Another session of the same person is not affected.
    let other = member("carol").with_session("sid-2");
    assert_eq!(app.get(&other, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_idle_session_ends_but_an_active_one_goes_on() {
    let app = app!();
    publish_policy(&app, 1, |p| p["session"] = json!({"idle_timeout_minutes": 30})).await;
    let carol = member("carol").with_session("sid-1");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);

    sqlx::query("UPDATE session_activity SET last_seen_at = now() - interval '20 minutes'")
      .execute(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(
      app.get(&carol, "/me").await.status,
      StatusCode::OK,
      "within the limit"
    );

    sqlx::query("UPDATE session_activity SET last_seen_at = now() - interval '31 minutes'")
      .execute(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
      app.get(&carol, "/me").await.status,
      StatusCode::UNAUTHORIZED,
      "an expired session stays expired"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn zero_limits_and_unknown_formats_are_refused() {
    let app = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    use base64::Engine;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    for edit in [
      json!({"session": {"max_age_hours": 0}}),
      json!({"session": {"idle_timeout_minutes": 0}}),
      json!({"directory": {"display_name_format": "shouting", "shows_full_name": true, "shows_email": false}}),
    ] {
      let mut value = serde_json::to_value(PolicyDocument {
        version: 1,
        policy: WorkspacePolicy::default(),
      })
      .unwrap();
      for (key, section) in edit.as_object().unwrap() {
        value["policy"][key] = section.clone();
      }
      let reply = app
        .post(
          &alice,
          "/workspace/policy",
          json!({"expected_version": 0, "payload": b64(&serde_json::to_vec(&value).unwrap()), "signature": b64(b"s"), "signer_device_id": device}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{edit}: {}", reply.body);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn local_sessions_obey_the_idle_timeout_and_a_shorter_maximum_age() {
    use std::sync::Arc;
    let mailer = Arc::new(teamcraft_server::testing::FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| {
        settings.local_accounts.is_enabled = true;
        settings.local_accounts.maybe_session_secret =
          Some("0123456789abcdef0123456789abcdef".to_owned().into());
        settings.local_accounts.password_memory_kib = 64;
        settings.local_accounts.password_iterations = 1;
        settings.local_accounts.require_totp = false;
      },
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    publish_policy(&app, 1, |p| {
      p["session"] = json!({"max_age_hours": 1, "idle_timeout_minutes": 10})
    })
    .await;
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "g@partner.test", "display_name": "G"}),
      )
      .await;
    let mail = mailer.sent().pop().unwrap();
    let token = mail
      .text
      .split("#token=")
      .nth(1)
      .unwrap()
      .split_whitespace()
      .next()
      .unwrap()
      .to_owned();
    let post = |path: &'static str, body: Value| {
      let app = &app;
      async move {
        let response = app
          .http
          .post(format!("{}/api/v1{path}", app.base_url))
          .json(&body)
          .send()
          .await
          .unwrap();
        (
          response.status(),
          response.json::<Value>().await.unwrap_or(Value::Null),
        )
      }
    };
    let password = "correct horse battery";
    post(
      "/auth/local/activate",
      json!({"token": token, "password": password}),
    )
    .await;
    // The first sign-in confirms the secret; with `require_totp` off a confirmed account still
    // needs a code, so use the policy's own path: an unconfirmed account signs in with the password.
    let (status, session) = post(
      "/auth/local/login",
      json!({"login": "g@partner.test", "password": password}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let access = session["access_token"].as_str().unwrap().to_owned();
    let ttl_ms =
      session["expires_at"].as_i64().unwrap() - time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
    assert!(
      ttl_ms <= 3_600_000 + 5_000,
      "the policy shortens the 12 hours of the config: {ttl_ms}"
    );

    let get_me = || async {
      app
        .http
        .get(format!("{}/api/v1/me", app.base_url))
        .bearer_auth(&access)
        .send()
        .await
        .unwrap()
        .status()
    };
    assert_eq!(get_me().await, StatusCode::OK);
    sqlx::query("UPDATE session_activity SET last_seen_at = now() - interval '11 minutes'")
      .execute(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(get_me().await, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }
}

mod second_factor {
  use super::*;

  #[tokio::test]
  async fn identity_provider_sign_ins_need_a_second_factor_when_the_policy_says_so() {
    let app = app!();
    publish_policy(&app, 1, |p| p["session"] = json!({"requires_mfa": true})).await;
    let password_only = member("carol").with_session("sid-1");
    let reply = app.get(&password_only, "/me").await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::FORBIDDEN, "mfa_required")
    );

    let with_otp = member("dave").with_authentication_methods(&["pwd", "otp"]);
    assert_eq!(app.get(&with_otp, "/me").await.status, StatusCode::OK);
    let with_hardware_key = member("erin").with_authentication_methods(&["hwk"]);
    assert_eq!(app.get(&with_hardware_key, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn local_accounts_need_their_code_under_the_policy_even_if_the_config_does_not() {
    use std::sync::Arc;
    let mailer = Arc::new(teamcraft_server::testing::FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| {
        settings.local_accounts.is_enabled = true;
        settings.local_accounts.maybe_session_secret =
          Some("0123456789abcdef0123456789abcdef".to_owned().into());
        settings.local_accounts.password_memory_kib = 64;
        settings.local_accounts.password_iterations = 1;
        settings.local_accounts.require_totp = false;
      },
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "g@partner.test", "display_name": "G"}),
      )
      .await;
    let mail = mailer.sent().pop().unwrap();
    let token = mail
      .text
      .split("#token=")
      .nth(1)
      .unwrap()
      .split_whitespace()
      .next()
      .unwrap()
      .to_owned();
    let password = "correct horse battery";
    let post = |path: &'static str, body: Value| {
      let app = &app;
      async move {
        app
          .http
          .post(format!("{}/api/v1{path}", app.base_url))
          .json(&body)
          .send()
          .await
          .unwrap()
          .status()
      }
    };
    post(
      "/auth/local/activate",
      json!({"token": token, "password": password}),
    )
    .await;
    let login = json!({"login": "g@partner.test", "password": password});
    assert_eq!(
      post("/auth/local/login", login.clone()).await,
      StatusCode::OK,
      "no policy, no code needed"
    );

    publish_policy(&app, 1, |p| p["session"] = json!({"requires_mfa": true})).await;
    assert_eq!(post("/auth/local/login", login).await, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_requirement_can_be_lifted_again() {
    let app = app!();
    publish_policy(&app, 1, |p| p["session"] = json!({"requires_mfa": true})).await;
    let carol = member("carol");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::FORBIDDEN);
    publish_policy(&app, 2, |_| {}).await;
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }
}

mod directory {
  use super::*;

  async fn name_of(app: &TestApp, viewer: &TestUser, user: &str) -> Value {
    let reply = app.get(viewer, &format!("/users/{user}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body
  }

  #[tokio::test]
  async fn display_names_follow_the_format() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let dave_id = user_id(&app, &dave).await;
    let as_stored = |name: &'static str| {
      let app = &app;
      let id = dave_id.parse::<i64>().unwrap();
      async move {
        sqlx::query("UPDATE users SET display_name = $1 WHERE id = $2")
          .bind(name)
          .bind(id)
          .execute(app.sql_pool())
          .await
          .unwrap();
      }
    };
    // (policy, stored name, shown name)
    let cases = [
      (
        json!({"display_name_format": "as_provided", "shows_full_name": true, "shows_email": false}),
        "Dave Q. Developer",
        "Dave Q. Developer",
      ),
      (
        json!({"display_name_format": "last_first", "shows_full_name": true, "shows_email": false}),
        "Dave Q. Developer",
        "Developer, Dave Q.",
      ),
      (
        json!({"display_name_format": "last_first", "shows_full_name": true, "shows_email": false}),
        "Developer, Dave",
        "Developer, Dave",
      ),
      (
        json!({"display_name_format": "first_last", "shows_full_name": true, "shows_email": false}),
        "Developer, Dave",
        "Dave Developer",
      ),
      (
        json!({"display_name_format": "first_last", "shows_full_name": true, "shows_email": false}),
        "Mononym",
        "Mononym",
      ),
      (
        json!({"display_name_format": "username", "shows_full_name": true, "shows_email": false}),
        "Dave Q. Developer",
        "dave",
      ),
      (
        json!({"display_name_format": "as_provided", "shows_full_name": false, "shows_email": false}),
        "Dave Q. Developer",
        "dave",
      ),
    ];
    for (index, (directory, stored, expected)) in cases.into_iter().enumerate() {
      publish_policy(&app, u64::try_from(index).unwrap() + 1, |p| {
        p["directory"] = directory.clone()
      })
      .await;
      as_stored(stored).await;
      let reply = app.get(&carol, &format!("/users/{dave_id}")).await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      assert_eq!(reply.body["display_name"], expected, "{directory} / {stored}");
    }
    // The search shows the same names.
    let found = app.get(&carol, "/users?query=dave").await;
    assert_eq!(found.body["users"][0]["display_name"], "dave");
    app.stop().await;
  }

  #[tokio::test]
  async fn e_mail_addresses_are_shown_to_members_only_when_the_policy_allows() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let ext = guest("ext");
    let dave_id = user_id(&app, &dave).await;
    channel_with(&app, &[&carol, &dave]).await;

    assert!(name_of(&app, &carol, &dave_id).await.get("email").is_none());
    publish_policy(&app, 1, |p| p["directory"]["shows_email"] = json!(true)).await;
    assert_eq!(name_of(&app, &carol, &dave_id).await["email"], "dave@example.com");
    let found = app.get(&carol, "/users?query=dave").await;
    assert_eq!(found.body["users"][0]["email"], "dave@example.com");

    // Guests never see addresses, even where they may see the person.
    let alice = owner();
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    add_space_member(&app, &alice, &space, &ext_id, "guest").await;
    add_space_member(&app, &alice, &space, &dave_id, "member").await;
    let shared = create_channel(&app, &alice, &space, "shared", "public").await;
    add_channel_member(&app, &alice, &shared, &ext_id).await;
    add_channel_member(&app, &alice, &shared, &dave_id).await;
    assert!(name_of(&app, &ext, &dave_id).await.get("email").is_none());
    app.stop().await;
  }

  #[test]
  fn the_default_directory_policy_shows_names_and_no_addresses() {
    let policy = WorkspacePolicy::default();
    assert!(policy.directory.shows_full_name && !policy.directory.shows_email);
  }
}

mod display_name_override {
  use super::*;

  async fn put_name(
    app: &TestApp,
    user: &TestUser,
    maybe_name: Option<&str>,
  ) -> teamcraft_server::testing::Reply {
    let body = maybe_name.map_or_else(|| json!({}), |name| json!({"display_name": name}));
    app.put(user, "/me/profile", body).await
  }

  async fn shown_name(app: &TestApp, viewer: &TestUser, user: &str) -> String {
    let reply = app.get(viewer, &format!("/users/{user}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body["display_name"].as_str().unwrap().to_owned()
  }

  #[tokio::test]
  async fn a_name_of_ones_own_is_trimmed_shown_to_everybody_and_cleared_by_an_empty_value() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;

    let before = app.get(&carol, "/me/profile").await;
    assert_eq!(
      before.body["can_override_display_name"], true,
      "allowed by default"
    );
    assert!(before.body.get("display_name_override").is_none());

    let set = put_name(&app, &carol, Some("  Ada L.  ")).await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    assert_eq!(set.body["display_name_override"], "Ada L.");
    assert_eq!(shown_name(&app, &dave, &carol_id).await, "Ada L.");

    let cleared = put_name(&app, &carol, Some("   ")).await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert!(cleared.body.get("display_name_override").is_none());
    assert_eq!(
      shown_name(&app, &dave, &carol_id).await,
      "carol",
      "the provider's name is back"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_names_are_refused() {
    let app = app!();
    let carol = member("carol");
    for name in ["x".repeat(81), "line\nbreak".to_owned(), "bell\u{7}".to_owned()] {
      let reply = put_name(&app, &carol, Some(&name)).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{name:?}: {}", reply.body);
      assert_eq!(reply.body["error_code"], "invalid_request");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_policy_that_forbids_names_refuses_new_ones_but_not_clearing_them() {
    let app = app!();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    assert_eq!(put_name(&app, &carol, Some("Ada")).await.status, StatusCode::OK);

    publish_policy(&app, 1, |p| {
      p["directory"]["allow_display_name_override"] = json!(false)
    })
    .await;
    let profile = app.get(&carol, "/me/profile").await;
    assert_eq!(profile.body["can_override_display_name"], false);

    let refused = put_name(&app, &carol, Some("Grace")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.body["error_code"], "display_name_override_not_allowed");
    assert_eq!(shown_name(&app, &carol, &carol_id).await, "Ada");

    // Saving the rest of the profile with the name unchanged still works.
    let same = put_name(&app, &carol, Some("Ada")).await;
    assert_eq!(same.status, StatusCode::OK, "{}", same.body);
    // Clearing is always possible.
    assert_eq!(put_name(&app, &carol, None).await.status, StatusCode::OK);
    assert_eq!(shown_name(&app, &carol, &carol_id).await, "carol");
    app.stop().await;
  }

  #[tokio::test]
  async fn the_sign_in_sync_keeps_the_name_and_clearing_brings_back_the_newest_provider_name() {
    let app = app!();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    assert_eq!(put_name(&app, &carol, Some("Ada")).await.status, StatusCode::OK);

    // The provider renames the person: same subject, new name in the token.
    let mut renamed = carol.clone();
    renamed.username = "carol-new".to_owned();
    assert_eq!(
      shown_name(&app, &renamed, &carol_id).await,
      "Ada",
      "the override survives the sync"
    );
    let profile = app.get(&renamed, "/me/profile").await;
    assert_eq!(profile.body["display_name_override"], "Ada");

    assert_eq!(put_name(&app, &renamed, None).await.status, StatusCode::OK);
    assert_eq!(shown_name(&app, &renamed, &carol_id).await, "carol-new");
    app.stop().await;
  }
}
