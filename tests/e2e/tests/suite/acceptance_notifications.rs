//! v1 acceptance, PLAN.md §5 Notifications: per-conversation level and mute, unread and mention
//! counts, do-not-disturb schedule, push registration (with a fake token: the push services
//! themselves are never called from tests).
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_core::store::LocalStore;
use teamcraft_e2e::scenario::{channel_with, space_with};
use teamcraft_e2e::{Peer, Stack};
use teamcraft_proto::PushProvider;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn levels_mutes_unread_counts_dnd_and_push_registration_are_kept_by_the_server() {
  let Some(stack) = Stack::start().await else { return };
  let mut alice = Peer::enroll(&stack, "alice").await;
  let mut carol = Peer::enroll(&stack, "carol").await;
  let space = space_with(&stack, &alice, "Project A", &[(&carol, "member")]).await;
  let channel = channel_with(&mut alice, space, "news", &mut [&mut carol]).await;
  let settings = format!("/conversations/{channel}/notification-settings");

  // Per conversation: the level, and a mute until a time; unknown levels are refused.
  let initial = stack.get(&carol.tokens, &settings).await.expect(StatusCode::OK);
  assert_eq!(initial.body["level"], "mentions", "{}", initial.body);
  let until = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as i64
    + 3_600_000;
  stack
    .put(
      &carol.tokens,
      &settings,
      json!({"level": "all", "muted_until": until}),
    )
    .await
    .expect(StatusCode::OK);
  let stored = stack.get(&carol.tokens, &settings).await.expect(StatusCode::OK);
  assert_eq!(
    (stored.body["level"].as_str(), stored.body["muted_until"].as_i64()),
    (Some("all"), Some(until))
  );
  let unknown = stack
    .put(&carol.tokens, &settings, json!({"level": "shout"}))
    .await;
  assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{unknown:?}");

  // Unread and mention counts per conversation, cleared by the read position.
  alice.client.send_text(channel, None, "plain news").await.unwrap();
  alice
    .client
    .send_text_mentioning(channel, None, "carol, look", vec![carol.user_id()])
    .await
    .unwrap();
  let unread = stack
    .get(&carol.tokens, "/unread")
    .await
    .expect(StatusCode::OK)
    .body;
  let entry = &unread["conversations"][0];
  // (The channel's own system message counts as unread too, so "at least".)
  assert!(entry["unread_count"].as_u64() >= Some(2), "{unread}");
  assert_eq!(entry["mention_count"].as_u64(), Some(1), "{unread}");
  let newest = alice
    .store
    .messages(channel, teamcraft_core::store::MessageFilter::TopLevel)
    .unwrap()
    .last()
    .and_then(|message| message.maybe_server_id)
    .unwrap();
  stack
    .put(
      &carol.tokens,
      &format!("/conversations/{channel}/read-position"),
      json!({"message_id": newest.to_string()}),
    )
    .await
    .expect(StatusCode::NO_CONTENT);
  let unread = stack
    .get(&carol.tokens, "/unread")
    .await
    .expect(StatusCode::OK)
    .body;
  assert!(unread["conversations"].as_array().unwrap().is_empty(), "{unread}");

  // The user's own settings: a do-not-disturb schedule that mentions still pass through, the push
  // mode, read receipts.
  let me = json!({
    "auto_follow_mentioned_threads": true, "push_mode": "always", "send_read_receipts": false,
    "dnd_schedule": {
      "windows": [{"weekdays": ["mon", "tue", "wed", "thu", "fri"], "start_minute": 1080, "end_minute": 480}],
      "utc_offset_minutes": 120, "allowed_senders": [alice.user_id().to_string()],
    },
  });
  stack
    .put(&carol.tokens, "/me/notification-settings", me.clone())
    .await
    .expect(StatusCode::OK);
  let back = stack
    .get(&carol.tokens, "/me/notification-settings")
    .await
    .expect(StatusCode::OK);
  assert_eq!(back.body["push_mode"], "always");
  assert_eq!(
    back.body["dnd_schedule"]["utc_offset_minutes"], 120,
    "{}",
    back.body
  );
  let mut bad = me;
  bad["dnd_schedule"]["utc_offset_minutes"] = json!(99_999);
  let refused = stack.put(&carol.tokens, "/me/notification-settings", bad).await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{refused:?}");

  // Push registration with a fake token (the dispatch itself is covered by the server's tests).
  carol
    .client
    .register_push(PushProvider::Fcm, "acceptance-fake-token")
    .await
    .unwrap();

  stack.stop().await;
}
