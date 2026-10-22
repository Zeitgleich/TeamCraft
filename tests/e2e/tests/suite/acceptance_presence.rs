//! v1 acceptance, PLAN.md §4 Presence and status: automatic online, do not disturb, appearing
//! offline, custom status with expiry, and the admin switches.
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::{Peer, Stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn presence_follows_devices_and_manual_choices_and_the_admin_switches() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let carol = Peer::enroll(&stack, "carol").await;
  let carol_id = carol.user_id().to_string();
  let state_of_carol = |viewer: &Peer| {
    let (stack, tokens, id) = (&stack, viewer.tokens.clone(), carol_id.clone());
    async move {
      let reply = stack
        .get(&tokens, &format!("/presence?user_ids={id}"))
        .await
        .expect(StatusCode::OK);
      reply.body["presences"][0].clone()
    }
  };

  // A connected device is online (the gateway registers it a moment after connecting).
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
  while state_of_carol(&alice).await["state"] != "online" {
    assert!(
      std::time::Instant::now() < deadline,
      "the connected device never counted as online"
    );
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
  }

  // Do not disturb shows as such; "appear offline" hides her from everybody else.
  stack
    .put(&carol.tokens, "/me/presence", json!({"manual": "dnd"}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(state_of_carol(&alice).await["state"], "dnd");
  stack
    .put(&carol.tokens, "/me/presence", json!({"manual": "invisible"}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(state_of_carol(&alice).await["state"], "offline");
  stack
    .put(&carol.tokens, "/me/presence", json!({"manual": "auto"}))
    .await
    .expect(StatusCode::OK);
  assert_eq!(state_of_carol(&alice).await["state"], "online");

  // A custom status with an emoji and an end; it is visible next to the presence and can be cleared.
  let ends = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as i64
    + 3_600_000;
  stack
    .put(
      &carol.tokens,
      "/me/status",
      json!({"emoji": "🌴", "text": "Vacation", "expires_at": ends}),
    )
    .await
    .expect(StatusCode::OK);
  let shown = state_of_carol(&alice).await;
  assert_eq!(shown["status"]["text"], "Vacation", "{shown}");
  assert_eq!(shown["status"]["emoji"], "🌴");
  let in_the_past = stack
    .put(
      &carol.tokens,
      "/me/status",
      json!({"text": "gone", "expires_at": 1}),
    )
    .await;
  assert!(
    !in_the_past.status.is_success(),
    "an expired status was accepted: {in_the_past:?}"
  );
  stack
    .delete(&carol.tokens, "/me/status")
    .await
    .expect(StatusCode::NO_CONTENT);
  assert!(state_of_carol(&alice).await.get("status").is_none());

  // The owner turns custom statuses off and offers presets instead: the server enforces it.
  alice
    .set_policy(|policy| {
      policy.presence.custom_status_enabled = false;
      policy.presence.presets = vec![teamcraft_proto::StatusPreset {
        maybe_emoji: Some("🚌".into()),
        text: "Commuting".into(),
        maybe_duration_minutes: Some(60),
      }];
    })
    .await;
  let custom = stack
    .put(&carol.tokens, "/me/status", json!({"text": "my own words"}))
    .await;
  assert!(
    !custom.status.is_success(),
    "a custom status although they are off: {custom:?}"
  );
  let preset = stack
    .put(
      &carol.tokens,
      "/me/status",
      json!({"emoji": "🚌", "text": "Commuting"}),
    )
    .await;
  assert!(preset.status.is_success(), "a preset must still work: {preset:?}");
  // Statuses off altogether: presence only.
  alice
    .set_policy(|policy| policy.presence.statuses_enabled = false)
    .await;
  let none = stack
    .put(
      &carol.tokens,
      "/me/status",
      json!({"emoji": "🚌", "text": "Commuting"}),
    )
    .await;
  assert!(!none.status.is_success(), "{none:?}");

  stack.stop().await;
}
