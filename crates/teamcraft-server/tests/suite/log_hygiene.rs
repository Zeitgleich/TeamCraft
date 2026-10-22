//! Runtime check that credentials and ciphertext never reach the logs (security.md): the server
//! handles requests carrying real secrets with logging at TRACE level, and the captured output must
//! contain none of them. The static counterpart is `teamcraft-ops/tests/no_secrets_in_logs.rs`.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use reqwest::Method;
use serde_json::json;
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
  fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
    self.0.lock().unwrap().extend_from_slice(buffer);
    Ok(buffer.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

impl<'a> MakeWriter<'a> for Capture {
  type Writer = Capture;

  fn make_writer(&'a self) -> Self::Writer {
    self.clone()
  }
}

const PUSH_TOKEN: &str = "push-token-SECRET-0123456789";
const PUSH_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
const CIPHERTEXT: &str = "c2VjcmV0LWNpcGhlcnRleHQtMDEyMzQ1Njc4OQ==";

#[tokio::test]
async fn credentials_and_ciphertext_do_not_appear_in_trace_level_logs() {
  let capture = Capture::default();
  let subscriber = tracing_subscriber::fmt()
    .with_max_level(tracing::Level::TRACE)
    .with_ansi(false)
    .with_writer(capture.clone())
    .finish();
  tracing::subscriber::set_global_default(subscriber).unwrap();

  let config = MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 5,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: Some("inbox-token-SECRET".to_owned().into()),
    smtp: None,
    mail_texts: MailTexts::default(),
  };
  let mailer = Arc::new(FakeMailer::default());
  let Some(app) =
    TestApp::start_with_meetings(&config, Arc::new(FakeCalendar::default()), mailer.clone()).await
  else {
    return;
  };
  let (alice, admin_user) = (owner(), admin());
  let alice_token = app.token(&alice);
  let device = register_device(&app, &alice).await;
  let channel = channel_with(&app, &[]).await;

  // Secrets in headers, URLs and bodies.
  let bot = app
    .post(&admin_user, "/admin/bots", json!({"name": "bridge"}))
    .await;
  let bot_token = bot.body["token"].as_str().unwrap().to_owned();
  call_with_token(&app, &bot_token, Method::GET, "/me", None).await;
  call_with_token(&app, "tcb_wrong-token-SECRET", Method::GET, "/me", None).await;
  call_with_token(
    &app,
    "eyJhbGciOiJSUzI1NiJ9.e30.garbage-signature-SECRET",
    Method::GET,
    "/me",
    None,
  )
  .await;
  app
    .put(
      &alice,
      &format!("/devices/{device}/push"),
      json!({"provider": "fcm", "token": PUSH_TOKEN, "push_key": PUSH_KEY}),
    )
    .await;
  let mut message = message_body(1, &device);
  message["ciphertext"] = json!(CIPHERTEXT);
  app
    .post(&alice, &format!("/conversations/{channel}/messages"), message)
    .await;
  let meeting = app
    .post(
      &alice,
      "/meetings",
      json!({"title": "Sync", "starts_at": 1_893_488_400_000_i64, "ends_at": 1_893_492_000_000_i64,
             "invitees": [{"kind": "external", "email": "gus@example.org"}]}),
    )
    .await;
  assert_eq!(meeting.status, reqwest::StatusCode::CREATED, "{}", meeting.body);
  let mail = mailer.sent().into_iter().next().unwrap();
  let invite_token = mail
    .text
    .split("#invite=")
    .nth(1)
    .unwrap()
    .split_whitespace()
    .next()
    .unwrap()
    .to_owned();
  let joined = app
    .http
    .post(format!("{}/api/v1/meetings/join", app.base_url))
    .json(&json!({"invite_token": invite_token, "display_name": "Gus"}))
    .send()
    .await
    .unwrap()
    .json::<serde_json::Value>()
    .await
    .unwrap();
  let guest_token = joined["guest_token"].as_str().unwrap().to_owned();
  call_with_token(&app, &guest_token, Method::GET, "/me", None).await;
  app
    .http
    .post(format!("{}/api/internal/v1/meetings/replies", app.base_url))
    .bearer_auth("inbox-token-SECRET")
    .header("content-type", "text/calendar")
    .header("x-envelope-sender", "someone@example.org")
    .body("garbage")
    .send()
    .await
    .unwrap();
  app.stop().await;

  let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
  assert!(
    logs.lines().count() > 20,
    "the capture saw only {} lines; logging is not wired",
    logs.lines().count()
  );
  for (what, secret) in [
    ("access token", alice_token.as_str()),
    ("access token signature", alice_token.rsplit('.').next().unwrap()),
    ("bot token", bot_token.as_str()),
    ("rejected bot token", "wrong-token-SECRET"),
    ("rejected jwt", "garbage-signature-SECRET"),
    ("push token", PUSH_TOKEN),
    ("push key", PUSH_KEY),
    ("ciphertext", CIPHERTEXT),
    ("guest invite token", invite_token.as_str()),
    ("guest session token", guest_token.as_str()),
    ("inbox token", "inbox-token-SECRET"),
    ("guest token secret", "0123456789abcdef0123456789abcdef"),
  ] {
    assert!(!logs.contains(secret), "{what} appears in the logs");
  }
}
