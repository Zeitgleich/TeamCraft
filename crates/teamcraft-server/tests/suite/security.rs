//! Request hardening: rate limits, body size limit and security headers of the HTTP API.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::json;
use teamcraft_server::state::Settings;
use teamcraft_server::testing::TestApp;

async fn start(edit: impl FnOnce(&mut Settings)) -> Option<TestApp> {
  TestApp::start_with(|settings| {
    settings.security.rate_limits.is_enabled = true;
    edit(settings);
  })
  .await
}

async fn raw(app: &TestApp, method: Method, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
  let mut request = app.http.request(method, format!("{}{path}", app.base_url));
  for (name, value) in headers {
    request = request.header(*name, *value);
  }
  request.send().await.unwrap()
}

mod rate_limits {
  use super::*;

  #[tokio::test]
  async fn one_address_cannot_exceed_its_request_budget() {
    let Some(app) = start(|s| s.security.rate_limits.ip_requests_per_minute = 5).await else {
      return;
    };
    for _ in 0..5 {
      assert_eq!(
        raw(&app, Method::GET, "/healthz", &[]).await.status(),
        StatusCode::OK
      );
    }

    let limited = raw(&app, Method::GET, "/healthz", &[]).await;

    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = limited.headers()["retry-after"]
      .to_str()
      .unwrap()
      .parse()
      .unwrap();
    assert!(retry >= 1);
    let body: serde_json::Value = limited.json().await.unwrap();
    assert_eq!(body["error_code"], "rate_limited");
    app.stop().await;
  }

  #[tokio::test]
  async fn the_forwarded_header_only_counts_behind_a_trusted_proxy() {
    let Some(trusting) = start(|s| {
      s.security.rate_limits.ip_requests_per_minute = 2;
      s.security.trust_forwarded_for = true;
    })
    .await
    else {
      return;
    };
    let get =
      |ip: &'static str| async { raw(&trusting, Method::GET, "/healthz", &[("x-forwarded-for", ip)]).await };
    assert_eq!(get("203.0.113.1").await.status(), StatusCode::OK);
    assert_eq!(get("203.0.113.1").await.status(), StatusCode::OK);
    assert_eq!(get("203.0.113.1").await.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
      get("203.0.113.2").await.status(),
      StatusCode::OK,
      "another client behind the proxy"
    );
    // A client-supplied first entry doesn't help: the proxy's last entry decides.
    let spoofed = |first: &'static str| async {
      raw(&trusting, Method::GET, "/healthz", &[("x-forwarded-for", first)]).await
    };
    assert_eq!(
      spoofed("1.1.1.1, 203.0.113.1").await.status(),
      StatusCode::TOO_MANY_REQUESTS
    );
    trusting.stop().await;

    let Some(direct) = start(|s| s.security.rate_limits.ip_requests_per_minute = 2).await else {
      return;
    };
    for ip in ["198.51.100.1", "198.51.100.2"] {
      assert_eq!(
        raw(&direct, Method::GET, "/healthz", &[("x-forwarded-for", ip)])
          .await
          .status(),
        StatusCode::OK
      );
    }
    let third = raw(
      &direct,
      Method::GET,
      "/healthz",
      &[("x-forwarded-for", "198.51.100.3")],
    )
    .await;
    assert_eq!(
      third.status(),
      StatusCode::TOO_MANY_REQUESTS,
      "forged headers don't buy new budgets"
    );
    direct.stop().await;
  }

  #[tokio::test]
  async fn writes_are_limited_per_user_and_reads_are_not() {
    let Some(app) = start(|s| s.security.rate_limits.user_writes_per_minute = 3).await else {
      return;
    };
    let (carol, dave) = (member("carol"), member("dave"));
    for n in 0..3 {
      let reply = app
        .post(
          &carol,
          "/spaces",
          json!({"name": format!("s{n}"), "join_policy": "invite_only"}),
        )
        .await;
      assert_ne!(reply.status, StatusCode::TOO_MANY_REQUESTS, "write {n}");
    }

    let limited = app
      .post(
        &carol,
        "/spaces",
        json!({"name": "s9", "join_policy": "invite_only"}),
      )
      .await;

    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.error_code(), "rate_limited");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_ne!(
      app
        .post(
          &dave,
          "/spaces",
          json!({"name": "d", "join_policy": "invite_only"})
        )
        .await
        .status,
      StatusCode::TOO_MANY_REQUESTS
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn repeated_failed_authentications_block_the_address_even_for_valid_tokens() {
    let Some(app) = start(|s| s.security.rate_limits.auth_failures_per_minute = 3).await else {
      return;
    };
    let carol = member("carol");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    for _ in 0..3 {
      let (status, _) = call_with_token(&app, "tcb_guess", Method::GET, "/me", None).await;
      assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let blocked = app.get(&carol, "/me").await;

    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    app.stop().await;
  }

  #[tokio::test]
  async fn endpoints_without_credentials_have_a_smaller_budget() {
    let Some(app) = start(|s| s.security.rate_limits.public_requests_per_minute = 2).await else {
      return;
    };
    let join = || async {
      app
        .http
        .post(format!("{}/api/v1/meetings/join", app.base_url))
        .json(&json!({"invite_token": "x", "display_name": "g"}))
        .send()
        .await
        .unwrap()
        .status()
    };
    assert_ne!(join().await, StatusCode::TOO_MANY_REQUESTS);
    assert_ne!(join().await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(join().await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
      app.get(&member("carol"), "/me").await.status,
      StatusCode::OK,
      "other endpoints are unaffected"
    );
    app.stop().await;
  }
}

mod sizes {
  use super::*;

  #[tokio::test]
  async fn oversized_bodies_get_413_with_the_error_body() {
    let Some(app) = TestApp::start_with(|s| s.security.max_request_body_bytes = 1024).await else {
      return;
    };
    let carol = member("carol");
    let channel = channel_with(&app, &[&carol]).await;
    let device = register_device(&app, &carol).await;
    let mut body = message_body(1, &device);
    body["ciphertext"] = json!("A".repeat(4096));

    let reply = app
      .post(&carol, &format!("/conversations/{channel}/messages"), body)
      .await;

    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", reply.body);
    assert_eq!(reply.error_code(), "payload_too_large");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    app.stop().await;
  }
}

mod headers {
  use super::*;

  #[tokio::test]
  async fn api_responses_are_not_cacheable_sniffable_or_frameable() {
    let Some(app) = TestApp::start().await else { return };
    let carol = member("carol");
    let authed = app
      .http
      .get(format!("{}/api/v1/me", app.base_url))
      .bearer_auth(app.token(&carol))
      .send()
      .await
      .unwrap();
    let error = raw(&app, Method::GET, "/api/v1/me", &[]).await;
    let health = raw(&app, Method::GET, "/healthz", &[]).await;

    for response in [&authed, &error, &health] {
      let headers = response.headers();
      assert_eq!(headers["x-content-type-options"], "nosniff");
      assert_eq!(headers["cache-control"], "no-store");
      assert_eq!(headers["referrer-policy"], "no-referrer");
      assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'"
      );
      assert!(
        headers.get("strict-transport-security").is_none(),
        "off unless configured"
      );
    }
    app.stop().await;

    let Some(tls) = TestApp::start_with(|s| s.security.hsts = true).await else {
      return;
    };
    let response = raw(&tls, Method::GET, "/healthz", &[]).await;
    assert!(
      response.headers()["strict-transport-security"]
        .to_str()
        .unwrap()
        .contains("max-age=31536000")
    );
    tls.stop().await;
  }
}
