//! A brand's own mail server (admin.md §6.2): the password is write-only, every kind of mail of the
//! brand goes through its server, a brand without one uses the instance's, and a broken server only
//! fails its own brand's mail, visibly.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_proto::SmtpSecurityMode;
use teamcraft_server::auth::BoxFuture;
use teamcraft_server::config::{BrandConfig, MailTexts, MeetingsConfig, UnknownHostPolicy};
use teamcraft_server::meetings::mailer::{MailError, Mailer, OutgoingMail};
use teamcraft_server::runtime::brand_mail::{BrandServerView, BrandTransport};
use teamcraft_server::runtime::mail_status::MailStatus;
use teamcraft_server::services::{brand_mail, email_digest, guests};
use teamcraft_server::state::Settings;
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const PASSWORD: &str = "brand-smtp-secret-pw";
const SECRET: &str = "0123456789abcdef0123456789abcdef";
const STARTS_AT: i64 = 1_893_488_400_000;

macro_rules! app {
  ($start:expr) => {
    match $start.await {
      Some(app) => app,
      None => return,
    }
  };
}

/// The host names the test brands use; the test client calls by address.
fn serve_brands(settings: &mut Settings) {
  settings.branding.served_hostnames = ["a", "b", "c", "file"]
    .map(|name| format!("chat.{name}.example.com"))
    .to_vec();
  settings.branding.unknown_host = UnknownHostPolicy::Default;
}

async fn start(customize: impl FnOnce(&mut Settings)) -> Option<(TestApp, Arc<FakeMailer>)> {
  let instance = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_mailer(
    |settings| {
      serve_brands(settings);
      customize(settings);
    },
    instance.clone(),
  )
  .await?;
  Some((app, instance))
}

/// A database brand with its home domain and a sender at that domain.
async fn make_brand(app: &TestApp, id: &str, domain: &str) {
  let made = app
    .post(
      &admin(),
      "/admin/brands",
      json!({"id": id, "settings": {
        "display_name": id,
        "hostnames": [format!("chat.{domain}")],
        "email_domains": [domain],
        "mail": {"sender_name": format!("{id} team"), "from_address": format!("chat@{domain}")},
      }}),
    )
    .await;
  assert_eq!(made.status, StatusCode::OK, "{}", made.body);
}

fn server(port: u16, password: Value) -> Value {
  json!({"host": "127.0.0.1", "port": port, "security": "none", "username": "chat", "password": password})
}

async fn put_server(app: &TestApp, id: &str, body: Value) -> teamcraft_server::testing::Reply {
  app
    .put(&admin(), &format!("/admin/brands/{id}/mail-server"), body)
    .await
}

async fn brand_json(app: &TestApp, id: &str) -> Value {
  let all = app.get(&admin(), "/admin/brands").await;
  all.body["brands"]
    .as_array()
    .unwrap()
    .iter()
    .find(|brand| brand["id"] == id)
    .cloned()
    .unwrap()
}

async fn sealed_of(app: &TestApp, id: &str) -> Option<Vec<u8>> {
  let row: (Option<Vec<u8>>,) =
    teamcraft_db::testing::sqlx::query_as("SELECT sealed_secret FROM brand_mail_servers WHERE brand_id = $1")
      .bind(id)
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
  row.0
}

async fn audit_of(app: &TestApp, action: &str) -> Value {
  app
    .get(&admin(), &format!("/admin/audit?action={action}"))
    .await
    .body["entries"]
    .clone()
}

/// The details of an audit entry (JSON as text).
fn detail(entry: &Value) -> Value {
  serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap()
}

/// Puts these mailers in force as the brands' own servers, in place of real SMTP.
fn install(app: &TestApp, mailers: Vec<(&str, Arc<dyn Mailer>)>) {
  let transports: HashMap<String, Arc<BrandTransport>> = mailers
    .into_iter()
    .map(|(id, mailer)| {
      let view = BrandServerView {
        host: "smtp.example.com".into(),
        port: 587,
        security: SmtpSecurityMode::StartTls,
        maybe_username: None,
        has_password: false,
      };
      let transport = BrandTransport::new(
        view,
        mailer,
        Arc::new(MailStatus::default()),
        Duration::from_secs(30),
      );
      (id.to_owned(), Arc::new(transport))
    })
    .collect();
  app.state.brand_mail.install(transports);
}

/// A small SMTP server on this machine that keeps what it is told.
#[derive(Clone, Default)]
struct Sink {
  received: Arc<Mutex<Vec<String>>>,
  auth: Arc<Mutex<Vec<String>>>,
}

impl Sink {
  async fn start() -> (Self, u16) {
    let sink = Self::default();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let serving = sink.clone();
    tokio::spawn(async move {
      while let Ok((stream, _)) = listener.accept().await {
        let session = serving.clone();
        tokio::spawn(async move { session.serve(stream).await });
      }
    });
    (sink, port)
  }

  async fn serve(&self, stream: tokio::net::TcpStream) -> Option<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    writer.write_all(b"220 sink ready\r\n").await.ok()?;
    let mut message: Option<String> = None;
    while let Some(line) = lines.next_line().await.ok()? {
      if let Some(body) = message.as_mut() {
        if line == "." {
          self.received.lock().unwrap().push(message.take()?);
          writer.write_all(b"250 stored\r\n").await.ok()?;
        } else {
          body.push_str(&line);
          body.push('\n');
        }
        continue;
      }
      let reply: &[u8] = match line.split(' ').next()?.to_ascii_uppercase().as_str() {
        "EHLO" => b"250-sink\r\n250 AUTH PLAIN\r\n",
        "AUTH" => {
          self.auth.lock().unwrap().push(line.clone());
          b"235 welcome\r\n"
        },
        "DATA" => {
          message = Some(String::new());
          b"354 go on\r\n"
        },
        "QUIT" => {
          writer.write_all(b"221 bye\r\n").await.ok()?;
          return Some(());
        },
        _ => b"250 ok\r\n",
      };
      writer.write_all(reply).await.ok()?;
    }
    Some(())
  }

  fn mails(&self) -> Vec<String> {
    self.received.lock().unwrap().clone()
  }

  /// The passwords clients logged in with (AUTH PLAIN: `\0user\0password` in base64).
  fn passwords(&self) -> Vec<String> {
    self
      .auth
      .lock()
      .unwrap()
      .iter()
      .map(|line| {
        let encoded = line.rsplit(' ').next().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        text.rsplit('\0').next().unwrap().to_owned()
      })
      .collect()
  }
}

mod the_password {
  use super::*;

  #[tokio::test]
  async fn is_sealed_never_returned_and_kept_when_an_update_leaves_it_out() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let bob = admin();

    let saved = put_server(
      &app,
      "brand-a",
      server(2525, json!({"action": "set", "value": PASSWORD})),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert!(!saved.body.to_string().contains(PASSWORD));
    let own = brand_json(&app, "brand-a").await["mail_server"].clone();
    assert_eq!(own["host"], "127.0.0.1");
    assert_eq!(own["has_password"], true);
    assert!(own.get("password").is_none());
    assert!(
      !app
        .get(&bob, "/admin/brands")
        .await
        .body
        .to_string()
        .contains(PASSWORD)
    );

    let row: (String, Vec<u8>) = teamcraft_db::testing::sqlx::query_as(
      "SELECT value::text, sealed_secret FROM brand_mail_servers WHERE brand_id = 'brand-a'",
    )
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert!(!row.0.contains(PASSWORD));
    assert!(
      !row
        .1
        .windows(PASSWORD.len())
        .any(|window| window == PASSWORD.as_bytes())
    );

    // Left out of the request, and said explicitly: the same sealed bytes.
    let left_out = put_server(
      &app,
      "brand-a",
      json!({"host": "127.0.0.1", "port": 2526, "security": "none", "username": "chat"}),
    )
    .await;
    assert_eq!(left_out.status, StatusCode::OK, "{}", left_out.body);
    assert_eq!(sealed_of(&app, "brand-a").await.unwrap(), row.1);
    let kept = put_server(&app, "brand-a", server(2527, json!({"action": "keep"}))).await;
    assert_eq!(kept.status, StatusCode::OK, "{}", kept.body);
    assert_eq!(sealed_of(&app, "brand-a").await.unwrap(), row.1);
    let own = brand_json(&app, "brand-a").await["mail_server"].clone();
    assert_eq!(
      (own["port"].clone(), own["has_password"].clone()),
      (json!(2527), json!(true))
    );

    let cleared = put_server(&app, "brand-a", server(2527, json!({"action": "clear"}))).await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(sealed_of(&app, "brand-a").await, None);
    assert_eq!(
      brand_json(&app, "brand-a").await["mail_server"]["has_password"],
      false
    );

    let audit = audit_of(&app, "brand_mail_server_updated").await;
    assert_eq!(audit.as_array().unwrap().len(), 4);
    assert!(!audit.to_string().contains(PASSWORD));
    let changes: Vec<String> = audit
      .as_array()
      .unwrap()
      .iter()
      .map(|entry| detail(entry)["password"].as_str().unwrap().to_owned())
      .collect();
    for change in ["changed", "kept", "cleared"] {
      assert!(changes.iter().any(|seen| seen == change), "{change}: {changes:?}");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn goes_to_the_server_after_an_update_that_kept_it() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let (sink, port) = Sink::start().await;
    let saved = put_server(
      &app,
      "brand-a",
      server(1, json!({"action": "set", "value": PASSWORD})),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    let moved = put_server(&app, "brand-a", server(port, json!({"action": "keep"}))).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);

    let test = app
      .post(
        &admin(),
        "/admin/brands/brand-a/mail-server/test",
        json!({"to_address": "someone@example.org"}),
      )
      .await;
    assert_eq!(test.body["is_ok"], true, "{}", test.body);
    assert_eq!(sink.passwords(), [PASSWORD]);
    app.stop().await;
  }

  #[tokio::test]
  async fn needs_the_settings_key() {
    let (mut app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    app.state.maybe_settings_key = None;
    app.reconfigure(|_| {});
    let refused = put_server(
      &app,
      "brand-a",
      server(25, json!({"action": "set", "value": "pw"})),
    )
    .await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    let without = put_server(&app, "brand-a", server(25, json!({"action": "clear"}))).await;
    assert_eq!(without.status, StatusCode::OK, "{}", without.body);
    assert_eq!(without.body["can_store_secrets"], false);
    app.stop().await;
  }
}

mod routing {
  use super::*;

  async fn invite(app: &TestApp, space: &str, email: &str) {
    let invited = app
      .post(
        &admin(),
        &format!("/spaces/{space}/invites"),
        json!({"email": email, "role": "member"}),
      )
      .await;
    assert!(invited.status.is_success(), "{}", invited.body);
  }

  #[tokio::test]
  async fn a_brand_uses_its_own_server_the_others_the_instance_and_a_broken_one_fails_alone() {
    let (app, instance) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    make_brand(&app, "brand-b", "b.example.com").await;
    make_brand(&app, "brand-c", "c.example.com").await;
    let (sink, port) = Sink::start().await;
    put_server(&app, "brand-a", server(port, json!({"action": "keep"}))).await;
    // Nothing listens on port 1.
    put_server(&app, "brand-c", server(1, json!({"action": "keep"}))).await;
    let space = create_space(&app, &admin(), "Project A", "invite_only").await;

    invite(&app, &space, "dana@a.example.com").await;
    invite(&app, &space, "erin@b.example.com").await;
    invite(&app, &space, "gus@c.example.com").await;

    let through_own = sink.mails();
    assert_eq!(through_own.len(), 1, "{through_own:?}");
    assert!(
      through_own[0].contains("chat@a.example.com"),
      "{}",
      through_own[0]
    );
    assert!(through_own[0].contains("dana@a.example.com"));

    // The brand without a server, and only that one, went through the instance's.
    let through_instance: Vec<String> = instance.sent().into_iter().map(|mail| mail.to_address).collect();
    assert_eq!(through_instance, ["erin@b.example.com"]);

    let brand_a = brand_json(&app, "brand-a").await["mail_server"]["status"].clone();
    assert!(brand_a.get("last_success_at").is_some(), "{brand_a}");
    assert!(brand_a.get("last_failure_at").is_none(), "{brand_a}");
    let brand_c = brand_json(&app, "brand-c").await["mail_server"]["status"].clone();
    assert!(
      brand_c.get("last_failure_at").is_some() && brand_c.get("last_error").is_some(),
      "{brand_c}"
    );
    assert!(brand_c.get("last_success_at").is_none(), "{brand_c}");
    assert!(brand_json(&app, "brand-b").await.get("mail_server").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_new_server_is_used_without_a_restart_and_removing_it_goes_back_to_the_instance() {
    let (app, instance) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let (first, first_port) = Sink::start().await;
    let (second, second_port) = Sink::start().await;
    let space = create_space(&app, &admin(), "Project A", "invite_only").await;

    put_server(&app, "brand-a", server(first_port, json!({"action": "keep"}))).await;
    invite(&app, &space, "one@a.example.com").await;
    assert_eq!((first.mails().len(), second.mails().len()), (1, 0));

    put_server(&app, "brand-a", server(second_port, json!({"action": "keep"}))).await;
    invite(&app, &space, "two@a.example.com").await;
    assert_eq!((first.mails().len(), second.mails().len()), (1, 1));

    // An empty host means the instance's server, like DELETE.
    let removed = put_server(
      &app,
      "brand-a",
      json!({"host": "  ", "port": 587, "security": "start_tls"}),
    )
    .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert!(brand_json(&app, "brand-a").await.get("mail_server").is_none());
    invite(&app, &space, "three@a.example.com").await;
    assert_eq!((first.mails().len(), second.mails().len()), (1, 1));
    assert_eq!(instance.sent().len(), 1);

    put_server(&app, "brand-a", server(first_port, json!({"action": "keep"}))).await;
    let deleted = app.delete(&admin(), "/admin/brands/brand-a/mail-server").await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert!(deleted.body["brands"].to_string().find("mail_server").is_none());
    assert_eq!(
      audit_of(&app, "brand_mail_server_removed")
        .await
        .as_array()
        .unwrap()
        .len(),
      2
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn deleting_the_brand_deletes_its_server() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    put_server(
      &app,
      "brand-a",
      server(25, json!({"action": "set", "value": PASSWORD})),
    )
    .await;
    assert!(app.state.brand_mail.transport("brand-a").is_some());
    let deleted = app.delete(&admin(), "/admin/brands/brand-a").await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert!(app.state.brand_mail.transport("brand-a").is_none());
    let count: (i64,) = teamcraft_db::testing::sqlx::query_as("SELECT count(*) FROM brand_mail_servers")
      .fetch_one(app.sql_pool())
      .await
      .unwrap();
    assert_eq!(count.0, 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_password_that_cannot_be_decrypted_fails_visibly_and_never_uses_the_instance() {
    let (app, instance) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let (sink, port) = Sink::start().await;
    put_server(
      &app,
      "brand-a",
      server(port, json!({"action": "set", "value": PASSWORD})),
    )
    .await;
    // What a restart with another settings key finds.
    teamcraft_db::testing::sqlx::query(
      "UPDATE brand_mail_servers SET sealed_secret = '\\x0102030405' WHERE brand_id = 'brand-a'",
    )
    .execute(app.sql_pool())
    .await
    .unwrap();
    brand_mail::load(&app.state).await.unwrap();

    let status = brand_json(&app, "brand-a").await["mail_server"]["status"].clone();
    assert!(
      status["last_error"].as_str().unwrap().contains("decrypted"),
      "{status}"
    );
    let test = app
      .post(
        &admin(),
        "/admin/brands/brand-a/mail-server/test",
        json!({"to_address": "someone@example.org"}),
      )
      .await;
    assert_eq!(test.status, StatusCode::OK);
    assert_eq!(test.body["is_ok"], false, "{}", test.body);
    let space = create_space(&app, &admin(), "Project A", "invite_only").await;
    invite(&app, &space, "dana@a.example.com").await;
    assert!(instance.sent().is_empty() && sink.mails().is_empty());
    app.stop().await;
  }
}

/// Every kind of mail takes the transport of its brand, none takes the instance's.
mod every_kind_of_mail {
  use super::*;

  fn meetings() -> MeetingsConfig {
    MeetingsConfig {
      public_base_url: "https://chat.example.com".into(),
      organizer_fallback_address: "meetings@example.com".into(),
      caldav: None,
      max_invitees: 10,
      guest_token_secret: SECRET.to_owned().into(),
      guest_grace_secs: 7200,
      max_shared_link_guests: 50,
      start_ring_lead_secs: 3600,
      maybe_inbox_token: None,
      smtp: None,
      mail_texts: MailTexts::default(),
    }
  }

  fn addresses(mailer: &FakeMailer) -> Vec<String> {
    mailer.sent().into_iter().map(|mail| mail.to_address).collect()
  }

  #[tokio::test]
  async fn invitations_and_their_resending() {
    let (app, instance) = app!(start(|settings| settings.limits.invite_resend_interval_secs = 0));
    make_brand(&app, "brand-a", "a.example.com").await;
    let own = Arc::new(FakeMailer::default());
    install(&app, vec![("brand-a", own.clone())]);
    let space = create_space(&app, &admin(), "Project A", "invite_only").await;
    let invited = app
      .post(
        &admin(),
        &format!("/spaces/{space}/invites"),
        json!({"email": "dana@a.example.com", "role": "member"}),
      )
      .await;
    assert!(invited.status.is_success(), "{}", invited.body);
    assert_eq!(addresses(&own), ["dana@a.example.com"]);
    let resent = app
      .post(
        &admin(),
        &format!("/spaces/{space}/invites/{}/resend", invited.id()),
        json!({}),
      )
      .await;
    assert!(resent.status.is_success(), "{}", resent.body);
    assert_eq!(own.sent().len(), 2);
    assert!(instance.sent().is_empty());
    app.stop().await;
  }

  #[tokio::test]
  async fn activation_mail() {
    let (app, instance) = app!(start(|settings| {
      settings.local_accounts.is_enabled = true;
      settings.local_accounts.maybe_session_secret = Some(SECRET.to_owned().into());
      settings.local_accounts.password_memory_kib = 64;
      settings.local_accounts.password_iterations = 1;
      settings.mail.public_base_url = "https://chat.example.com".into();
    }));
    make_brand(&app, "brand-a", "a.example.com").await;
    let own = Arc::new(FakeMailer::default());
    install(&app, vec![("brand-a", own.clone())]);
    let space = create_space(&app, &admin(), "Project A", "invite_only").await;
    let created = app
      .post(
        &admin(),
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "gus@a.example.com", "display_name": "Gus Guest"}),
      )
      .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(addresses(&own), ["gus@a.example.com"]);

    let user = created.body["user"]["id"].as_str().unwrap().to_owned();
    let reissued = app
      .post(
        &admin(),
        &format!("/admin/local-accounts/{user}/activation"),
        json!({}),
      )
      .await;
    assert!(reissued.status.is_success(), "{}", reissued.body);
    assert_eq!(own.sent().len(), 2);
    assert!(instance.sent().is_empty());
    app.stop().await;
  }

  #[tokio::test]
  async fn meeting_invitations_follow_the_organizers_brand() {
    let instance = Arc::new(FakeMailer::default());
    let app = app!(TestApp::start_with_meetings_and_settings(
      &meetings(),
      Arc::new(FakeCalendar::default()),
      instance.clone(),
      serve_brands,
    ));
    make_brand(&app, "brand-a", "a.example.com").await;
    let own = Arc::new(FakeMailer::default());
    install(&app, vec![("brand-a", own.clone())]);

    let schedule = async |organizer: &TestUser| {
      let scheduled = app
        .post(
          organizer,
          "/meetings",
          json!({"title": "Sync", "starts_at": STARTS_AT, "ends_at": STARTS_AT + 3_600_000,
                 "invitees": [{"kind": "external", "email": "gus@example.org"}]}),
        )
        .await;
      assert_eq!(scheduled.status, StatusCode::CREATED, "{}", scheduled.body);
    };
    schedule(&member("olga").with_email("olga@a.example.com", true)).await;
    assert_eq!(addresses(&own), ["gus@example.org"]);
    assert!(instance.sent().is_empty());

    schedule(&member("omar")).await;
    assert_eq!(own.sent().len(), 1);
    assert_eq!(addresses(&instance), ["gus@example.org"]);
    app.stop().await;
  }

  #[tokio::test]
  async fn guest_expiry_notices() {
    let (app, instance) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let own = Arc::new(FakeMailer::default());
    install(&app, vec![("brand-a", own.clone())]);
    let alice = owner();
    let ext = guest("ext").with_email("ext@a.example.com", true);
    let ext_id = user_id(&app, &ext).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let ends = time::OffsetDateTime::now_utc() + time::Duration::days(30);
    let added = app
      .post(
        &alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": ext_id, "role": "guest", "expires_at": ends.unix_timestamp() * 1000}),
      )
      .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);

    guests::send_notices_once(&app.state, ends - time::Duration::days(14))
      .await
      .unwrap();
    assert_eq!(addresses(&own), ["ext@a.example.com"]);
    assert_eq!(
      addresses(&instance),
      vec![alice.email.clone()],
      "the sponsor has no brand"
    );
    app.stop().await;
  }

  /// A person with an address at `domain` with a digest due and one mention waiting.
  async fn digest_due(app: &TestApp, name: &str, domain: &str) {
    let person = member(name).with_email(&format!("{name}@{domain}"), true);
    let channel = channel_with(app, &[&person]).await;
    let id = user_id(app, &person).await;
    let enabled = app
      .put(&person, "/me/email-digest", json!({"is_enabled": true}))
      .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
    let alice = owner();
    let device = register_device(app, &alice).await;
    let mut body = message_body(1, &device);
    body["mentioned_user_ids"] = json!([id]);
    let sent = app
      .post(&alice, &format!("/conversations/{channel}/messages"), body)
      .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);
  }

  fn with_digests(settings: &mut Settings) {
    settings.notifications.email_digest.is_enabled = true;
    settings.notifications.email_digest.grace_secs = 0;
    settings.mail.public_base_url = "https://chat.example.com".into();
  }

  fn after_everything() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc() + time::Duration::days(30)
  }

  #[tokio::test]
  async fn digests_and_a_hanging_server_does_not_hold_up_the_others() {
    /// Never answers.
    struct Hanging;
    impl Mailer for Hanging {
      fn send<'a>(&'a self, _mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(std::future::pending())
      }
    }

    let (app, instance) = app!(start(with_digests));
    make_brand(&app, "brand-a", "a.example.com").await;
    make_brand(&app, "brand-b", "b.example.com").await;
    make_brand(&app, "brand-c", "c.example.com").await;
    let own = Arc::new(FakeMailer::default());
    install(
      &app,
      vec![("brand-a", Arc::new(Hanging)), ("brand-b", own.clone())],
    );
    digest_due(&app, "ann", "a.example.com").await;
    digest_due(&app, "ben", "b.example.com").await;
    digest_due(&app, "cid", "c.example.com").await;

    let state = app.state.clone();
    let job = tokio::spawn(async move { email_digest::run_due(&state, after_everything()).await });
    tokio::time::timeout(Duration::from_secs(10), async {
      while own.sent().is_empty() || instance.sent().is_empty() {
        tokio::time::sleep(Duration::from_millis(20)).await;
      }
    })
    .await
    .expect("brand B and the instance send while brand A's server hangs");
    assert_eq!(addresses(&own), ["ben@b.example.com"]);
    assert_eq!(addresses(&instance), ["cid@c.example.com"]);
    assert!(!job.is_finished(), "brand A's queue is still waiting");
    job.abort();
    app.stop().await;
  }
}

mod who_may {
  use super::*;

  #[tokio::test]
  async fn read_and_change_it_only_instance_admins_do() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    let body = json!({"host": "smtp.a.example.com", "port": 587, "security": "start_tls"});
    let calls: Vec<(Method, &str, Option<Value>)> = vec![
      (Method::GET, "/admin/brands", None),
      (
        Method::PUT,
        "/admin/brands/brand-a/mail-server",
        Some(body.clone()),
      ),
      (Method::DELETE, "/admin/brands/brand-a/mail-server", None),
      (
        Method::POST,
        "/admin/brands/brand-a/mail-server/test",
        Some(json!({})),
      ),
    ];
    for user in [member("carol"), guest("gina")] {
      for (method, path, body) in &calls {
        let reply = app.call(method.clone(), Some(&user), path, body.clone()).await;
        assert_eq!(
          reply.status,
          StatusCode::FORBIDDEN,
          "{} {method} {path}",
          user.username
        );
      }
    }
    assert_eq!(
      app.call(Method::GET, None, "/admin/brands", None).await.status,
      StatusCode::UNAUTHORIZED
    );
    assert!(app.state.brand_mail.transport("brand-a").is_none());
    for user in [owner(), admin()] {
      let saved = app
        .put(&user, "/admin/brands/brand-a/mail-server", body.clone())
        .await;
      assert_eq!(saved.status, StatusCode::OK, "{}: {}", user.username, saved.body);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn the_public_brand_never_shows_it() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    put_server(
      &app,
      "brand-a",
      server(2525, json!({"action": "set", "value": PASSWORD})),
    )
    .await;
    let public = app
      .http
      .get(format!("{}/api/v1/brand", app.base_url))
      .send()
      .await
      .unwrap()
      .text()
      .await
      .unwrap();
    assert!(
      !public.contains("mail_server") && !public.contains("127.0.0.1"),
      "{public}"
    );
    app.stop().await;
  }
}

mod refused {
  use super::*;

  #[tokio::test]
  async fn the_instance_guard_applies() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    for broken in [
      json!({"host": "smtp.example.com", "port": 25, "security": "none"}),
      json!({"host": "smtp example.com", "port": 587, "security": "start_tls"}),
      json!({"host": "smtp.example.com", "port": 0, "security": "start_tls"}),
      json!({"host": "smtp.example.com", "port": 587, "security": "ssl3"}),
      json!({"host": "smtp.example.com", "port": 587, "security": "tls", "username": "a\r\nb"}),
    ] {
      let reply = put_server(&app, "brand-a", broken.clone()).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{broken}: {}", reply.body);
    }
    assert!(app.state.brand_mail.transport("brand-a").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn read_only_and_unknown_brands_and_a_brand_without_a_sender() {
    let (app, _) = app!(start(|settings| {
      settings.brands = vec![BrandConfig {
        id: "file".into(),
        is_default: true,
        hostnames: vec!["chat.file.example.com".into()],
        display_name: "From the file".into(),
        ..BrandConfig::default()
      }];
    }));
    let body = server(2525, json!({"action": "keep"}));
    assert_eq!(
      put_server(&app, "file", body.clone()).await.status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      put_server(&app, "nobody", body.clone()).await.status,
      StatusCode::NOT_FOUND
    );

    let made = app
      .post(
        &admin(),
        "/admin/brands",
        json!({"id": "plain", "settings": {"display_name": "No sender", "hostnames": ["chat.b.example.com"]}}),
      )
      .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    assert_eq!(
      put_server(&app, "plain", body.clone()).await.status,
      StatusCode::BAD_REQUEST,
      "its server would have no sender to send as"
    );

    make_brand(&app, "brand-a", "a.example.com").await;
    let test = app
      .post(&admin(), "/admin/brands/brand-a/mail-server/test", json!({}))
      .await;
    assert_eq!(test.status, StatusCode::CONFLICT, "no server of its own yet");
    assert_eq!(put_server(&app, "brand-a", body).await.status, StatusCode::OK);
    let without_sender = app
      .put(
        &admin(),
        "/admin/brands/brand-a",
        json!({"settings": {"display_name": "brand-a", "hostnames": ["chat.a.example.com"]}}),
      )
      .await;
    assert_eq!(
      without_sender.status,
      StatusCode::BAD_REQUEST,
      "{}",
      without_sender.body
    );
    assert_eq!(
      audit_of(&app, "brand_mail_server_updated")
        .await
        .as_array()
        .unwrap()
        .len(),
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_test_mail_to_no_address_and_it_records_the_outcome() {
    let (app, _) = app!(start(|_| {}));
    make_brand(&app, "brand-a", "a.example.com").await;
    put_server(&app, "brand-a", server(1, json!({"action": "keep"}))).await;
    let bad = app
      .post(
        &admin(),
        "/admin/brands/brand-a/mail-server/test",
        json!({"to_address": "nobody"}),
      )
      .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    for _ in 0..2 {
      // Asks the server each time, also right after a failure.
      let test = app
        .post(
          &admin(),
          "/admin/brands/brand-a/mail-server/test",
          json!({"to_address": "someone@example.org"}),
        )
        .await;
      assert_eq!(test.body["is_ok"], false, "{}", test.body);
      assert!(
        !test.body["error"].as_str().unwrap().contains("not tried again"),
        "{}",
        test.body
      );
    }
    let audit = audit_of(&app, "brand_mail_test_sent").await;
    assert_eq!(audit.as_array().unwrap().len(), 2);
    assert_eq!(detail(&audit[0])["brand_id"], "brand-a");
    assert_eq!(detail(&audit[0])["is_ok"], false);
    app.stop().await;
  }
}
