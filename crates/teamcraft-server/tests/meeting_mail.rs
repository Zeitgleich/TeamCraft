//! The SMTP sender against a real mail catcher (Mailpit from `docker-compose.dev.yml`, SMTP on
//! 127.0.0.1:1025 and API on 127.0.0.1:8025). Skipped when Mailpit is not running.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use serde_json::Value;
use teamcraft_server::config::{SmtpConfig, SmtpSecurity};
use teamcraft_server::meetings::ical::Method;
use teamcraft_server::meetings::mailer::{Mailer, OutgoingMail, SmtpMailer};

const API: &str = "http://127.0.0.1:8025/api/v1";

#[tokio::test]
async fn an_invitation_reaches_the_mail_catcher_with_the_calendar_part_and_attachment() {
  let client = reqwest::Client::new();
  if client.get(format!("{API}/info")).send().await.is_err() {
    eprintln!("skipping: no Mailpit at {API}");
    return;
  }
  let mailer = SmtpMailer::new(&SmtpConfig {
    host: "127.0.0.1".into(),
    port: 1025,
    security: SmtpSecurity::None,
    username: None,
    password: None,
    from_address: "meetings@example.com".into(),
    from_name: Some("TeamCraft".into()),
    timeout_secs: 5,
  })
  .unwrap();
  let marker = format!("Invitation: Review {}", std::process::id());
  let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:1@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

  mailer
    .send(&OutgoingMail {
      to_address: "gus@example.org".into(),
      maybe_to_name: Some("Gus Guest".into()),
      subject: marker.clone(),
      text: "Join: https://chat.example.com/meeting/1".into(),
      method: Method::Request,
      ics: ics.into(),
      maybe_from_name: None,
      maybe_from_address: None,
    })
    .await
    .unwrap();

  let mut search_url = url::Url::parse(&format!("{API}/search")).unwrap();
  search_url
    .query_pairs_mut()
    .append_pair("query", &format!("subject:\"{marker}\""));
  let search: Value = client.get(search_url).send().await.unwrap().json().await.unwrap();
  let id = search["messages"][0]["ID"]
    .as_str()
    .expect("message arrived")
    .to_owned();
  let raw = client
    .get(format!("{API}/message/{id}/raw"))
    .send()
    .await
    .unwrap()
    .text()
    .await
    .unwrap();
  assert!(
    raw.contains("text/calendar; charset=utf-8; method=REQUEST"),
    "{raw}"
  );
  assert!(raw.contains("filename=\"invite.ics\""));
  assert!(
    raw.contains("From: TeamCraft <meetings@example.com>")
      || raw.contains("From: \"TeamCraft\" <meetings@example.com>")
  );
}
