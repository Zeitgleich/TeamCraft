//! Local guest accounts: activation, password plus TOTP sign-in, lockout and sessions.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_crypto::account::TotpSecret;
use teamcraft_server::testing::{FakeMailer, TestApp, TestUser};

const PASSWORD: &str = "correct horse battery";
const SECRET: &str = "0123456789abcdef0123456789abcdef-secret";

macro_rules! app {
  () => {
    match start(|_| {}).await {
      Some(started) => started,
      None => return,
    }
  };
}

async fn start(
  customize: impl FnOnce(&mut teamcraft_server::state::Settings),
) -> Option<(TestApp, Arc<FakeMailer>)> {
  let mailer = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_mailer(
    |settings| {
      settings.local_accounts.is_enabled = true;
      settings.local_accounts.maybe_session_secret = Some(SECRET.to_owned().into());
      // Cheap hashes keep the tests fast; the cost is configuration, not behaviour.
      settings.local_accounts.password_memory_kib = 64;
      settings.local_accounts.password_iterations = 1;
      settings.mail.public_base_url = "https://chat.example.com".into();
      customize(settings);
    },
    mailer.clone(),
  )
  .await?;
  Some((app, mailer))
}

/// Creates the guest account and returns (user id, activation token).
async fn create_guest(
  app: &TestApp,
  mailer: &FakeMailer,
  sponsor: &TestUser,
  space: &str,
) -> (String, String) {
  let reply = app
    .post(
      sponsor,
      &format!("/spaces/{space}/local-guests"),
      json!({"email": "Gus@Partner.test", "display_name": "Gus Guest"}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  assert_eq!(reply.body["email"], "gus@partner.test");
  let mail = mailer.sent().pop().expect("activation mail");
  assert_eq!(mail.to_address, "gus@partner.test");
  let token = mail
    .text
    .split("#token=")
    .nth(1)
    .and_then(|rest| rest.split_whitespace().next())
    .expect("token in the link")
    .to_owned();
  (reply.body["user"]["id"].as_str().unwrap().to_owned(), token)
}

async fn public_post(app: &TestApp, path: &str, body: Value) -> (StatusCode, Value) {
  let response = app
    .http
    .post(format!("{}/api/v1{path}", app.base_url))
    .json(&body)
    .send()
    .await
    .unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

async fn bearer(
  app: &TestApp,
  method: reqwest::Method,
  token: &str,
  path: &str,
  body: Option<Value>,
) -> (StatusCode, Value) {
  let mut request = app
    .http
    .request(method, format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(token);
  if let Some(body) = body {
    request = request.json(&body);
  }
  let response = request.send().await.unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

fn base32_decode(text: &str) -> Vec<u8> {
  let (mut buffer, mut bits, mut out) = (0u32, 0u32, Vec::new());
  for ch in text.bytes() {
    let value = match ch {
      b'A'..=b'Z' => ch - b'A',
      b'2'..=b'7' => ch - b'2' + 26,
      _ => panic!("not base32"),
    };
    buffer = (buffer << 5) | u32::from(value);
    bits += 5;
    if bits >= 8 {
      bits -= 8;
      out.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
    }
  }
  out
}

fn code_at(secret_base32: &str, offset_steps: i64) -> String {
  let secret = TotpSecret::from_bytes(&base32_decode(secret_base32));
  let now = time::OffsetDateTime::now_utc().unix_timestamp();
  let step = u64::try_from(now / 30 + offset_steps).unwrap();
  format!("{:06}", secret.code(step, 6))
}

mod sign_in {
  use super::*;

  #[tokio::test]
  async fn a_guest_activates_signs_in_with_a_code_and_lands_in_the_space() {
    let (app, mailer) = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "client", "public").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "gus@partner.test", "display_name": "Gus Guest", "channel_ids": [channel]}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
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
    assert!(mail.text.contains("https://chat.example.com/activate#token="));

    let (status, activated) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    let secret = activated["totp_secret"].as_str().unwrap().to_owned();
    assert!(
      activated["totp_uri"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/TeamCraft:gus%40partner.test")
    );

    // The activation link works once.
    let (status, _) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Without a code, with a wrong password and with a wrong code the answer is a plain 401.
    for body in [
      json!({"login": "gus@partner.test", "password": PASSWORD}),
      json!({"login": "gus@partner.test", "password": "wrong wrong wrong", "totp_code": code_at(&secret, 0)}),
      json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": "000000"}),
      json!({"login": "nobody@partner.test", "password": PASSWORD, "totp_code": "000000"}),
    ] {
      let (status, reply) = public_post(&app, "/auth/local/login", body.clone()).await;
      assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}: {reply}");
    }

    let (status, session) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "Gus@Partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 0)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let access = session["access_token"].as_str().unwrap().to_owned();

    let (status, me) = bearer(&app, reqwest::Method::GET, &access, "/me", None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["role"], "guest");
    let (_, spaces) = bearer(&app, reqwest::Method::GET, &access, "/spaces", None).await;
    assert_eq!(spaces["spaces"][0]["name"], "Client");
    assert_eq!(spaces["spaces"][0]["my_role"], "guest");
    let (_, conversations) = bearer(&app, reqwest::Method::GET, &access, "/conversations", None).await;
    assert_eq!(conversations["conversations"][0]["name"], "client");

    // A code is accepted once: the same one again (same time step) is a replay.
    let (status, _) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 0)}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "replayed code");

    // Logging out ends the session.
    let (status, _) = bearer(&app, reqwest::Method::POST, &access, "/auth/local/logout", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = bearer(&app, reqwest::Method::GET, &access, "/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }

  #[tokio::test]
  async fn repeated_failures_lock_the_account() {
    let Some((app, mailer)) = start(|settings| settings.local_accounts.max_failed_attempts = 3).await else {
      return;
    };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let (_, token) = create_guest(&app, &mailer, &alice, &space).await;
    let (_, activated) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    let secret = activated["totp_secret"].as_str().unwrap().to_owned();

    for _ in 0..3 {
      let (status, _) = public_post(
        &app,
        "/auth/local/login",
        json!({"login": "gus@partner.test", "password": "nope nope nope nope"}),
      )
      .await;
      assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Locked: even the right credentials are refused for now.
    let (status, _) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 0)}),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    app.stop().await;
  }

  #[tokio::test]
  async fn changing_the_password_ends_all_sessions() {
    let (app, mailer) = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let (_, token) = create_guest(&app, &mailer, &alice, &space).await;
    let (_, activated) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    let secret = activated["totp_secret"].as_str().unwrap().to_owned();
    let (_, session) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 0)}),
    )
    .await;
    let access = session["access_token"].as_str().unwrap().to_owned();

    let (status, _) = bearer(
      &app,
      reqwest::Method::POST,
      &access,
      "/auth/local/password",
      Some(json!({"current_password": "wrong wrong wrong", "new_password": "a brand new password"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = bearer(
      &app,
      reqwest::Method::POST,
      &access,
      "/auth/local/password",
      Some(json!({"current_password": PASSWORD, "new_password": "short"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = bearer(
      &app,
      reqwest::Method::POST,
      &access,
      "/auth/local/password",
      Some(json!({"current_password": PASSWORD, "new_password": "a brand new password"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
      bearer(&app, reqwest::Method::GET, &access, "/me", None).await.0,
      StatusCode::UNAUTHORIZED
    );
    let (status, _) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "gus@partner.test", "password": "a brand new password", "totp_code": code_at(&secret, 1)}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_admin_can_lock_an_account_out_with_a_new_activation_link() {
    let (app, mailer) = app!();
    let (alice, bob) = (owner(), admin());
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let (user, token) = create_guest(&app, &mailer, &alice, &space).await;
    let (_, activated) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    let secret = activated["totp_secret"].as_str().unwrap().to_owned();
    let login = json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 0)});
    let (status, session) = public_post(&app, "/auth/local/login", login.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let access = session["access_token"].as_str().unwrap().to_owned();

    let member = member("carol");
    assert_eq!(
      app
        .post(
          &member,
          &format!("/admin/local-accounts/{user}/activation"),
          json!({})
        )
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    let reply = app
      .post(
        &bob,
        &format!("/admin/local-accounts/{user}/activation"),
        json!({}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(mailer.sent().len(), 2);

    assert_eq!(
      bearer(&app, reqwest::Method::GET, &access, "/me", None).await.0,
      StatusCode::UNAUTHORIZED
    );
    let (status, _) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "gus@partner.test", "password": PASSWORD, "totp_code": code_at(&secret, 1)}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }
}

mod creation {
  use super::*;

  #[tokio::test]
  async fn the_feature_is_off_by_default_and_needs_a_mail_server() {
    let Some(app) = TestApp::start().await else { return };
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "g@partner.test", "display_name": "G"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = public_post(
      &app,
      "/auth/local/login",
      json!({"login": "g@partner.test", "password": "x"}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    app.stop().await;

    let Some(app) = TestApp::start_with(|settings| {
      settings.local_accounts.is_enabled = true;
      settings.local_accounts.maybe_session_secret = Some(SECRET.to_owned().into());
    })
    .await
    else {
      return;
    };
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "g@partner.test", "display_name": "G"}),
      )
      .await;
    assert_eq!(
      reply.status,
      StatusCode::SERVICE_UNAVAILABLE,
      "no mail server, no activation link"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_people_who_may_invite_guests_create_them_and_domains_are_checked() {
    let Some((app, _mailer)) =
      start(|settings| settings.guests.allowed_email_domains = vec!["partner.test".into()]).await
    else {
      return;
    };
    let alice = owner();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let path = format!("/spaces/{space}/local-guests");

    let denied = app
      .post(
        &carol,
        &path,
        json!({"email": "g@partner.test", "display_name": "G"}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);
    let other_domain = app
      .post(
        &alice,
        &path,
        json!({"email": "g@other.test", "display_name": "G"}),
      )
      .await;
    assert_eq!(other_domain.status, StatusCode::BAD_REQUEST);
    let nameless = app
      .post(
        &alice,
        &path,
        json!({"email": "g@partner.test", "display_name": "  "}),
      )
      .await;
    assert_eq!(nameless.status, StatusCode::BAD_REQUEST);
    assert_eq!(
      app
        .post(
          &alice,
          &path,
          json!({"email": "g@partner.test", "display_name": "G"})
        )
        .await
        .status,
      StatusCode::CREATED
    );
    let twice = app
      .post(
        &alice,
        &path,
        json!({"email": "G@partner.test", "display_name": "G"}),
      )
      .await;
    assert_eq!(twice.status, StatusCode::CONFLICT);
    app.stop().await;
  }

  #[tokio::test]
  async fn weak_passwords_and_unknown_tokens_are_refused() {
    let (app, mailer) = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let (_, token) = create_guest(&app, &mailer, &alice, &space).await;
    let (status, _) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": "short"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": "nope", "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The failed attempts did not spend the real token.
    let (status, _) = public_post(
      &app,
      "/auth/local/activate",
      json!({"token": token, "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.stop().await;
  }
}
