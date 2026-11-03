//! DPoP over real requests (RFC 9449; e2ee.md §3.1.2): the challenge, what each refusal looks like
//! on the wire, the policy in the admin console, the WebSocket first frames and a restart. The rules
//! in detail are unit tests in `auth/dpop.rs` and in `teamcraft_crypto::dpop`.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use crate::common;

use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderMap, WWW_AUTHENTICATE};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_crypto::dpop::{DpopProofRequest, DpopSigningKey};
use teamcraft_proto::{
  DeviceActivity, GATEWAY_PATH, GatewayClientMessage, GatewayServerMessage, RENDEZVOUS_PATH,
  RendezvousClientMessage, RendezvousServerMessage,
};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

/// The origin the server is configured with. The tests call it by address: that a proof names this
/// origin and not the address it was reached on is what they rely on.
const ORIGIN: &str = "https://chat.example.com";
const ME: &str = "/api/v1/me";
const RENDEZVOUS_ID: &str = "0123456789abcdef0123456789abcdef";

macro_rules! app {
  ($start:expr) => {
    match $start.await {
      Some(app) => app,
      None => return,
    }
  };
}

async fn start() -> Option<TestApp> {
  TestApp::start_with(|settings| {
    settings.security.public_origins = vec![ORIGIN.to_owned()];
  })
  .await
}

mod challenge {
  use super::*;

  #[tokio::test]
  async fn a_bound_token_works_with_a_proof_after_the_nonce_challenge() {
    let app = app!(start());
    let device = Device::new(&app, &owner());

    let first = device.get(&app, ME, None).await;
    assert_eq!(first.status, StatusCode::UNAUTHORIZED);
    assert_eq!(first.error_code(), "use_dpop_nonce");
    assert!(
      first.challenge().starts_with(r#"DPoP error="use_dpop_nonce""#),
      "{}",
      first.challenge()
    );
    let nonce = first.nonce();

    let second = device.get(&app, ME, Some(&nonce)).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    // Every answer to a request with a proof names the nonce for the next one.
    assert!(!second.nonce().is_empty());
    app.stop().await;
  }

  /// Everybody meets the challenge after a restart; an office behind one address must not get
  /// blocked for it.
  #[tokio::test]
  async fn the_challenge_does_not_count_as_a_failed_sign_in_but_a_bad_proof_does() {
    let app = app!(TestApp::start_with(|settings| {
      settings.security.public_origins = vec![ORIGIN.to_owned()];
      settings.security.rate_limits.is_enabled = true;
      settings.security.rate_limits.auth_failures_per_minute = 2;
    }));
    let device = Device::new(&app, &owner());
    for _ in 0..5 {
      assert_eq!(device.get(&app, ME, None).await.status, StatusCode::UNAUTHORIZED);
    }
    let nonce = device.get(&app, ME, None).await.nonce();
    assert_eq!(device.get(&app, ME, Some(&nonce)).await.status, StatusCode::OK);

    let wrong = device.proof_for("POST", ME, Some(&nonce));
    for _ in 0..2 {
      assert_eq!(
        device.send(&app, ME, Some(&wrong)).await.status,
        StatusCode::UNAUTHORIZED
      );
    }
    assert_eq!(
      device.get(&app, ME, Some(&nonce)).await.status,
      StatusCode::TOO_MANY_REQUESTS
    );
    app.stop().await;
  }
}

mod refusals {
  use super::*;

  #[tokio::test]
  async fn replay_from_another_device_fails() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let thief = Device {
      key: DpopSigningKey::generate(),
      token: device.token.clone(),
    };
    let reply = thief.get(&app, ME, Some(&nonce)).await;
    reply.assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    assert!(reply.challenge().contains("another key"), "{}", reply.challenge());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_replayed_jti_fails() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let proof = device.proof_for("GET", ME, Some(&nonce));
    assert_eq!(device.send(&app, ME, Some(&proof)).await.status, StatusCode::OK);
    device
      .send(&app, ME, Some(&proof))
      .await
      .assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    app.stop().await;
  }

  #[tokio::test]
  async fn wrong_method_url_token_or_time_fail() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let now = now_unix();
    let other_token = "another.access.token";
    for (description, method, uri, iat, token) in [
      ("htm", "POST", format!("{ORIGIN}{ME}"), now, device.token.as_str()),
      (
        "htu origin",
        "GET",
        format!("https://chat.example.net{ME}"),
        now,
        device.token.as_str(),
      ),
      (
        "htu path",
        "GET",
        format!("{ORIGIN}/api/v1/spaces"),
        now,
        device.token.as_str(),
      ),
      ("ath", "GET", format!("{ORIGIN}{ME}"), now, other_token),
      (
        "expired iat",
        "GET",
        format!("{ORIGIN}{ME}"),
        now - 3600,
        device.token.as_str(),
      ),
      (
        "future iat",
        "GET",
        format!("{ORIGIN}{ME}"),
        now + 3600,
        device.token.as_str(),
      ),
    ] {
      let proof = device
        .key
        .sign_proof(&DpopProofRequest {
          method,
          target_uri: &uri,
          issued_at_unix: iat,
          maybe_access_token: Some(token),
          maybe_nonce: Some(&nonce),
        })
        .unwrap();
      let reply = device.send(&app, ME, Some(proof.as_str())).await;
      assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{description}");
      assert_eq!(reply.error_code(), "invalid_dpop_proof", "{description}");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_client_clock_inside_the_leeway_works() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    for skew in [-90, 45] {
      let proof = device
        .key
        .sign_proof(&DpopProofRequest {
          method: "GET",
          target_uri: &format!("{ORIGIN}{ME}"),
          issued_at_unix: now_unix() + skew,
          maybe_access_token: Some(&device.token),
          maybe_nonce: Some(&nonce),
        })
        .unwrap();
      assert_eq!(
        device.send(&app, ME, Some(proof.as_str())).await.status,
        StatusCode::OK,
        "{skew}"
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn malformed_and_oversized_proofs_fail() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    for proof in ["garbage".to_owned(), "a.b.c".to_owned(), "a".repeat(5000)] {
      device
        .send(&app, ME, Some(&proof))
        .await
        .assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn two_proof_headers_fail() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let reply = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .header(AUTHORIZATION, format!("DPoP {}", device.token))
      .header("DPoP", device.proof_for("GET", ME, Some(&nonce)))
      .header("DPoP", device.proof_for("GET", ME, Some(&nonce)))
      .send()
      .await
      .unwrap();
    Answer::read(reply)
      .await
      .assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    app.stop().await;
  }

  /// A bound token used as a bearer token is what a stolen one looks like, under `optional` too.
  #[tokio::test]
  async fn a_bound_token_without_a_proof_or_with_the_bearer_scheme_fails_under_optional() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;

    let reply = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .bearer_auth(&device.token)
      .send()
      .await
      .unwrap();
    Answer::read(reply)
      .await
      .assert_refused("dpop_required", "invalid_token");

    let reply = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .bearer_auth(&device.token)
      .header("DPoP", device.proof_for("GET", ME, Some(&nonce)))
      .send()
      .await
      .unwrap();
    Answer::read(reply)
      .await
      .assert_refused("dpop_required", "invalid_token");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_token_with_a_cnf_that_cannot_be_checked_is_invalid() {
    let app = app!(start());
    let user = owner();
    let now = i64::try_from(common::now_secs()).unwrap();
    for cnf in [json!({"x5t#S256": "abc"}), json!({"jkt": "short"}), json!("text")] {
      let token = app.token_with(&user, now, &json!({"cnf": cnf}));
      let reply = app
        .http
        .get(format!("{}{ME}", app.base_url))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
      assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
    }
    app.stop().await;
  }
}

mod behind_a_proxy {
  use super::*;

  /// Whoever sends the request controls `Host` and the forwarded headers, so they prove nothing: a
  /// proof made for a look-alike stays refused whatever they claim.
  #[tokio::test]
  async fn forwarded_headers_do_not_make_a_look_alike_the_public_origin() {
    let app = app!(TestApp::start_with(|settings| {
      settings.security.public_origins = vec![ORIGIN.to_owned()];
      settings.security.trust_forwarded_for = true;
      settings.branding.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    }));
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let lookalike = "chat.example.net";
    let proof = device
      .key
      .sign_proof(&DpopProofRequest {
        method: "GET",
        target_uri: &format!("https://{lookalike}{ME}"),
        issued_at_unix: now_unix(),
        maybe_access_token: Some(&device.token),
        maybe_nonce: Some(&nonce),
      })
      .unwrap();
    let reply = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .header(AUTHORIZATION, format!("DPoP {}", device.token))
      .header("DPoP", proof.as_str())
      .header("X-Forwarded-Host", lookalike)
      .header("X-Forwarded-Proto", "https")
      .header("Forwarded", format!("host={lookalike};proto=https"))
      .send()
      .await
      .unwrap();
    Answer::read(reply)
      .await
      .assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    app.stop().await;
  }

  /// The address the server sits on behind the proxy isn't a public origin either.
  #[tokio::test]
  async fn the_received_address_is_not_a_public_origin() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let proof = device
      .key
      .sign_proof(&DpopProofRequest {
        method: "GET",
        target_uri: &format!("{}{ME}", app.base_url),
        issued_at_unix: now_unix(),
        maybe_access_token: Some(&device.token),
        maybe_nonce: Some(&nonce),
      })
      .unwrap();
    device
      .send(&app, ME, Some(proof.as_str()))
      .await
      .assert_refused("invalid_dpop_proof", "invalid_dpop_proof");
    app.stop().await;
  }

  /// A brand's host name is a public origin without further configuration.
  #[tokio::test]
  async fn a_brand_host_name_is_a_public_origin() {
    let app = app!(TestApp::start_with(|settings| {
      settings.branding.served_hostnames = vec!["team.example.com".to_owned()];
      settings.branding.unknown_host = teamcraft_server::config::UnknownHostPolicy::Default;
    }));
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let proof = device
      .key
      .sign_proof(&DpopProofRequest {
        method: "GET",
        target_uri: &format!("https://team.example.com{ME}"),
        issued_at_unix: now_unix(),
        maybe_access_token: Some(&device.token),
        maybe_nonce: Some(&nonce),
      })
      .unwrap();
    assert_eq!(
      device.send(&app, ME, Some(proof.as_str())).await.status,
      StatusCode::OK
    );
    app.stop().await;
  }
}

mod restart {
  use super::*;

  /// The replay cache is lost with the process, and with it the nonces, so a proof from before the
  /// restart gets a challenge instead of being taken again. The client's next proof works.
  #[tokio::test]
  async fn a_proof_seen_before_a_restart_is_not_taken_again() {
    let mut app = app!(start());
    let device = Device::new(&app, &owner());
    let nonce = device.fresh_nonce(&app).await;
    let proof = device.proof_for("GET", ME, Some(&nonce));
    assert_eq!(device.send(&app, ME, Some(&proof)).await.status, StatusCode::OK);

    app.state.drain.begin();
    assert!(
      app
        .restart_after_drain_with_fresh_memory(Duration::from_secs(5))
        .await
    );

    let replayed = device.send(&app, ME, Some(&proof)).await;
    assert_eq!(replayed.status, StatusCode::UNAUTHORIZED);
    assert_eq!(replayed.error_code(), "use_dpop_nonce");
    let again = device.get(&app, ME, Some(&replayed.nonce())).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    app.stop().await;
  }
}

mod policy {
  use super::*;

  #[tokio::test]
  async fn optional_is_the_default_and_old_clients_keep_working() {
    let app = app!(start());
    let user = owner();
    let reply = app.get(&user, "/me").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let settings = app.get(&user, "/admin/settings").await;
    assert_eq!(settings.body["token_binding"]["dpop_policy"], json!("optional"));
    app.stop().await;
  }

  #[tokio::test]
  async fn required_can_only_be_set_from_a_session_that_proves_its_key() {
    let app = app!(start());
    let user = owner();
    let values = binding_values(&app, &user, "required").await;

    let unbound = app.put(&user, "/admin/settings", section(&values)).await;
    assert_eq!(unbound.status, StatusCode::BAD_REQUEST, "{}", unbound.body);
    let settings = app.get(&user, "/admin/settings").await;
    assert_eq!(settings.body["token_binding"]["dpop_policy"], json!("optional"));

    let device = Device::new(&app, &user);
    let nonce = device.fresh_nonce(&app).await;
    let reply = device.put_settings(&app, &section(&values), &nonce).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["token_binding"]["dpop_policy"], json!("required"));

    // Now old clients are refused, in a way that says what is missing.
    let old = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .bearer_auth(app.token(&user))
      .send()
      .await
      .unwrap();
    Answer::read(old)
      .await
      .assert_refused("dpop_required", "invalid_token");

    let nonce = device.fresh_nonce(&app).await;
    assert_eq!(device.get(&app, ME, Some(&nonce)).await.status, StatusCode::OK);
    app.stop().await;
  }

  #[tokio::test]
  async fn required_is_refused_while_the_server_does_not_know_its_origin() {
    let app = app!(TestApp::start());
    let user = owner();
    let values = binding_values(&app, &user, "required").await;
    let reply = app.put(&user, "/admin/settings", section(&values)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(reply.body.to_string().contains("public origin"), "{}", reply.body);
    app.stop().await;
  }

  /// `off` is the way back if proofs fail for everybody: nothing is checked and no nonce is sent.
  #[tokio::test]
  async fn off_takes_bound_tokens_as_bearer_tokens() {
    let app = app!(start());
    let user = owner();
    let values = binding_values(&app, &user, "off").await;
    let reply = app.put(&user, "/admin/settings", section(&values)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    let device = Device::new(&app, &user);
    let reply = app
      .http
      .get(format!("{}{ME}", app.base_url))
      .bearer_auth(&device.token)
      .header("DPoP", "ignored")
      .send()
      .await
      .unwrap();
    let answer = Answer::read(reply).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.nonce(), "");
    app.stop().await;
  }

  #[tokio::test]
  async fn out_of_range_values_are_refused_and_a_reset_goes_back_to_the_default() {
    let app = app!(start());
    let user = owner();
    for (name, value) in [
      ("dpop_policy", json!("sometimes")),
      ("proof_max_age_secs", json!(0)),
      ("proof_leeway_secs", json!(3601)),
      ("nonce_rotation_secs", json!(0)),
      ("replay_cache_max_entries", json!(10)),
      ("replay_cache_max_entries_per_user", json!(10)),
    ] {
      let mut values = binding_values(&app, &user, "optional").await;
      values[name] = value;
      let reply = app.put(&user, "/admin/settings", section(&values)).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{name}: {}", reply.body);
    }

    let values = binding_values(&app, &user, "off").await;
    assert_eq!(
      app.put(&user, "/admin/settings", section(&values)).await.status,
      StatusCode::OK
    );
    let reset = app.delete(&user, "/admin/settings/token_binding").await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    assert_eq!(reset.body["token_binding"]["dpop_policy"], json!("optional"));
    app.stop().await;
  }

  #[tokio::test]
  async fn members_cannot_change_it() {
    let app = app!(start());
    let values = binding_values(&app, &owner(), "off").await;
    let reply = app
      .put(&member("mallory"), "/admin/settings", section(&values))
      .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }
}

mod websockets {
  use super::*;

  #[tokio::test]
  async fn the_gateway_takes_a_proof_in_its_first_frame() {
    let app = app!(start());
    let user = owner();
    let device_id = register_device(&app, &user).await;
    let device = Device::new(&app, &user);

    // Without a proof.
    let refused = identify(&app, &device, &device_id, None).await;
    assert_error(&refused, "dpop_required");

    // Without a nonce: it arrives in the frame before the error.
    let proof = device.proof_for("GET", GATEWAY_PATH, None);
    let challenged = identify(&app, &device, &device_id, Some(proof)).await;
    let Some(GatewayServerMessage::DpopNonce { nonce }) = challenged.first() else {
      panic!("{challenged:?}");
    };
    assert_error(&challenged, "use_dpop_nonce");

    // With it.
    let proof = device.proof_for("GET", GATEWAY_PATH, Some(nonce));
    let ready = identify(&app, &device, &device_id, Some(proof.clone())).await;
    assert!(
      matches!(ready.first(), Some(GatewayServerMessage::Ready { .. })),
      "{ready:?}"
    );

    // The same proof again.
    let replayed = identify(&app, &device, &device_id, Some(proof)).await;
    assert_error(&replayed, "invalid_dpop_proof");

    // A proof for a REST request doesn't open the gateway.
    let nonce = device.fresh_nonce(&app).await;
    let elsewhere = device.proof_for("GET", ME, Some(&nonce));
    let refused = identify(&app, &device, &device_id, Some(elsewhere)).await;
    assert_error(&refused, "invalid_dpop_proof");
    app.stop().await;
  }

  #[tokio::test]
  async fn old_clients_connect_as_before_under_optional() {
    let app = app!(start());
    let user = owner();
    let device_id = register_device(&app, &user).await;
    let device = Device {
      key: DpopSigningKey::generate(),
      token: app.token(&user),
    };
    let ready = identify(&app, &device, &device_id, None).await;
    assert!(
      matches!(ready.first(), Some(GatewayServerMessage::Ready { .. })),
      "{ready:?}"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_rendezvous_relay_takes_a_proof_in_its_first_frame() {
    let app = app!(start());
    let user = owner();
    let device = Device::new(&app, &user);

    let refused = join_relay(&app, &device, None).await;
    assert!(
      matches!(&refused, Some(RendezvousServerMessage::Error { error }) if error.error_code.as_str() == "dpop_required"),
      "{refused:?}"
    );

    let nonce = device.fresh_nonce(&app).await;
    let proof = device.proof_for("GET", RENDEZVOUS_PATH, Some(&nonce));
    let joined = join_relay(&app, &device, Some(proof)).await;
    assert!(
      matches!(joined, Some(RendezvousServerMessage::Joined { .. })),
      "{joined:?}"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_refused_proof_on_the_gateway_counts_as_a_failed_sign_in_and_the_challenge_does_not() {
    let app = app!(TestApp::start_with(|settings| {
      settings.security.public_origins = vec![ORIGIN.to_owned()];
      settings.security.rate_limits.is_enabled = true;
      settings.security.rate_limits.auth_failures_per_minute = 2;
    }));
    let user = owner();
    let device_id = register_device(&app, &user).await;
    let device = Device::new(&app, &user);
    for _ in 0..4 {
      let proof = device.proof_for("GET", GATEWAY_PATH, None);
      let challenged = identify(&app, &device, &device_id, Some(proof)).await;
      assert_error(&challenged, "use_dpop_nonce");
    }
    let nonce = device.fresh_nonce(&app).await;
    for _ in 0..2 {
      let wrong = device.proof_for("POST", GATEWAY_PATH, Some(&nonce));
      assert_error(
        &identify(&app, &device, &device_id, Some(wrong)).await,
        "invalid_dpop_proof",
      );
    }
    assert_eq!(
      device.get(&app, ME, Some(&nonce)).await.status,
      StatusCode::TOO_MANY_REQUESTS
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_rendezvous_relay_names_the_nonce_before_the_challenge() {
    let app = app!(start());
    let device = Device::new(&app, &owner());
    let url = app.ws_url.replace(GATEWAY_PATH, RENDEZVOUS_PATH);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let join = RendezvousClientMessage::Join {
      maybe_dpop_proof: Some(device.proof_for("GET", RENDEZVOUS_PATH, None)),
      token: device.token.clone(),
      rendezvous_id: RENDEZVOUS_ID.to_owned(),
    };
    socket
      .send(Message::Text(serde_json::to_string(&join).unwrap().into()))
      .await
      .unwrap();
    let mut seen: Vec<RendezvousServerMessage> = Vec::new();
    while let Ok(Some(Ok(Message::Text(text)))) =
      tokio::time::timeout(Duration::from_secs(5), socket.next()).await
    {
      seen.push(serde_json::from_str(text.as_str()).unwrap());
    }
    let Some(RendezvousServerMessage::DpopNonce { nonce }) = seen.first() else {
      panic!("{seen:?}");
    };
    assert!(
      matches!(seen.get(1), Some(RendezvousServerMessage::Error { error }) if error.error_code.as_str() == "use_dpop_nonce"),
      "{seen:?}"
    );
    let proof = device.proof_for("GET", RENDEZVOUS_PATH, Some(nonce));
    assert!(matches!(
      join_relay(&app, &device, Some(proof)).await,
      Some(RendezvousServerMessage::Joined { .. })
    ));
    app.stop().await;
  }

  /// Everything the gateway answers to a first frame, until it closes the connection or is ready.
  async fn identify(
    app: &TestApp,
    device: &Device,
    device_id: &str,
    maybe_dpop_proof: Option<String>,
  ) -> Vec<GatewayServerMessage> {
    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    socket.next().await;
    let identify = GatewayClientMessage::Identify {
      maybe_dpop_proof,
      token: device.token.clone(),
      device_id: serde_json::from_value(json!(device_id)).unwrap(),
      activity: DeviceActivity::Active,
    };
    socket
      .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
      .await
      .unwrap();
    let mut seen = Vec::new();
    while let Ok(Some(Ok(Message::Text(text)))) =
      tokio::time::timeout(Duration::from_secs(5), socket.next()).await
    {
      let message: GatewayServerMessage = serde_json::from_str(text.as_str()).unwrap();
      let is_ready = matches!(message, GatewayServerMessage::Ready { .. });
      seen.push(message);
      if is_ready {
        break;
      }
    }
    seen
  }

  fn assert_error(messages: &[GatewayServerMessage], code: &str) {
    assert!(
      messages
        .iter()
        .any(|message| matches!(message, GatewayServerMessage::Error { error } if error.error_code.as_str() == code)),
      "{messages:?}"
    );
  }

  async fn join_relay(
    app: &TestApp,
    device: &Device,
    maybe_dpop_proof: Option<String>,
  ) -> Option<RendezvousServerMessage> {
    let url = app.ws_url.replace(GATEWAY_PATH, RENDEZVOUS_PATH);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let join = RendezvousClientMessage::Join {
      maybe_dpop_proof,
      token: device.token.clone(),
      rendezvous_id: RENDEZVOUS_ID.to_owned(),
    };
    socket
      .send(Message::Text(serde_json::to_string(&join).unwrap().into()))
      .await
      .unwrap();
    match tokio::time::timeout(Duration::from_secs(5), socket.next()).await {
      Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str(text.as_str()).ok(),
      _ => None,
    }
  }
}

/// A client with a proof key and a token bound to it.
struct Device {
  key: DpopSigningKey,
  token: String,
}

/// An answer with what a client looks at.
struct Answer {
  status: StatusCode,
  headers: HeaderMap,
  body: Value,
}

impl Device {
  fn new(app: &TestApp, user: &TestUser) -> Self {
    let key = DpopSigningKey::generate();
    let now = i64::try_from(common::now_secs()).unwrap();
    let token = app.token_with(
      user,
      now,
      &json!({"typ": "DPoP", "cnf": {"jkt": key.thumbprint().as_str()}}),
    );
    Self { key, token }
  }

  fn proof_for(&self, method: &str, path: &str, maybe_nonce: Option<&str>) -> String {
    self
      .key
      .sign_proof(&DpopProofRequest {
        method,
        target_uri: &format!("{ORIGIN}{path}"),
        issued_at_unix: now_unix(),
        maybe_access_token: Some(&self.token),
        maybe_nonce,
      })
      .unwrap()
      .as_str()
      .to_owned()
  }

  /// `GET` with a fresh proof.
  async fn get(&self, app: &TestApp, path: &str, maybe_nonce: Option<&str>) -> Answer {
    let proof = self.proof_for("GET", path, maybe_nonce);
    self.send(app, path, Some(&proof)).await
  }

  /// `GET` with exactly this proof.
  async fn send(&self, app: &TestApp, path: &str, maybe_proof: Option<&str>) -> Answer {
    let mut request = app
      .http
      .request(Method::GET, format!("{}{path}", app.base_url))
      .header(AUTHORIZATION, format!("DPoP {}", self.token));
    if let Some(proof) = maybe_proof {
      request = request.header("DPoP", proof);
    }
    Answer::read(request.send().await.unwrap()).await
  }

  async fn fresh_nonce(&self, app: &TestApp) -> String {
    let nonce = self.get(app, ME, None).await.nonce();
    assert!(!nonce.is_empty());
    nonce
  }

  async fn put_settings(&self, app: &TestApp, body: &Value, nonce: &str) -> Answer {
    let path = "/api/v1/admin/settings";
    let request = app
      .http
      .put(format!("{}{path}", app.base_url))
      .header(AUTHORIZATION, format!("DPoP {}", self.token))
      .header("DPoP", self.proof_for("PUT", path, Some(nonce)))
      .json(body);
    Answer::read(request.send().await.unwrap()).await
  }
}

impl Answer {
  async fn read(response: reqwest::Response) -> Self {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.bytes().await.unwrap();
    Self {
      status,
      headers,
      body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
  }

  fn error_code(&self) -> &str {
    self.body["error_code"].as_str().unwrap_or_default()
  }

  fn challenge(&self) -> &str {
    self
      .headers
      .get(WWW_AUTHENTICATE)
      .and_then(|value| value.to_str().ok())
      .unwrap_or_default()
  }

  fn nonce(&self) -> String {
    self
      .headers
      .get("DPoP-Nonce")
      .and_then(|value| value.to_str().ok())
      .unwrap_or_default()
      .to_owned()
  }

  #[track_caller]
  fn assert_refused(&self, code: &str, challenge: &str) {
    assert_eq!(self.status, StatusCode::UNAUTHORIZED, "{}", self.body);
    assert_eq!(self.error_code(), code, "{}", self.body);
    assert!(
      self
        .challenge()
        .starts_with(&format!(r#"DPoP error="{challenge}""#)),
      "{}",
      self.challenge()
    );
  }
}

async fn binding_values(app: &TestApp, user: &TestUser, policy: &str) -> Value {
  let mut values = app.get(user, "/admin/settings").await.body["token_binding"].clone();
  values["dpop_policy"] = json!(policy);
  values
}

fn section(values: &Value) -> Value {
  json!({"section": "token_binding", "values": values})
}

fn now_unix() -> i64 {
  i64::try_from(common::now_secs()).unwrap()
}
