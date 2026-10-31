//! Regression tests for the server findings of docs/security-review.md.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// D2: admins act on members, guests and bots; only owners act on admins and owners.
mod d2_admin_hierarchy {
  use super::*;

  #[tokio::test]
  async fn admins_cannot_lock_out_owners_or_other_admins() {
    let app = app!();
    let (alice, bob, carl) = (owner(), admin(), TestUser::new("carl", &["admin"]));
    let (carol, ext) = (member("carol"), guest("ext"));
    let (alice_id, bob_id, carol_id, ext_id) = (
      user_id(&app, &alice).await,
      user_id(&app, &bob).await,
      user_id(&app, &carol).await,
      user_id(&app, &ext).await,
    );
    let carl_id = user_id(&app, &carl).await;
    let device = register_device(&app, &alice).await;

    for target in [&alice_id, &carl_id] {
      let deactivate = app
        .post(&bob, &format!("/admin/users/{target}/deactivate"), json!({}))
        .await;
      assert_eq!(
        deactivate.status,
        StatusCode::FORBIDDEN,
        "{target}: {}",
        deactivate.body
      );
      let revoke = app
        .delete(&bob, &format!("/admin/users/{target}/devices/{device}"))
        .await;
      assert_eq!(revoke.status, StatusCode::FORBIDDEN, "{target}");
    }
    // Members and guests are fine.
    for target in [&carol_id, &ext_id] {
      let reply = app
        .post(&bob, &format!("/admin/users/{target}/deactivate"), json!({}))
        .await;
      assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
      assert_eq!(
        app
          .post(&bob, &format!("/admin/users/{target}/reactivate"), json!({}))
          .await
          .status,
        StatusCode::NO_CONTENT
      );
    }
    // Owners may act on admins; admins cannot reactivate what an owner deactivated.
    assert_eq!(
      app
        .post(&alice, &format!("/admin/users/{bob_id}/deactivate"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app
        .post(&carl, &format!("/admin/users/{bob_id}/reactivate"), json!({}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&alice, &format!("/admin/users/{bob_id}/reactivate"), json!({}))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }
}

/// D3, D12: only access tokens issued to the app's own client are bearer tokens.
mod d3_d12_token_kinds {
  use super::*;

  #[tokio::test]
  async fn id_tokens_and_tokens_of_other_clients_are_not_accepted() {
    let app = app!();
    let carol = member("carol");
    let now = i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap();
    let get = |token: String| {
      let app = &app;
      async move {
        app
          .http
          .get(format!("{}/api/v1/me", app.base_url))
          .bearer_auth(token)
          .send()
          .await
          .unwrap()
          .status()
      }
    };
    assert_eq!(get(app.token_with(&carol, now, &json!({}))).await, StatusCode::OK);
    assert_eq!(
      get(app.token_with(&carol, now, &json!({"typ": "Bearer"}))).await,
      StatusCode::OK
    );
    for typ in ["ID", "Logout", "Refresh"] {
      let token = app.token_with(&carol, now, &json!({"typ": typ}));
      assert_eq!(get(token).await, StatusCode::UNAUTHORIZED, "{typ}");
    }
    let other_client = app.token_with(
      &carol,
      now,
      &json!({"azp": "other-app", "aud": ["account", "teamcraft"]}),
    );
    assert_eq!(get(other_client).await, StatusCode::UNAUTHORIZED);
    app.stop().await;
  }
}

/// D9: a back-channel logout token works once.
mod d9_logout_replay {
  use super::*;

  #[tokio::test]
  async fn a_captured_logout_token_cannot_be_replayed() {
    let app = app!();
    let carol = member("carol");
    user_id(&app, &carol).await;
    let token = app.logout_token(Some("sub-carol"), None);
    let post = |token: String| {
      let app = &app;
      async move {
        app
          .http
          .post(format!("{}/api/v1/auth/backchannel-logout", app.base_url))
          .form(&[("logout_token", token)])
          .send()
          .await
          .unwrap()
          .status()
      }
    };
    assert_eq!(post(token.clone()).await, StatusCode::OK);
    assert_eq!(post(token).await, StatusCode::UNAUTHORIZED);
    // Logging in again afterwards works; a replay does not end that session.
    let later = app.token_issued_at(
      &carol,
      i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap() + 5,
    );
    let reply = app
      .http
      .get(format!("{}/api/v1/me", app.base_url))
      .bearer_auth(later)
      .send()
      .await
      .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    app.stop().await;
  }
}

/// D4, D5, D10: the gateway limits what one connection can do.
mod gateway_limits {
  use std::time::Duration;

  use futures_util::{SinkExt, StreamExt};
  use teamcraft_proto::GatewayClientMessage;
  use tokio_tungstenite::tungstenite::Message;

  use super::*;

  /// Waits until the server closes the connection (or a short time passes).
  async fn is_closed_soon(socket: &mut ws::Socket) -> bool {
    loop {
      match tokio::time::timeout(Duration::from_millis(800), socket.next()).await {
        Err(_) => return false,
        Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return true,
        Ok(Some(Ok(_))) => {},
      }
    }
  }

  async fn send_text(socket: &mut ws::Socket, text: String) {
    let _ = socket.send(Message::Text(text.into())).await;
  }

  #[tokio::test]
  async fn a_connection_that_floods_frames_is_closed() {
    let Some(app) = TestApp::start_with(|settings| {
      settings.gateway.client_frame_burst = 10;
      settings.gateway.max_client_frames_per_minute = 60;
    })
    .await
    else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;
    let mut calm = ws::connect(&app, &carol, &device).await;
    // A few frames are fine.
    for _ in 0..5 {
      send_text(
        &mut calm,
        serde_json::to_string(&GatewayClientMessage::Heartbeat).unwrap(),
      )
      .await;
    }
    assert!(!is_closed_soon(&mut calm).await);

    let dave = member("dave");
    let device = register_device(&app, &dave).await;
    let mut flooding = ws::connect(&app, &dave, &device).await;
    // Malformed frames count as well.
    for n in 0..40 {
      send_text(
        &mut flooding,
        if n % 2 == 0 {
          "{".into()
        } else {
          serde_json::to_string(&GatewayClientMessage::Heartbeat).unwrap()
        },
      )
      .await;
    }
    assert!(
      is_closed_soon(&mut flooding).await,
      "the flood should end the connection"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn typing_in_made_up_conversations_is_answered_with_errors_and_does_not_pile_up() {
    let Some(app) = TestApp::start_with(|settings| {
      settings.gateway.client_frame_burst = 1000;
      settings.gateway.max_client_frames_per_minute = 60_000;
    })
    .await
    else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;
    let mut socket = ws::connect(&app, &carol, &device).await;
    for n in 1..=300_u64 {
      send_text(
        &mut socket,
        json!({"type": "typing", "conversation_id": (n * 7919).to_string()}).to_string(),
      )
      .await;
    }
    // The connection stays healthy and answers a heartbeat after the burst.
    send_text(
      &mut socket,
      serde_json::to_string(&GatewayClientMessage::Heartbeat).unwrap(),
    )
    .await;
    let mut acked = false;
    while let Ok(Some(Ok(Message::Text(text)))) =
      tokio::time::timeout(Duration::from_secs(5), socket.next()).await
    {
      if text.as_str().contains("heartbeat_ack") {
        acked = true;
        break;
      }
    }
    assert!(acked);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_device_has_a_limited_number_of_sessions() {
    let Some(app) = TestApp::start_with(|settings| settings.gateway.max_sessions_per_device = 2).await else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;
    let mut first = ws::connect(&app, &carol, &device).await;
    let mut second = ws::connect(&app, &carol, &device).await;
    let mut third = ws::connect(&app, &carol, &device).await;
    assert!(is_closed_soon(&mut first).await, "the oldest session was dropped");
    assert!(!is_closed_soon(&mut second).await);
    assert!(!is_closed_soon(&mut third).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn failed_gateway_logins_count_against_the_address() {
    let Some(app) = TestApp::start_with(|settings| {
      settings.security.rate_limits.is_enabled = true;
      settings.security.rate_limits.auth_failures_per_minute = 3;
    })
    .await
    else {
      return;
    };
    let mut blocked = false;
    for _ in 0..8 {
      match tokio_tungstenite::connect_async(&app.ws_url).await {
        Ok((mut socket, _)) => {
          socket.next().await;
          send_text(
            &mut socket,
            json!({"type": "identify", "token": "not-a-token", "device_id": "1", "activity": "active"})
              .to_string(),
          )
          .await;
          // The error frame carries a public message only.
          if let Some(Ok(Message::Text(text))) = socket.next().await {
            assert!(
              !text.as_str().contains("signing keys") && !text.as_str().contains("database"),
              "{text}"
            );
          }
        },
        Err(_) => {
          blocked = true;
          break;
        },
      }
    }
    assert!(
      blocked,
      "guessing tokens over the gateway should get the address blocked"
    );
    app.stop().await;
  }
}

/// D7: per-user quotas.
mod d7_quotas {
  use std::sync::Arc;

  use teamcraft_server::config::{MailTexts, MeetingsConfig};
  use teamcraft_server::testing::{FakeCalendar, FakeMailer};

  use super::*;

  #[tokio::test]
  async fn a_user_has_a_limited_number_of_devices() {
    let Some(app) = TestApp::start_with(|settings| settings.limits.max_devices_per_user = 2).await else {
      return;
    };
    let carol = member("carol");
    let first = register_device(&app, &carol).await;
    register_device(&app, &carol).await;
    let third = app
      .post(
        &carol,
        "/devices",
        json!({"name": "tablet", "public_key": "cHVibGlj", "cross_signature": "c2ln"}),
      )
      .await;
    assert_eq!(third.status, StatusCode::CONFLICT, "{}", third.body);
    assert_eq!(
      app.delete(&carol, &format!("/devices/{first}")).await.status,
      StatusCode::NO_CONTENT
    );
    register_device(&app, &carol).await;
    // The limit is per user.
    register_device(&app, &member("dave")).await;
    app.stop().await;
  }

  #[tokio::test]
  async fn unfinished_uploads_and_stored_bytes_are_limited_per_user() {
    let Some(app) = TestApp::start_with_storage(|settings| {
      settings.limits.max_pending_attachments_per_user = 2;
      settings.limits.max_attachment_bytes_per_user = 150;
    })
    .await
    else {
      return;
    };
    let (carol, dave) = (member("carol"), member("dave"));
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let announce = |user: &TestUser, size: u64| {
      let (app, channel) = (&app, channel.clone());
      let user = user.clone();
      async move {
        app
          .post(
            &user,
            "/attachments",
            json!({"conversation_id": channel, "size_bytes": size}),
          )
          .await
      }
    };
    assert_eq!(announce(&carol, 60).await.status, StatusCode::CREATED);
    assert_eq!(announce(&carol, 60).await.status, StatusCode::CREATED);
    // Two unfinished uploads: no third, however small.
    assert_eq!(announce(&carol, 1).await.status, StatusCode::CONFLICT);
    // Others have their own quota.
    assert_eq!(announce(&dave, 100).await.status, StatusCode::CREATED);
    assert_eq!(
      announce(&dave, 51).await.status,
      StatusCode::PAYLOAD_TOO_LARGE,
      "150 bytes in total"
    );
    assert_eq!(announce(&dave, 50).await.status, StatusCode::CREATED);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_organizers_meetings_send_a_limited_number_of_mails_per_hour() {
    let mailer = Arc::new(FakeMailer::default());
    let config = MeetingsConfig {
      public_base_url: "https://chat.example.com".into(),
      organizer_fallback_address: "meetings@example.com".into(),
      caldav: None,
      max_invitees: 10,
      guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
      guest_grace_secs: 7200,
      max_shared_link_guests: 50,
      start_ring_lead_secs: 3600,
      maybe_inbox_token: None,
      smtp: None,
      mail_texts: MailTexts::default(),
    };
    let Some(app) = TestApp::start_with_meetings_and_settings(
      &config,
      Arc::new(FakeCalendar::default()),
      mailer.clone(),
      |settings| settings.limits.max_meeting_mails_per_user_per_hour = 3,
    )
    .await
    else {
      return;
    };
    let alice = owner();
    let schedule = |count: usize| {
      let (app, alice) = (&app, alice.clone());
      async move {
        let invitees: Vec<_> = (0..count)
          .map(|n| json!({"kind": "external", "email": format!("guest{n}@example.org")}))
          .collect();
        app
          .post(
            &alice,
            "/meetings",
            json!({"title": "Review", "starts_at": 1_893_488_400_000_i64, "ends_at": 1_893_492_000_000_i64, "invitees": invitees}),
          )
          .await
      }
    };
    let first = schedule(2).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(mailer.sent().len(), 2);
    assert!(
      first.body["undelivered_invitations"]
        .as_array()
        .is_none_or(Vec::is_empty)
    );

    // Two more do not fit in the budget of three: none go out, and the response says so.
    let second = schedule(2).await;
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(mailer.sent().len(), 2);
    assert_eq!(
      second.body["undelivered_invitations"].as_array().unwrap().len(),
      2
    );
    // One still fits; another organizer has a budget of their own.
    assert_eq!(schedule(1).await.status, StatusCode::CREATED);
    assert_eq!(mailer.sent().len(), 3);
    app.stop().await;
  }
}

/// D11: the workspace policy's `admin_self_join` is enforced by the server too.
mod d11_admin_self_join {
  use base64::Engine;
  use teamcraft_proto::{PolicyDocument, WorkspacePolicy};

  use super::*;

  async fn publish_self_join(app: &TestApp, version: u64, setting: &str) {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let alice = owner();
    let device = match app.get(&alice, "/devices").await.body["devices"]
      .as_array()
      .and_then(|d| d.first())
    {
      Some(device) => device["id"].as_str().unwrap().to_owned(),
      None => register_device(app, &alice).await,
    };
    let mut value = serde_json::to_value(PolicyDocument {
      version,
      policy: WorkspacePolicy::default(),
    })
    .unwrap();
    value["policy"]["admin_self_join"] = json!(setting);
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

  #[tokio::test]
  async fn admins_join_private_channels_on_their_own_only_if_the_policy_allows() {
    let app = app!();
    let (alice, bob, carol) = (owner(), admin(), member("carol"));
    let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_space_member(&app, &alice, &space, &bob_id, "member").await;
    let private = create_channel(&app, &alice, &space, "leads", "private").await;
    let public = create_channel(&app, &alice, &space, "general", "public").await;
    let join = |channel: &str| {
      let (app, bob, bob_id, channel) = (&app, bob.clone(), bob_id.clone(), channel.to_owned());
      async move {
        app
          .post(
            &bob,
            &format!("/conversations/{channel}/members"),
            json!({"user_id": bob_id}),
          )
          .await
      }
    };

    publish_self_join(&app, 1, "disabled").await;
    let refused = join(&private).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    // The policy is about private channels; adding others and public channels are unaffected.
    assert_eq!(join(&public).await.status, StatusCode::CREATED);
    let other = app
      .post(
        &bob,
        &format!("/conversations/{private}/members"),
        json!({"user_id": carol_id}),
      )
      .await;
    assert_eq!(other.status, StatusCode::CREATED, "{}", other.body);

    publish_self_join(&app, 2, "owner_approval").await;
    assert_eq!(join(&private).await.status, StatusCode::FORBIDDEN);
    // An owner who is a member adds the admin: that is the approval.
    let approved = app
      .post(
        &alice,
        &format!("/conversations/{private}/members"),
        json!({"user_id": bob_id}),
      )
      .await;
    assert_eq!(approved.status, StatusCode::CREATED, "{}", approved.body);
    app
      .delete(&alice, &format!("/conversations/{private}/members/{bob_id}"))
      .await;

    publish_self_join(&app, 3, "visible").await;
    assert_eq!(join(&private).await.status, StatusCode::CREATED);
    app.stop().await;
  }
}

/// A4 (server side): signed device lists are stored opaque, with a sequence that only goes up.
mod a4_device_lists {
  use super::*;

  #[tokio::test]
  async fn a_list_is_stored_compare_and_swap_and_readable_by_peers() {
    let app = app!();
    let (alice, bob) = (owner(), member("bob"));
    let alice_id = user_id(&app, &alice).await;
    user_id(&app, &bob).await;

    let none = app.get(&bob, &format!("/users/{alice_id}/device-list")).await;
    assert_eq!(none.status, StatusCode::NOT_FOUND, "{}", none.body);
    assert_eq!(none.body["error_code"], "device_list_not_found");

    let put = |expected: u64, sequence: u64, blob: &str| json!({"expected_sequence": expected, "sequence": sequence, "signed_list": blob});
    let first = app.put(&alice, "/me/device-list", put(0, 1, "AQID")).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);

    let seen = app.get(&bob, &format!("/users/{alice_id}/device-list")).await;
    assert_eq!(seen.body["sequence"], 1);
    assert_eq!(seen.body["signed_list"], "AQID");

    // A second device that missed the update loses the race.
    let stale = app.put(&alice, "/me/device-list", put(0, 1, "BAUG")).await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert_eq!(stale.body["current_version"], 1);

    // The sequence has to go up.
    let same = app.put(&alice, "/me/device-list", put(1, 1, "BAUG")).await;
    assert_eq!(same.status, StatusCode::BAD_REQUEST, "{}", same.body);
    let down = app.put(&alice, "/me/device-list", put(1, 0, "BAUG")).await;
    assert_eq!(down.status, StatusCode::BAD_REQUEST, "{}", down.body);

    let next = app.put(&alice, "/me/device-list", put(1, 2, "BAUG")).await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.body);
    let empty = app.put(&alice, "/me/device-list", put(2, 3, "")).await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn oversized_lists_are_refused() {
    let Some(app) = TestApp::start_with(|s| s.limits.max_device_list_bytes = 8).await else {
      return;
    };
    let alice = owner();
    user_id(&app, &alice).await;
    let big = "AAAAAAAAAAAAAAAAAAAA";
    let reply = app
      .put(
        &alice,
        "/me/device-list",
        json!({"expected_sequence": 0, "sequence": 1, "signed_list": big}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", reply.body);
    app.stop().await;
  }
}

/// A8 (server side): the admin client can list the channels of a user to sign the removals.
mod a8_admin_channels_of_user {
  use super::*;

  #[tokio::test]
  async fn the_channel_list_can_be_filtered_by_member() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    user_id(&app, &alice).await;
    // Alice creates both (`channel_with` makes the owner the creator, so she isn't added again).
    let with_carol = channel_with(&app, &[&carol]).await;
    let _without = channel_with(&app, &[]).await;

    let reply = app
      .get(&alice, &format!("/admin/conversations?user_id={carol_id}"))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let ids: Vec<&str> = reply.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .map(|c| c["id"].as_str().unwrap())
      .collect();
    assert_eq!(ids, vec![with_carol.as_str()]);
    app.stop().await;
  }
}
