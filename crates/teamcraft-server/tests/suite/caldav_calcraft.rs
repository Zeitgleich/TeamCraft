//! The CalDAV store and the iCalendar writer against a real CalCraft (`docker-compose.dev.yml`,
//! account created by `dev/init-calcraft.sh`). Skipped when CalCraft is not running.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use reqwest::StatusCode;
use teamcraft_proto::RsvpStatus;
use teamcraft_server::config::CalDavConfig;
use teamcraft_server::meetings::caldav::{CalDavStore, CalendarStore};
use teamcraft_server::meetings::ical::{EventAttendee, EventData, EventPerson, EventStatus, render_event};
use time::macros::datetime;

const CALENDAR_URL: &str = "http://127.0.0.1:5232/dav/calendars/teamcraft/default/";
const USERNAME: &str = "teamcraft";
const PASSWORD: &str = "teamcraft-dev-only";

#[tokio::test]
async fn an_event_we_render_is_accepted_stored_updated_and_deleted_by_calcraft() {
  let client = reqwest::Client::new();
  let probe = client
    .get(CALENDAR_URL)
    .basic_auth(USERNAME, Some(PASSWORD))
    .send()
    .await;
  if probe.is_err() {
    eprintln!("skipping: no CalCraft at {CALENDAR_URL}");
    return;
  }
  let store = CalDavStore::new(&CalDavConfig {
    calendar_url: CALENDAR_URL.into(),
    username: USERNAME.into(),
    password: PASSWORD.to_owned().into(),
    request_timeout_secs: 10,
    maybe_free_busy_url_template: None,
  })
  .unwrap();
  let name = format!("test-{}", std::process::id());
  let event_url = format!("{CALENDAR_URL}{name}.ics");
  let fetch = || async {
    client
      .get(&event_url)
      .basic_auth(USERNAME, Some(PASSWORD))
      .send()
      .await
      .unwrap()
  };

  let mut ics = render_event(&event(&name, 0, "Sprint review"), None);
  store.put_event(&name, &ics).await.unwrap();
  let stored = fetch().await;
  assert_eq!(stored.status(), StatusCode::OK);
  let body = unfolded(&stored.text().await.unwrap());
  assert!(
    body.contains("SUMMARY:Sprint review") && body.contains("mailto:gus@example.org"),
    "{body}"
  );

  // Writing the same object again replaces it.
  ics = render_event(&event(&name, 1, "Sprint review (moved)"), None);
  store.put_event(&name, &ics).await.unwrap();
  assert!(
    fetch()
      .await
      .text()
      .await
      .unwrap()
      .contains("SUMMARY:Sprint review (moved)")
  );

  store.delete_event(&name).await.unwrap();
  assert_eq!(fetch().await.status(), StatusCode::NOT_FOUND);
  // Deleting what is gone is fine.
  store.delete_event(&name).await.unwrap();
}

/// Joins folded content lines (the server may return the object with its own line endings).
fn unfolded(ics: &str) -> String {
  ics.replace("\r\n ", "").replace("\n ", "")
}

fn event(uid: &str, sequence: i32, summary: &'static str) -> EventData<'static> {
  EventData {
    uid: Box::leak(format!("{uid}@chat.example.com").into_boxed_str()),
    sequence,
    stamp: datetime!(2026-11-01 08:00 UTC),
    summary,
    maybe_description: Some("Demo; planning, and more"),
    starts_at: datetime!(2030-01-01 09:00 UTC),
    ends_at: datetime!(2030-01-01 10:00 UTC),
    maybe_recurrence_rule: Some("FREQ=WEEKLY;INTERVAL=1;COUNT=3"),
    status: EventStatus::Confirmed,
    join_url: "https://chat.example.com/meeting/42",
    organizer: EventPerson {
      email: "olive@example.com".into(),
      maybe_name: Some("Olive Organizer".into()),
    },
    attendees: vec![EventAttendee {
      person: EventPerson {
        email: "gus@example.org".into(),
        maybe_name: Some("Gus Guest".into()),
      },
      rsvp: RsvpStatus::NeedsAction,
    }],
  }
}
