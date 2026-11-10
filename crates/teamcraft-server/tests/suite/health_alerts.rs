//! The health checks and their alerts against a real server and Postgres (issue #213): who may read
//! them, who is told, and that an outage has to last before it is an alert. The evaluations are
//! driven by the test with a time of its own; nothing sleeps.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{DependencyName, GatewayEvent, HealthAlertSettings};
use teamcraft_server::testing::{FakeMailer, TestApp};
use time::OffsetDateTime;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

const QUIET: Duration = Duration::from_millis(300);

mod reading {
  use super::*;

  #[tokio::test]
  async fn only_admins_read_the_health_and_a_fresh_server_has_no_alert() {
    let app = app!();
    let carol = member("carol");
    let reply = app.get(&carol, "/admin/health").await;
    assert_eq!(
      (reply.status, reply.error_code()),
      (StatusCode::FORBIDDEN, "forbidden")
    );

    let reply = app.get(&admin(), "/admin/health").await;
    assert_eq!(reply.status, StatusCode::OK);
    let checks = reply.body["checks"].as_array().unwrap();
    assert_eq!(checks.len(), teamcraft_server::health::ALL_CHECKS.len());
    assert!(checks.iter().all(|check| check["status"] != "alert"));
    assert_eq!(status_of(&reply.body, "database"), "no_data", "not evaluated yet");
    assert_eq!(reply.body["alerts"], json!([]));
    assert_eq!(reply.body["is_alerting_enabled"], true);
    app.stop().await;
  }

  #[tokio::test]
  async fn what_is_not_set_up_says_so_and_nothing_in_the_answer_names_anything_else() {
    let app = app!();
    evaluate(&app, 0).await;
    let reply = app.get(&admin(), "/admin/health").await;
    assert_eq!(status_of(&reply.body, "database"), "ok");
    for check in [
      "object_storage",
      "push_gateway",
      "media_server",
      "media_udp_ipv4",
      "media_udp_ipv6",
      "call_quality",
      "backup",
      "certificate",
      "secret_expiry",
      "disk_space",
    ] {
      assert_eq!(status_of(&reply.body, check), "not_configured", "{check}");
    }
    // Names, states and numbers: no free text from anywhere.
    let text = reply.body.to_string();
    assert!(!text.contains("http"), "{text}");
    app.stop().await;
  }
}

mod alerting {
  use super::*;

  #[tokio::test]
  async fn an_outage_is_an_alert_only_after_the_hold_time_and_only_admins_hear_it() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    let (bob_device, carol_device) = (
      register_device(&app, &bob).await,
      register_device(&app, &carol).await,
    );
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
    ws::events(&mut bob_socket, QUIET).await;
    ws::events(&mut carol_socket, QUIET).await;

    let hold = HealthAlertSettings::default().hold_secs as i64;
    break_identity_provider(&app);
    // Failing, not yet an alert: a restart or a slow answer must raise nothing.
    for seconds in (0..hold).step_by(30) {
      evaluate(&app, seconds).await;
    }
    let reply = app.get(&bob, "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "failing");
    assert_eq!(reply.body["alerts"], json!([]));
    assert!(ws::events(&mut bob_socket, QUIET).await.is_empty());

    evaluate(&app, hold).await;
    let reply = app.get(&bob, "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "alert");
    assert_eq!(reply.body["alerts"][0]["check"], "identity_provider");
    assert!(reply.body["alerts"][0].get("resolved_at").is_none());
    assert!(
      ws::events(&mut bob_socket, QUIET)
        .await
        .iter()
        .any(|event| matches!(event, GatewayEvent::HealthAlertUpdate)),
      "the admin is told"
    );
    assert!(
      ws::events(&mut carol_socket, QUIET).await.is_empty(),
      "a member is told nothing"
    );

    // Back again: the alert ends only after the clear time.
    app
      .state
      .dependencies
      .guard(DependencyName::IdentityProvider)
      .record_success();
    let clear = HealthAlertSettings::default().clear_secs as i64;
    for seconds in (hold + 30..hold + 30 + clear).step_by(30) {
      evaluate(&app, seconds).await;
    }
    assert_eq!(
      status_of(&app.get(&bob, "/admin/health").await.body, "identity_provider"),
      "alert",
      "fine again, but not for the clear time yet"
    );
    evaluate(&app, hold + 30 + clear).await;
    let reply = app.get(&bob, "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "ok");
    assert!(reply.body["alerts"][0]["resolved_at"].is_number());
    assert!(
      ws::events(&mut bob_socket, QUIET)
        .await
        .iter()
        .any(|event| matches!(event, GatewayEvent::HealthAlertUpdate)),
      "the admin hears that it ended"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_dependency_that_comes_back_before_the_hold_raises_nothing() {
    let app = app!();
    let bob = admin();
    let hold = HealthAlertSettings::default().hold_secs as i64;
    break_identity_provider(&app);
    for seconds in (0..hold).step_by(30) {
      evaluate(&app, seconds).await;
    }
    app
      .state
      .dependencies
      .guard(DependencyName::IdentityProvider)
      .record_success();
    for seconds in (hold..hold * 2).step_by(30) {
      evaluate(&app, seconds).await;
    }
    let reply = app.get(&bob, "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "ok");
    assert_eq!(reply.body["alerts"], json!([]));
    app.stop().await;
  }

  #[tokio::test]
  async fn alerts_switched_off_in_the_settings_raise_nothing() {
    let app = app!();
    let bob = admin();
    let settings = HealthAlertSettings {
      is_enabled: false,
      ..HealthAlertSettings::default()
    };
    let reply = app.put(&bob, "/admin/settings", section(&settings)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    break_identity_provider(&app);
    for seconds in (0..settings.hold_secs as i64 * 2).step_by(30) {
      evaluate(&app, seconds).await;
    }
    let reply = app.get(&bob, "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "failing");
    assert_eq!(reply.body["is_alerting_enabled"], false);
    assert_eq!(reply.body["alerts"], json!([]));
    app.stop().await;
  }

  #[tokio::test]
  async fn the_settings_are_validated_and_only_admins_change_them() {
    let app = app!();
    let too_short = HealthAlertSettings {
      hold_secs: 5,
      ..HealthAlertSettings::default()
    };
    let body = section(&too_short);
    let denied = app.put(&member("carol"), "/admin/settings", body.clone()).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let refused = app.put(&admin(), "/admin/settings", body).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    app.stop().await;
  }
}

mod passing_on {
  use super::*;

  #[tokio::test]
  async fn an_alert_is_mailed_to_the_admins_and_queued_for_subscribed_webhooks() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(
      |settings| settings.webhooks.allow_private_targets = true,
      mailer.clone(),
    )
    .await
    else {
      return;
    };
    let bob = admin();
    // Nobody answers there: the event stays queued, which is what the test looks at.
    let subscribed = create_webhook(&app, &["health.alert_raised"]).await;
    let other = create_webhook(&app, &["channel.created"]).await;
    // A member's address is never used.
    app.get(&member("carol"), "/me").await;

    let hold = HealthAlertSettings::default().hold_secs as i64;
    break_identity_provider(&app);
    for seconds in (0..=hold).step_by(30) {
      evaluate(&app, seconds).await;
    }
    app.state.health.deliver_notices(&app.state, at_fixed(hold)).await;

    let mails = mailer.sent();
    assert_eq!(mails.len(), 1, "one mail per admin and alert");
    assert_eq!(mails[0].to_address, bob.email);
    assert_eq!(mails[0].subject, "TeamCraft health alert: identity_provider");
    assert_eq!(queued(&app, &subscribed).await, 1);
    assert_eq!(queued(&app, &other).await, 0);

    // Nothing is sent twice.
    app
      .state
      .health
      .deliver_notices(&app.state, at_fixed(hold + 3600))
      .await;
    assert_eq!(mailer.sent().len(), 1);
    assert_eq!(queued(&app, &subscribed).await, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn mail_switched_off_in_the_settings_sends_none() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(|_| {}, mailer.clone()).await else {
      return;
    };
    let settings = HealthAlertSettings {
      is_email_enabled: false,
      ..HealthAlertSettings::default()
    };
    let reply = app.put(&admin(), "/admin/settings", section(&settings)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    break_identity_provider(&app);
    for seconds in (0..=settings.hold_secs as i64).step_by(30) {
      evaluate(&app, seconds).await;
    }
    app
      .state
      .health
      .deliver_notices(&app.state, at_fixed(settings.hold_secs as i64))
      .await;
    let reply = app.get(&admin(), "/admin/health").await;
    assert_eq!(status_of(&reply.body, "identity_provider"), "alert");
    assert!(mailer.sent().is_empty());
    app.stop().await;
  }
}

mod restarting {
  use teamcraft_proto::{HealthCheckId, HealthCheckStatus};
  use teamcraft_server::health::{HealthConfig, HealthMonitor, JobSchedules};

  use super::*;

  #[tokio::test]
  async fn an_open_alert_survives_a_restart_and_a_failure_that_lasts_is_not_raised_again() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(|_| {}, mailer.clone()).await else {
      return;
    };
    let bob = admin();
    app.get(&bob, "/admin/health").await;
    assert!(app.state.health.restore(&app.state).await);
    let hold = HealthAlertSettings::default().hold_secs as i64;
    break_identity_provider(&app);
    for seconds in (0..=hold).step_by(30) {
      evaluate(&app, seconds).await;
    }
    app.state.health.deliver_notices(&app.state, at_fixed(hold)).await;
    assert_eq!(mailer.sent().len(), 1);
    assert!(app.state.health.persist(&app.state, at_fixed(hold)).await);

    let restarted = restart(&app, hold + 30).await;
    let response = restarted.response(&HealthAlertSettings::default());
    assert_eq!(response.alerts.len(), 1, "the history is back");
    assert_eq!(response.alerts[0].check, HealthCheckId::IdentityProvider);
    assert!(response.alerts[0].maybe_resolved_at.is_none());
    assert_eq!(
      status(&response, HealthCheckId::IdentityProvider),
      HealthCheckStatus::Alert
    );

    // Still failing for longer than the hold after the restart: the same alert, nothing new.
    for seconds in (hold + 30..=hold * 3).step_by(30) {
      restarted.evaluate(&app.state, at_fixed(seconds)).await;
    }
    restarted.deliver_notices(&app.state, at_fixed(hold * 3)).await;
    assert_eq!(
      restarted.response(&HealthAlertSettings::default()).alerts.len(),
      1
    );
    assert_eq!(mailer.sent().len(), 1, "nobody is told twice");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_notice_waiting_at_a_restart_goes_out_after_it_and_a_delivered_one_does_not() {
    let mailer = Arc::new(FakeMailer::default());
    let Some(app) = TestApp::start_with_mailer(|_| {}, mailer.clone()).await else {
      return;
    };
    let bob = admin();
    app.get(&bob, "/admin/health").await;
    assert!(app.state.health.restore(&app.state).await);
    let settings = HealthAlertSettings::default();
    let (hold, clear) = (settings.hold_secs as i64, settings.clear_secs as i64);
    break_identity_provider(&app);
    for seconds in (0..=hold).step_by(30) {
      evaluate(&app, seconds).await;
    }
    // The raise is delivered and written; then the alert ends, and the process stops before that
    // notice went out.
    app.state.health.deliver_notices(&app.state, at_fixed(hold)).await;
    assert!(app.state.health.persist(&app.state, at_fixed(hold)).await);
    app
      .state
      .dependencies
      .guard(DependencyName::IdentityProvider)
      .record_success();
    let resolved_at = hold + 30 + clear;
    for seconds in (hold + 30..=resolved_at).step_by(30) {
      evaluate(&app, seconds).await;
    }
    assert!(app.state.health.persist(&app.state, at_fixed(resolved_at)).await);
    assert_eq!(mailer.sent().len(), 1);

    let restarted = restart(&app, resolved_at + 30).await;
    restarted
      .deliver_notices(&app.state, at_fixed(resolved_at + 30))
      .await;
    let subjects: Vec<String> = mailer.sent().into_iter().map(|mail| mail.subject).collect();
    assert_eq!(
      subjects,
      [
        "TeamCraft health alert: identity_provider",
        "TeamCraft health alert resolved: identity_provider",
      ]
    );
    let response = restarted.response(&settings);
    assert!(response.alerts[0].maybe_resolved_at.is_some());
    assert_eq!(
      status(&response, HealthCheckId::IdentityProvider),
      HealthCheckStatus::NoData
    );

    // Written as delivered: a second restart sends nothing.
    assert!(restarted.persist(&app.state, at_fixed(resolved_at + 30)).await);
    let again = restart(&app, resolved_at + 60).await;
    again
      .deliver_notices(&app.state, at_fixed(resolved_at + 60))
      .await;
    assert_eq!(mailer.sent().len(), 2);
    app.stop().await;
  }

  /// A new monitor on the same database, as after a restart, with what was kept read back.
  async fn restart(app: &TestApp, seconds: i64) -> HealthMonitor {
    let monitor = HealthMonitor::new(
      HealthConfig {
        disk_paths: Vec::new(),
        ..HealthConfig::default()
      },
      JobSchedules::default(),
      at_fixed(seconds),
    );
    assert!(monitor.restore(&app.state).await);
    monitor
  }

  fn status(response: &teamcraft_proto::AdminHealthResponse, check: HealthCheckId) -> HealthCheckStatus {
    response
      .checks
      .iter()
      .find(|entry| entry.check == check)
      .unwrap()
      .status
      .clone()
  }
}

// --- helpers -----------------------------------------------------------------------------------

/// A time well after the server's start, `seconds` further on.
fn at(seconds: i64) -> OffsetDateTime {
  OffsetDateTime::now_utc() + time::Duration::days(1) + time::Duration::seconds(seconds)
}

async fn evaluate(app: &TestApp, seconds: i64) {
  app.state.health.evaluate(&app.state, at_fixed(seconds)).await;
}

/// The same base for every call of one test, so the seconds add up.
fn at_fixed(seconds: i64) -> OffsetDateTime {
  static BASE: std::sync::OnceLock<OffsetDateTime> = std::sync::OnceLock::new();
  *BASE.get_or_init(|| at(0)) + time::Duration::seconds(seconds)
}

/// Fails the identity provider's breaker as three failed calls would.
fn break_identity_provider(app: &TestApp) {
  let guard = app.state.dependencies.guard(DependencyName::IdentityProvider);
  for _ in 0..guard_threshold(app) {
    guard.record_failure();
  }
}

fn guard_threshold(app: &TestApp) -> u32 {
  app.state.dependencies.options().failure_threshold
}

fn status_of(body: &Value, check: &str) -> String {
  body["checks"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["check"] == check)
    .unwrap_or_else(|| panic!("no check {check}"))["status"]
    .as_str()
    .unwrap()
    .to_owned()
}

fn section(settings: &HealthAlertSettings) -> Value {
  json!({"section": "health_alerts", "values": settings})
}

/// A webhook to an address nobody answers on; returns its ID.
async fn create_webhook(app: &TestApp, events: &[&str]) -> String {
  let reply = app
    .post(
      &admin(),
      "/admin/webhooks",
      json!({"url": "http://[::1]:9/hook", "description": "CI", "events": events, "is_enabled": true}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["webhook"]["id"].as_str().unwrap().to_owned()
}

async fn queued(app: &TestApp, webhook: &str) -> u64 {
  app
    .state
    .database
    .count_webhook_deliveries(webhook.parse().unwrap())
    .await
    .unwrap()
}
