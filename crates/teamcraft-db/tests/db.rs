//! Integration tests against Postgres. They run only with `--features integration` and skip (with a
//! note on stderr) when `DATABASE_URL` is not set, so `cargo test --workspace --all-features` also
//! works without the dev stack. Start it with `docker compose -f docker-compose.dev.yml up -d postgres`.
//! Every test migrates its own throwaway database on that server.
#![cfg(feature = "integration")]
// Test helpers outside `#[test]` functions are not covered by clippy.toml.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use teamcraft_db::testing::TestDatabase;
use teamcraft_db::{
  ImageChange, KeyPackageCounts, NewConversation, NewDevice, NewKeyPackage, NewMessage, NewPushRetry,
  NewSpace, OidcIdentity, PostOutcome, RoleLogFounding, SnowflakeGenerator, StoredSpaceImage,
  participants_key,
};
use teamcraft_proto::{
  ClientMessageId, ConversationId, ConversationKind, ConversationVisibility, DeviceId, HistoryPolicy, Id,
  IdempotencyKey, JoinPolicy, MessageId, PushKind, PushWakeUp, SpaceId, UserId, UserKind,
};

const ISSUER: &str = "https://auth.example.com/realms/dev";

#[tokio::test]
async fn key_packages_are_served_only_while_valid_and_swept_after() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let user = db
    .upsert_oidc_user(&identity("k", "Kim"), ids.next_id().unwrap())
    .await
    .unwrap();
  let device = db
    .register_device(&NewDevice {
      id: ids.next_id().unwrap(),
      user_id: user.id,
      name: "laptop",
      public_key: &[1; 32],
      cross_signature: &[2; 64],
    })
    .await
    .unwrap()
    .id;
  let now = time::OffsetDateTime::now_utc();
  let day = time::Duration::days(1);
  let package = |bytes: &'static [u8], expires_at| NewKeyPackage {
    id: ids.next_id::<DeviceId>().unwrap().snowflake(),
    bytes,
    expires_at,
  };
  let regular = [package(b"soon", now + 5 * day), package(b"later", now + 70 * day)];
  db.add_key_packages(device, &regular, Some(package(b"last", now + 20 * day)), 10, now)
    .await
    .unwrap();

  // With a renewal window of 30 days: one available, one expiring soon, no fresh last resort.
  let counts = db.key_package_counts(device, now, now + 30 * day).await.unwrap();
  assert_eq!(
    counts,
    KeyPackageCounts {
      available: 1,
      expiring_soon: 1,
      has_last_resort: false,
    }
  );
  // 100 days on: nothing counts any more.
  let later = now + 100 * day;
  let counts = db
    .key_package_counts(device, later, later + 30 * day)
    .await
    .unwrap();
  assert_eq!(
    (counts.available, counts.expiring_soon, counts.has_last_resort),
    (0, 0, false)
  );

  // Only packages valid for 10 more days are served, the one expiring first; then the last resort.
  let valid_until = now + 10 * day;
  let first = db.claim_key_package(device, valid_until).await.unwrap().unwrap();
  assert_eq!(
    (first.key_package.as_slice(), first.is_last_resort),
    (&b"later"[..], false)
  );
  let second = db.claim_key_package(device, valid_until).await.unwrap().unwrap();
  assert_eq!(
    (second.key_package.as_slice(), second.is_last_resort),
    (&b"last"[..], true)
  );
  // At +100 days nothing at all is served.
  assert!(db.claim_key_package(device, later).await.unwrap().is_none());

  // The sweep deletes what can't be served any more; the rest stays.
  assert_eq!(db.delete_expiring_key_packages(valid_until).await.unwrap(), 1);
  assert!(db.claim_key_package(device, now).await.unwrap().is_some());
  assert_eq!(db.delete_expiring_key_packages(later).await.unwrap(), 2);
  test_db.drop_database().await;
}

#[tokio::test]
async fn upsert_creates_then_updates_user() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let identity = identity("sub-1", "Alice");

  let first = db
    .upsert_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  let renamed = OidcIdentity {
    display_name: "Alice B.",
    kind: UserKind::Guest,
    ..identity
  };
  let second = db
    .upsert_oidc_user(&renamed, ids.next_id::<UserId>().unwrap())
    .await
    .unwrap();

  assert_eq!(first.id, second.id);
  assert_eq!(second.display_name, "Alice B.");
  assert_eq!(second.kind, UserKind::Guest);
  test_db.drop_database().await;
}

#[tokio::test]
async fn sync_keeps_existing_user_and_applies_changes() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let identity = identity("sub-2", "Bob");

  let first = db
    .sync_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  let again = db
    .sync_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  assert_eq!(first.id, again.id);

  let renamed = OidcIdentity {
    display_name: "Robert",
    ..identity
  };
  let updated = db.sync_oidc_user(&renamed, ids.next_id().unwrap()).await.unwrap();
  assert_eq!((updated.id, updated.display_name.as_str()), (first.id, "Robert"));
  test_db.drop_database().await;
}

#[tokio::test]
async fn partitions_are_created_idempotently() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  test_db.db.ensure_partitions(2).await.unwrap();
  test_db.db.ensure_partitions(2).await.unwrap();

  for table in ["messages", "mls_messages"] {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_inherits WHERE inhparent = $1::regclass")
      .bind(table)
      .fetch_one(&test_db.pool)
      .await
      .unwrap();
    // Default partition plus the months created by the migration and by the calls above.
    assert!(count >= 4, "unexpected partition count {count} for {table}");
  }
  test_db.drop_database().await;
}

#[tokio::test]
async fn rows_land_in_the_partition_of_their_month() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let pool = &test_db.pool;
  sqlx::query("INSERT INTO users (id, display_name, kind) VALUES (1, 'A', 'member')")
    .execute(pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO devices (id, user_id, name, public_key, cross_signature) VALUES (1, 1, 'd', '\\x00', '\\x00')")
    .execute(pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO conversations (id, kind, name, participants_key) VALUES (1, 'group', 'g', '1')")
    .execute(pool)
    .await
    .unwrap();

  // An ID created "now" must go into a monthly partition, not the default one.
  let id = SnowflakeGenerator::new(1)
    .unwrap()
    .next_snowflake()
    .unwrap()
    .get();
  sqlx::query(
    "INSERT INTO messages (id, conversation_id, sender_device_id, sender_user_id, content_key_id, ciphertext, \
     client_message_id) VALUES ($1, 1, 1, 1, '\\x01', '\\x02', '\\x00112233445566778899aabbccddeeff')",
  )
  .bind(id)
  .execute(pool)
  .await
  .unwrap();
  let partition: String = sqlx::query_scalar("SELECT tableoid::regclass::text FROM messages WHERE id = $1")
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
  assert_ne!(partition, "messages_default");
  assert!(
    partition.starts_with("messages_20"),
    "unexpected partition {partition}"
  );

  // Constraint: exactly one of inline ciphertext and S3 reference.
  let both_missing = sqlx::query(
    "INSERT INTO messages (id, conversation_id, sender_device_id, sender_user_id, content_key_id, client_message_id) \
     VALUES ($1 + 1, 1, 1, 1, '\\x01', '\\x00112233445566778899aabbccddeeff')",
  )
  .bind(id)
  .execute(pool)
  .await;
  assert!(both_missing.is_err());
  test_db.drop_database().await;
}

#[tokio::test]
async fn direct_messages_are_created_once_per_pair() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let alice = db
    .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
    .await
    .unwrap();
  let bob = db
    .upsert_oidc_user(&identity("b", "Bob"), ids.next_id().unwrap())
    .await
    .unwrap();

  let participants = [alice.id, bob.id];
  let make = |id: ConversationId| NewConversation {
    id,
    maybe_space: None,
    kind: ConversationKind::Dm,
    maybe_name: None,
    maybe_topic: None,
    maybe_description: None,
    visibility: ConversationVisibility::Private,
    history_policy: HistoryPolicy::FromJoin,
    maybe_category: None,
    maybe_participants_key: Some(participants_key(&participants)),
    is_integration: false,
  };
  let (first, created) = db
    .get_or_create_participants_conversation(&make(ids.next_id().unwrap()), &participants)
    .await
    .unwrap();
  let (second, created_again) = db
    .get_or_create_participants_conversation(&make(ids.next_id().unwrap()), &[bob.id, alice.id])
    .await
    .unwrap();

  assert!(created && !created_again);
  assert_eq!(first, second);
  assert_eq!(db.conversation_member_ids(first).await.unwrap().len(), 2);
  test_db.drop_database().await;
}

/// A DM goes with a deleted participant even when an earlier removal from all chats took their
/// membership (issue #1066); the import account, a member but no participant, takes no DM with it.
#[tokio::test]
async fn dms_go_with_their_deleted_participant_not_with_other_members() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let mut users = Vec::new();
  for (subject, name) in [("a", "Alice"), ("b", "Bob"), ("i", "Importer")] {
    let user = db
      .upsert_oidc_user(&identity(subject, name), ids.next_id().unwrap())
      .await
      .unwrap();
    users.push(user.id);
  }
  let [alice, bob, importer] = users[..] else {
    unreachable!()
  };
  let participants = [alice, bob];
  let new_dm = NewConversation {
    id: ids.next_id().unwrap(),
    maybe_space: None,
    kind: ConversationKind::Dm,
    maybe_name: None,
    maybe_topic: None,
    maybe_description: None,
    visibility: ConversationVisibility::Private,
    history_policy: HistoryPolicy::FromJoin,
    maybe_category: None,
    maybe_participants_key: Some(participants_key(&participants)),
    is_integration: false,
  };
  let (dm, _) = db
    .get_or_create_participants_conversation(&new_dm, &participants)
    .await
    .unwrap();
  db.add_importer(dm, importer).await.unwrap();

  assert!(
    db.direct_conversations_deleted_with(importer)
      .await
      .unwrap()
      .is_empty()
  );
  // Bob's deletion takes his memberships; the DM stays his, as deletions before #1066 left it.
  db.delete_user(bob).await.unwrap();
  assert_eq!(
    db.deleted_users_with_direct_conversations(&[], 10).await.unwrap(),
    vec![bob]
  );
  // A DM held by name doesn't count (data-export.md §4.2).
  assert!(
    db.deleted_users_with_direct_conversations(&[dm], 10)
      .await
      .unwrap()
      .is_empty()
  );
  let leftover = db.direct_conversations_deleted_with(bob).await.unwrap();
  assert_eq!(leftover.len(), 1);
  assert_eq!(leftover[0].conversation, dm);
  let mut remaining = leftover[0].remaining.clone();
  remaining.sort_unstable();
  assert_eq!(remaining, vec![alice, importer]);

  // A wake-up queued for alice's phone in that DM goes with it (#1136).
  let device = db
    .register_device(&NewDevice {
      id: ids.next_id().unwrap(),
      user_id: alice,
      name: "phone",
      public_key: &[1; 32],
      cross_signature: &[2; 64],
    })
    .await
    .unwrap()
    .id;
  let retry = NewPushRetry {
    id: ids.next_id().unwrap(),
    user: alice,
    device,
    is_user_endpoint: false,
    wake_up: PushWakeUp {
      kind: PushKind::Message,
      conversation_id: dm,
      maybe_message_id: None,
      maybe_sender_id: None,
      is_urgent: false,
      maybe_thread_id: None,
      maybe_call_id: None,
      maybe_badge: None,
      maybe_sent_at: None,
      maybe_meeting: None,
      maybe_message: None,
    },
    attempts: 1,
    delay: std::time::Duration::from_secs(60),
    ttl: std::time::Duration::from_secs(600),
  };
  assert!(db.enqueue_push_retry(&retry, 10).await.unwrap());
  assert_eq!(db.count_push_retries(device).await.unwrap(), 1);

  // A hold keeps it, decided under its lock.
  let kept = db
    .delete_direct_conversation_locked(dm, &leftover[0].remaining, |_| {
      Ok::<_, teamcraft_db::DbError>(true)
    })
    .await
    .unwrap();
  assert_eq!(kept, None);
  assert_eq!(db.count_push_retries(device).await.unwrap(), 1);
  db.delete_direct_conversation_locked(dm, &leftover[0].remaining, |_| {
    Ok::<_, teamcraft_db::DbError>(false)
  })
  .await
  .unwrap()
  .unwrap();
  assert_eq!(db.count_push_retries(device).await.unwrap(), 0);
  assert!(
    db.deleted_users_with_direct_conversations(&[], 10)
      .await
      .unwrap()
      .is_empty()
  );
  assert_eq!(db.deleted_conversations_of(alice).await.unwrap(), vec![dm]);
  test_db.drop_database().await;
}

/// Two devices founding a group at once publish different information for epoch 0: the first stays
/// (#504). Repeating it is fine, and later epochs replace each other.
#[tokio::test]
async fn the_first_group_information_of_epoch_zero_wins() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let alice = db
    .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
    .await
    .unwrap();
  let bob = db
    .upsert_oidc_user(&identity("b", "Bob"), ids.next_id().unwrap())
    .await
    .unwrap();
  let participants = [alice.id, bob.id];
  let new_dm = NewConversation {
    id: ids.next_id().unwrap(),
    maybe_space: None,
    kind: ConversationKind::Dm,
    maybe_name: None,
    maybe_topic: None,
    maybe_description: None,
    visibility: ConversationVisibility::Private,
    history_policy: HistoryPolicy::FromJoin,
    maybe_category: None,
    maybe_participants_key: Some(participants_key(&participants)),
    is_integration: false,
  };
  let (chat, _) = db
    .get_or_create_participants_conversation(&new_dm, &participants)
    .await
    .unwrap();

  let accepted = |outcome| matches!(outcome, PostOutcome::Accepted);
  assert!(accepted(
    db.put_mls_group_info(chat, 0, b"first", false).await.unwrap()
  ));
  assert!(!accepted(
    db.put_mls_group_info(chat, 0, b"second", false).await.unwrap()
  ));
  assert!(accepted(
    db.put_mls_group_info(chat, 0, b"first", false).await.unwrap()
  ));
  assert_eq!(
    db.current_mls_group_info(chat).await.unwrap(),
    Some((0, b"first".to_vec()))
  );
  // The organizer may replace it while the group is in epoch 0 (the first one could not be joined).
  assert!(accepted(
    db.put_mls_group_info(chat, 0, b"replacement", true)
      .await
      .unwrap()
  ));
  assert_eq!(
    db.current_mls_group_info(chat).await.unwrap(),
    Some((0, b"replacement".to_vec()))
  );
  test_db.drop_database().await;
}

#[tokio::test]
async fn a_channel_picture_is_set_replaced_and_removed_with_the_previous_key_returned() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let alice = db
    .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
    .await
    .unwrap();
  let space: SpaceId = ids.next_id().unwrap();
  db.create_space(&NewSpace {
    id: space,
    name: "S",
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    join_policy: JoinPolicy::Open,
    creator: alice.id,
    maybe_idempotency_key: None,
  })
  .await
  .unwrap();
  let channel: ConversationId = ids.next_id().unwrap();
  db.create_channel(
    &NewConversation {
      id: channel,
      maybe_space: Some(space),
      kind: ConversationKind::Channel,
      maybe_name: Some("general"),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Public,
      history_policy: HistoryPolicy::FromJoin,
      maybe_category: None,
      maybe_participants_key: None,
      is_integration: false,
    },
    alice.id,
    RoleLogFounding::Open,
    None,
  )
  .await
  .unwrap();
  let image = |key: &str| StoredSpaceImage {
    key: key.into(),
    version: format!("v-{key}"),
    size_bytes: 10,
  };
  let shown = |db: &teamcraft_db::Database| {
    let db = db.clone();
    async move {
      db.get_conversation(channel, alice.id)
        .await
        .unwrap()
        .unwrap()
        .conversation
        .maybe_image
    }
  };

  let replaced = |key: &str| ImageChange::Set {
    maybe_replaced_key: Some(key.to_owned()),
  };
  let nothing_replaced = ImageChange::Set {
    maybe_replaced_key: None,
  };

  assert_eq!(shown(db).await, None);
  assert_eq!(
    db.set_conversation_image(channel, Some(&image("a")))
      .await
      .unwrap(),
    nothing_replaced
  );
  assert_eq!(shown(db).await, Some(image("a")));
  // The replaced picture's object is returned for deletion; the same key is not.
  assert_eq!(
    db.set_conversation_image(channel, Some(&image("b")))
      .await
      .unwrap(),
    replaced("a")
  );
  assert_eq!(
    db.set_conversation_image(channel, Some(&image("b")))
      .await
      .unwrap(),
    nothing_replaced
  );
  assert_eq!(
    db.set_conversation_image(channel, None).await.unwrap(),
    replaced("b")
  );
  assert_eq!(shown(db).await, None);
  // A channel deleted while the picture uploaded: nothing is stored and the caller learns it.
  let gone: ConversationId = ids.next_id().unwrap();
  assert_eq!(
    db.set_conversation_image(gone, Some(&image("c"))).await.unwrap(),
    ImageChange::ChannelGone
  );
  test_db.drop_database().await;
}

#[tokio::test]
async fn sending_the_same_client_message_id_twice_stores_one_message() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let alice = db
    .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
    .await
    .unwrap();
  let space: SpaceId = ids.next_id().unwrap();
  db.create_space(&NewSpace {
    id: space,
    name: "S",
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    join_policy: JoinPolicy::Open,
    creator: alice.id,
    maybe_idempotency_key: None,
  })
  .await
  .unwrap();
  let channel: ConversationId = ids.next_id().unwrap();
  db.create_channel(
    &NewConversation {
      id: channel,
      maybe_space: Some(space),
      kind: ConversationKind::Channel,
      maybe_name: Some("general"),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Public,
      history_policy: HistoryPolicy::FromJoin,
      maybe_category: None,
      maybe_participants_key: None,
      is_integration: false,
    },
    alice.id,
    RoleLogFounding::Open,
    None,
  )
  .await
  .unwrap();
  let device: DeviceId = ids.next_id().unwrap();
  sqlx::query("INSERT INTO devices (id, user_id, name, public_key, cross_signature) VALUES ($1, $2, 'd', '\\x00', '\\x00')")
    .bind(device.snowflake().get())
    .bind(alice.id.snowflake().get())
    .execute(&test_db.pool)
    .await
    .unwrap();

  let client_id = ClientMessageId::from_bytes([7; 16]);
  let send = |id: MessageId| NewMessage {
    id,
    client_message_id: client_id,
    conversation_id: channel,
    sender_user_id: alice.id,
    sender_device_id: device,
    maybe_thread_id: None,
    mentioned_user_ids: &[],
    is_urgent: false,
    content_key_id: b"k",
    ciphertext: b"opaque",
  };
  let first = db.store_message(&send(ids.next_id().unwrap())).await.unwrap();
  let second = db.store_message(&send(ids.next_id().unwrap())).await.unwrap();

  assert!(first.is_new && !second.is_new);
  assert_eq!(first.message.id, second.message.id);
  assert_eq!(second.message.ciphertext, b"opaque");
  test_db.drop_database().await;
}

mod health_alerts {
  use teamcraft_db::{
    HealthNoticeChannel, HealthNoticeKind, HealthStateChanges, HealthStateRetention, StoredHealthAlert,
    StoredHealthNotice,
  };
  use teamcraft_proto::{HealthCheckId, HealthNoticeId, Snowflake};
  use time::{Duration, OffsetDateTime};

  use super::*;

  #[tokio::test]
  async fn the_history_and_the_outbox_read_back_as_written_and_a_second_write_changes_nothing() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let db = &test_db.db;
    let now = start();
    let changes = HealthStateChanges {
      alerts: vec![
        alert(HealthCheckId::Backup, now - hours(2), Some(now - hours(1))),
        alert(HealthCheckId::Database, now, None),
      ],
      notices: vec![notice(1, now), mail(2, now)],
      removed_notices: Vec::new(),
    };
    for _ in 0..2 {
      db.save_health_state(&changes, &keep_all(now)).await.unwrap();
    }
    let alerts = db.health_alerts(500).await.unwrap();
    assert_eq!(alerts.len(), 2);
    assert_eq!(alerts[0].check, HealthCheckId::Database, "newest first");
    assert_eq!(alerts[1], changes.alerts[0]);
    assert_eq!(db.health_notices().await.unwrap(), changes.notices);

    // Resolving updates the alert in place; a delivered notice goes, a retried one keeps its row.
    let mut retried = changes.notices[1].clone();
    retried.failures = 1;
    retried.next_attempt_at = now + Duration::seconds(30);
    let update = HealthStateChanges {
      alerts: vec![alert(HealthCheckId::Database, now, Some(now + hours(1)))],
      notices: vec![retried.clone()],
      removed_notices: vec![id(1)],
    };
    db.save_health_state(&update, &keep_all(now)).await.unwrap();
    let alerts = db.health_alerts(500).await.unwrap();
    assert_eq!(alerts.len(), 2);
    assert_eq!(alerts[0].maybe_resolved_at, Some(now + hours(1)));
    assert_eq!(db.health_notices().await.unwrap(), [retried]);
  }

  #[tokio::test]
  async fn retention_drops_old_and_surplus_resolved_alerts_and_stale_notices_but_never_open_alerts() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let db = &test_db.db;
    let now = start();
    let changes = HealthStateChanges {
      alerts: vec![
        // Open for long: stays whatever its age.
        alert(HealthCheckId::DiskSpace, now - hours(24 * 200), None),
        // Ended before the history: goes.
        alert(
          HealthCheckId::Backup,
          now - hours(24 * 100),
          Some(now - hours(24 * 99)),
        ),
        // Three recent ones, of which the count keeps the newest two.
        alert(HealthCheckId::Certificate, now - hours(3), Some(now - hours(2))),
        alert(HealthCheckId::Certificate, now - hours(5), Some(now - hours(4))),
        alert(HealthCheckId::Certificate, now - hours(7), Some(now - hours(6))),
      ],
      notices: vec![notice(1, now - hours(25)), notice(2, now - hours(1))],
      removed_notices: Vec::new(),
    };
    let retention = HealthStateRetention {
      resolved_before: now - hours(24 * 90),
      max_alerts: 2,
      notices_queued_before: now - hours(24),
    };
    db.save_health_state(&changes, &retention).await.unwrap();
    let raised: Vec<(HealthCheckId, OffsetDateTime)> = db
      .health_alerts(500)
      .await
      .unwrap()
      .into_iter()
      .map(|alert| (alert.check, alert.raised_at))
      .collect();
    assert_eq!(
      raised,
      [
        (HealthCheckId::Certificate, now - hours(3)),
        (HealthCheckId::Certificate, now - hours(5)),
        (HealthCheckId::DiskSpace, now - hours(24 * 200)),
      ]
    );
    let left: Vec<HealthNoticeId> = db
      .health_notices()
      .await
      .unwrap()
      .into_iter()
      .map(|notice| notice.id)
      .collect();
    assert_eq!(left, [id(2)]);
  }

  /// Whole seconds: Postgres keeps microseconds.
  fn start() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
  }

  fn hours(hours: i64) -> Duration {
    Duration::hours(hours)
  }

  fn keep_all(now: OffsetDateTime) -> HealthStateRetention {
    HealthStateRetention {
      resolved_before: now - hours(24 * 365),
      max_alerts: 500,
      notices_queued_before: now - hours(24 * 365),
    }
  }

  fn alert(
    check: HealthCheckId,
    raised_at: OffsetDateTime,
    maybe_resolved_at: Option<OffsetDateTime>,
  ) -> StoredHealthAlert {
    StoredHealthAlert {
      check,
      raised_at,
      maybe_resolved_at,
    }
  }

  fn notice(n: i64, queued_at: OffsetDateTime) -> StoredHealthNotice {
    StoredHealthNotice {
      id: id(n),
      check: HealthCheckId::Database,
      kind: HealthNoticeKind::Raised,
      changed_at: queued_at,
      channel: HealthNoticeChannel::Webhook,
      queued_at,
      next_attempt_at: queued_at,
      failures: 0,
    }
  }

  fn mail(n: i64, queued_at: OffsetDateTime) -> StoredHealthNotice {
    StoredHealthNotice {
      kind: HealthNoticeKind::Resolved,
      channel: HealthNoticeChannel::Mail {
        address: "admin@example.com".into(),
      },
      ..notice(n, queued_at)
    }
  }

  fn id(n: i64) -> HealthNoticeId {
    HealthNoticeId::from_snowflake(Snowflake::new(n).unwrap())
  }
}

/// A legal hold signed at the same moment as a deletion of its channel (issue #2143): the two are
/// interleaved by their row locks, never by timing, and exactly one of them wins.
mod legal_hold_race {
  use std::time::Duration;

  use teamcraft_db::{
    AppendOutcome, Database, DbError, HeldPerson, HistoryQuery, NewAuditEntry, NewWorkspaceLogEntry,
    RetentionCut, Withdrawal,
  };
  use teamcraft_proto::WorkspaceLogKind;

  use super::*;

  const HOLD_POLICY: &[u8] = b"a policy that holds the channel";
  const LOCK_WAIT_LIMIT: Duration = Duration::from_secs(30);
  const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

  /// The deletion has the channel locked and has read the policy (no hold yet) when the hold is
  /// appended: the append waits for the deletion, then finds the channel deleted and stores nothing.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_hold_appended_while_a_deletion_runs_is_refused_for_the_deleted_channel() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    let (in_decide, is_in_decide) = tokio::sync::oneshot::channel();
    let (go_on, may_go_on) = std::sync::mpsc::channel::<()>();

    let deletion = tokio::spawn({
      let db = test_db.db.clone();
      let channel = fixture.channel;
      let audit_id = fixture.ids.next_snowflake().unwrap();
      async move {
        db.delete_channel_locked(channel, &audit(audit_id), None, move |_, _, evidence| {
          assert_eq!(evidence.maybe_policy, None);
          in_decide.send(()).unwrap();
          // Holds the transaction open, with the channel locked, until the append waits for it.
          // Blocking is fine: this worker runs nothing else meanwhile.
          may_go_on.recv().unwrap();
          Ok::<_, DbError>(())
        })
        .await
      }
    });
    is_in_decide.await.unwrap();
    let append = tokio::spawn(append_hold(test_db.db.clone(), fixture.clone()));
    wait_for_lock_waiters(&test_db.pool, 1).await;
    go_on.send(()).unwrap();

    assert!(deletion.await.unwrap().unwrap().is_some());
    assert_eq!(append.await.unwrap(), AppendOutcome::NotExistingConversations);
    assert_eq!(
      test_db
        .db
        .workspace_log_version(&WorkspaceLogKind::Policy)
        .await
        .unwrap(),
      0
    );
    test_db.drop_database().await;
  }

  /// The append has the channel share-locked when the deletion starts: the deletion waits for the
  /// append's commit and then reads the policy that holds the channel.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_deletion_started_while_a_hold_is_appended_reads_the_hold() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    let append = PendingAppend::start(&test_db, &fixture).await;

    let deletion = tokio::spawn({
      let db = test_db.db.clone();
      let channel = fixture.channel;
      let audit_id = fixture.ids.next_snowflake().unwrap();
      async move {
        db.delete_channel_locked(channel, &audit(audit_id), None, |_, _, evidence| {
          match evidence.maybe_policy.as_deref() {
            Some(payload) => Err(Refusal::Held(payload.to_vec())),
            None => Ok(()),
          }
        })
        .await
      }
    });
    wait_for_lock_waiters(&test_db.pool, 2).await;

    assert_eq!(append.finish().await, AppendOutcome::Appended);
    match deletion.await.unwrap() {
      Err(Refusal::Held(payload)) => assert_eq!(payload, HOLD_POLICY),
      other => panic!("the deletion should have read the hold, got {other:?}"),
    }
    assert!(test_db.db.channel_facts(fixture.channel).await.unwrap().is_some());
    test_db.drop_database().await;
  }

  /// The same for a space: its deletion locks all its channels in id order and waits for the
  /// append's share lock on the held one.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_space_deletion_started_while_a_hold_is_appended_reads_the_hold() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    let append = PendingAppend::start(&test_db, &fixture).await;

    let deletion = tokio::spawn({
      let db = test_db.db.clone();
      let space = fixture.space;
      let audit_id = fixture.ids.next_snowflake().unwrap();
      async move {
        db.delete_space_locked(space, &audit(audit_id), None, |_, evidence| {
          match evidence.maybe_policy.as_deref() {
            Some(payload) => Err(Refusal::Held(payload.to_vec())),
            None => Ok(()),
          }
        })
        .await
      }
    });
    wait_for_lock_waiters(&test_db.pool, 2).await;

    assert_eq!(append.finish().await, AppendOutcome::Appended);
    assert!(matches!(deletion.await.unwrap(), Err(Refusal::Held(_))));
    assert!(test_db.db.channel_facts(fixture.channel).await.unwrap().is_some());
    test_db.drop_database().await;
  }

  /// Leaving the space with `deletes_memberless` would delete the private channel only the leaver is
  /// in: it waits for the append and then keeps the held channel.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn leaving_a_space_while_a_hold_is_appended_keeps_the_held_channel() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    let append = PendingAppend::start(&test_db, &fixture).await;

    let leave = tokio::spawn({
      let db = test_db.db.clone();
      let (space, user, channel) = (fixture.space, fixture.alice, fixture.channel);
      async move {
        db.remove_space_member(space, user, true, |evidence| {
          Ok::<_, DbError>(match evidence.maybe_policy.as_deref() {
            Some(payload) if payload == HOLD_POLICY => vec![channel],
            _ => Vec::new(),
          })
        })
        .await
      }
    });
    wait_for_lock_waiters(&test_db.pool, 2).await;

    assert_eq!(append.finish().await, AppendOutcome::Appended);
    let left = leave.await.unwrap().unwrap();
    assert_eq!(left.left, vec![fixture.channel]);
    assert!(left.deleted.is_empty());
    assert!(test_db.db.channel_facts(fixture.channel).await.unwrap().is_some());
    test_db.drop_database().await;
  }

  /// A person hold (issue #2135) locks the conversations the person is in like a hold by name: a
  /// deletion that starts meanwhile waits and then finds the channel covered.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn a_deletion_started_while_a_person_hold_is_appended_finds_the_channel_covered() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    let people = [held(fixture.alice, false)];
    let append = PendingAppend::start_with(&test_db, {
      let (db, fixture) = (test_db.db.clone(), fixture.clone());
      async move { append_people(&db, &fixture, 0, &people).await }
    })
    .await;

    let deletion = tokio::spawn({
      let db = test_db.db.clone();
      let channel = fixture.channel;
      let audit_id = fixture.ids.next_snowflake().unwrap();
      async move {
        db.delete_channel_locked(channel, &audit(audit_id), None, move |_, _, evidence| {
          if evidence.covered_by_people.contains(&channel) {
            Err(Refusal::Held(Vec::new()))
          } else {
            Ok(())
          }
        })
        .await
      }
    });
    wait_for_lock_waiters(&test_db.pool, 2).await;

    assert_eq!(append.finish().await, AppendOutcome::Appended);
    assert!(matches!(deletion.await.unwrap(), Err(Refusal::Held(_))));
    assert!(test_db.db.channel_facts(fixture.channel).await.unwrap().is_some());
    test_db.drop_database().await;
  }

  /// A held person's conversations stay covered after they leave, until the hold is released; a
  /// hold on their direct messages only leaves channels out; unknown people are refused.
  #[tokio::test]
  async fn a_person_hold_covers_what_they_are_in_and_what_they_leave_while_held() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let db = &test_db.db;
    let fixture = Fixture::create(db).await;
    let stranger: UserId = fixture.ids.next_id().unwrap();
    assert_eq!(
      append_people(db, &fixture, 0, &[held(stranger, false)]).await,
      AppendOutcome::UnknownPeople
    );
    assert_eq!(
      append_people(db, &fixture, 0, &[held(fixture.alice, true)]).await,
      AppendOutcome::Appended
    );
    assert!(db.conversations_covered_by_people().await.unwrap().is_empty());
    assert_eq!(
      append_people(db, &fixture, 1, &[held(fixture.alice, false)]).await,
      AppendOutcome::Appended
    );
    assert_eq!(
      db.conversations_covered_by_people().await.unwrap(),
      vec![fixture.channel]
    );
    assert_eq!(db.held_people().await.unwrap(), vec![held(fixture.alice, false)]);

    let left = db
      .remove_space_member(fixture.space, fixture.alice, false, |_| {
        Ok::<_, DbError>(Vec::new())
      })
      .await
      .unwrap();
    assert_eq!(left.left, vec![fixture.channel]);
    assert_eq!(
      db.conversations_covered_by_people().await.unwrap(),
      vec![fixture.channel]
    );

    assert_eq!(append_people(db, &fixture, 2, &[]).await, AppendOutcome::Appended);
    assert!(db.conversations_covered_by_people().await.unwrap().is_empty());
    assert!(db.held_people().await.unwrap().is_empty());
    // Held again: the departure before this hold doesn't count.
    assert_eq!(
      append_people(db, &fixture, 3, &[held(fixture.alice, false)]).await,
      AppendOutcome::Appended
    );
    assert!(db.conversations_covered_by_people().await.unwrap().is_empty());
    test_db.drop_database().await;
  }

  /// A member's withdrawal of their own message: gone at once without a hold; under one, marked,
  /// hidden from history and removed once no hold keeps the conversation.
  #[tokio::test]
  async fn a_withdrawal_under_a_hold_waits_for_the_release() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let db = &test_db.db;
    let fixture = Fixture::create(db).await;
    let kept = ClientMessageId::from_bytes([1; 16]);
    let gone = ClientMessageId::from_bytes([2; 16]);
    for client_id in [kept, gone] {
      store(db, &fixture, client_id).await;
    }
    let other: UserId = fixture.ids.next_id().unwrap();
    assert_eq!(
      db.withdraw_message_locked(fixture.channel, other, kept, |_| Ok::<_, teamcraft_db::DbError>(
        false
      ))
      .await
      .unwrap(),
      Withdrawal::Nothing
    );
    assert_eq!(
      db.withdraw_message_locked(fixture.channel, fixture.alice, gone, |_| Ok::<
        _,
        teamcraft_db::DbError,
      >(false))
        .await
        .unwrap(),
      Withdrawal::Deleted {
        maybe_object_key: None
      }
    );
    assert!(
      db.get_message_by_client_id(fixture.channel, gone)
        .await
        .unwrap()
        .is_none()
    );
    for _ in 0..2 {
      assert_eq!(
        db.withdraw_message_locked(fixture.channel, fixture.alice, kept, |_| Ok::<
          _,
          teamcraft_db::DbError,
        >(true))
          .await
          .unwrap(),
        Withdrawal::KeptForHold
      );
    }
    assert!(history(db, &fixture).await.is_empty());
    assert!(
      db.get_message_by_client_id(fixture.channel, kept)
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(
      db.conversations_with_pending_deletions(10).await.unwrap(),
      vec![fixture.channel]
    );
    assert_eq!(
      db.apply_pending_deletions_locked(fixture.channel, |_| Ok::<_, teamcraft_db::DbError>(true))
        .await
        .unwrap(),
      None
    );
    let applied = db
      .apply_pending_deletions_locked(fixture.channel, |_| Ok::<_, teamcraft_db::DbError>(false))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(applied.messages, 1);
    assert!(
      db.get_message_by_client_id(fixture.channel, kept)
        .await
        .unwrap()
        .is_none()
    );
    assert!(
      db.conversations_with_pending_deletions(10)
        .await
        .unwrap()
        .is_empty()
    );
    test_db.drop_database().await;
  }

  /// Retention locks the conversation before it deletes anything: a hold appended while the run is
  /// on its way keeps every message.
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn retention_started_while_a_hold_is_appended_deletes_nothing() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let fixture = Fixture::create(&test_db.db).await;
    test_db
      .db
      .store_message(&NewMessage {
        id: fixture.ids.next_id().unwrap(),
        client_message_id: ClientMessageId::from_bytes([7; 16]),
        conversation_id: fixture.channel,
        sender_user_id: fixture.alice,
        sender_device_id: fixture.signer,
        maybe_thread_id: None,
        mentioned_user_ids: &[],
        is_urgent: false,
        content_key_id: b"k",
        ciphertext: b"opaque",
      })
      .await
      .unwrap();
    let cut = RetentionCut {
      before: fixture.ids.next_id().unwrap(),
      index_cutoff: time::OffsetDateTime::now_utc(),
      keep_pinned: false,
    };
    let append = PendingAppend::start(&test_db, &fixture).await;

    let retention = tokio::spawn({
      let db = test_db.db.clone();
      let channel = fixture.channel;
      async move {
        db.delete_past_retention_locked(channel, &cut, |evidence| {
          Ok::<_, DbError>(evidence.maybe_policy.as_deref() == Some(HOLD_POLICY))
        })
        .await
      }
    });
    wait_for_lock_waiters(&test_db.pool, 2).await;

    assert_eq!(append.finish().await, AppendOutcome::Appended);
    assert_eq!(retention.await.unwrap().unwrap(), None);
    let messages: i64 = sqlx::query_scalar("SELECT count(*) FROM messages WHERE conversation_id = $1")
      .bind(fixture.channel.snowflake().get())
      .fetch_one(&test_db.pool)
      .await
      .unwrap();
    assert_eq!(messages, 1);
    test_db.drop_database().await;
  }

  #[derive(Debug)]
  enum Refusal {
    Held(Vec<u8>),
    Db(#[allow(dead_code)] DbError),
  }

  impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
      Self::Db(error)
    }
  }

  /// Alice, her device, her space and a private channel only she is in.
  #[derive(Clone)]
  struct Fixture {
    ids: std::sync::Arc<SnowflakeGenerator>,
    alice: UserId,
    space: SpaceId,
    channel: ConversationId,
    signer: DeviceId,
  }

  impl Fixture {
    async fn create(db: &Database) -> Self {
      let ids = std::sync::Arc::new(SnowflakeGenerator::new(1).unwrap());
      let alice = db
        .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
        .await
        .unwrap();
      let signer = db
        .register_device(&NewDevice {
          id: ids.next_id().unwrap(),
          user_id: alice.id,
          name: "laptop",
          public_key: &[1; 32],
          cross_signature: &[2; 64],
        })
        .await
        .unwrap()
        .id;
      let space: SpaceId = ids.next_id().unwrap();
      db.create_space(&NewSpace {
        id: space,
        name: "S",
        maybe_icon: None,
        maybe_color: None,
        maybe_description: None,
        join_policy: JoinPolicy::Open,
        creator: alice.id,
        maybe_idempotency_key: None,
      })
      .await
      .unwrap();
      let channel: ConversationId = ids.next_id().unwrap();
      db.create_channel(
        &NewConversation {
          id: channel,
          maybe_space: Some(space),
          kind: ConversationKind::Channel,
          maybe_name: Some("general"),
          maybe_topic: None,
          maybe_description: None,
          visibility: ConversationVisibility::Private,
          history_policy: HistoryPolicy::FromJoin,
          maybe_category: None,
          maybe_participants_key: None,
          is_integration: false,
        },
        alice.id,
        RoleLogFounding::Open,
        None,
      )
      .await
      .unwrap();
      Self {
        ids,
        alice: alice.id,
        space,
        channel,
        signer,
      }
    }
  }

  /// The real append of the hold, stopped after it share-locked the channel: another append of the
  /// same version, not yet committed, makes it wait on the log's primary key.
  struct PendingAppend {
    rival: sqlx::Transaction<'static, sqlx::Postgres>,
    task: tokio::task::JoinHandle<AppendOutcome>,
  }

  impl PendingAppend {
    async fn start(test_db: &TestDatabase, fixture: &Fixture) -> Self {
      Self::start_with(test_db, append_hold(test_db.db.clone(), fixture.clone())).await
    }

    async fn start_with(
      test_db: &TestDatabase,
      append: impl std::future::Future<Output = AppendOutcome> + Send + 'static,
    ) -> Self {
      let mut rival = test_db.pool.begin().await.unwrap();
      sqlx::query(
        "INSERT INTO workspace_log (kind, version, payload, signature) VALUES ('policy', 1, 'x', 'y')",
      )
      .execute(&mut *rival)
      .await
      .unwrap();
      let task = tokio::spawn(append);
      wait_for_lock_waiters(&test_db.pool, 1).await;
      Self { rival, task }
    }

    /// Lets the append go on and returns what it came to.
    async fn finish(self) -> AppendOutcome {
      self.rival.rollback().await.unwrap();
      self.task.await.unwrap()
    }
  }

  async fn append_hold(db: Database, fixture: Fixture) -> AppendOutcome {
    db.append_workspace_log(&NewWorkspaceLogEntry {
      kind: &WorkspaceLogKind::Policy,
      expected_version: 0,
      payload: HOLD_POLICY,
      signature: b"signature",
      signer: fixture.signer,
      new_holds: &[fixture.channel],
      maybe_held_people: Some(&[]),
      audit: &[],
    })
    .await
    .unwrap()
  }

  /// Appends a policy at `expected_version` that holds `people` and nothing by name.
  async fn append_people(
    db: &Database,
    fixture: &Fixture,
    expected_version: u64,
    people: &[HeldPerson],
  ) -> AppendOutcome {
    db.append_workspace_log(&NewWorkspaceLogEntry {
      kind: &WorkspaceLogKind::Policy,
      expected_version,
      payload: b"a policy that holds people",
      signature: b"signature",
      signer: fixture.signer,
      new_holds: &[],
      maybe_held_people: Some(people),
      audit: &[],
    })
    .await
    .unwrap()
  }

  fn held(user: UserId, is_direct_only: bool) -> HeldPerson {
    HeldPerson { user, is_direct_only }
  }

  async fn store(db: &Database, fixture: &Fixture, client_message_id: ClientMessageId) {
    db.store_message(&NewMessage {
      id: fixture.ids.next_id().unwrap(),
      client_message_id,
      conversation_id: fixture.channel,
      sender_user_id: fixture.alice,
      sender_device_id: fixture.signer,
      maybe_thread_id: None,
      mentioned_user_ids: &[],
      is_urgent: false,
      content_key_id: b"k",
      ciphertext: b"opaque",
    })
    .await
    .unwrap();
  }

  async fn history(db: &Database, fixture: &Fixture) -> Vec<teamcraft_db::StoredMessage> {
    db.list_messages(&HistoryQuery {
      conversation: fixture.channel,
      maybe_before: None,
      maybe_after: None,
      maybe_not_before: None,
      sees_imported: false,
      limit: 10,
    })
    .await
    .unwrap()
  }

  fn audit(id: teamcraft_proto::Snowflake) -> NewAuditEntry<'static> {
    NewAuditEntry {
      id,
      maybe_actor: None,
      action: "channel_deleted",
      maybe_target_kind: None,
      maybe_target_id: None,
      detail: "{}",
    }
  }

  /// Waits until `count` sessions of this test's database wait for a lock: the interleaving the
  /// test sets up, observed rather than timed.
  async fn wait_for_lock_waiters(pool: &teamcraft_db::testing::PgPool, count: i64) {
    let mut poll = tokio::time::interval(LOCK_POLL_INTERVAL);
    let waiting = async {
      loop {
        poll.tick().await;
        let waiters: i64 = sqlx::query_scalar(
          "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiters >= count {
          return;
        }
      }
    };
    tokio::time::timeout(LOCK_WAIT_LIMIT, waiting)
      .await
      .expect("the other transaction never waited for the lock");
  }
}

fn identity<'a>(subject: &'a str, display_name: &'a str) -> OidcIdentity<'a> {
  OidcIdentity {
    issuer: ISSUER,
    subject,
    maybe_username: None,
    maybe_email: None,
    display_name,
    kind: UserKind::Member,
    maybe_oidc_role: None,
    maybe_organization: None,
    should_sync_organization: false,
  }
}

#[tokio::test]
async fn creating_a_space_or_channel_twice_with_one_key_creates_one() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let alice = db
    .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
    .await
    .unwrap();
  let bob = db
    .upsert_oidc_user(&identity("b", "Bob"), ids.next_id().unwrap())
    .await
    .unwrap();
  let key = IdempotencyKey::from_bytes([9; 16]);
  let new_space = |id: SpaceId, creator: UserId, maybe_key: Option<IdempotencyKey>| NewSpace {
    id,
    name: "S",
    maybe_icon: None,
    maybe_color: None,
    maybe_description: None,
    join_policy: JoinPolicy::Open,
    creator,
    maybe_idempotency_key: maybe_key,
  };

  let first = db
    .create_space(&new_space(ids.next_id().unwrap(), alice.id, Some(key)))
    .await
    .unwrap();
  let second = db
    .create_space(&new_space(ids.next_id().unwrap(), alice.id, Some(key)))
    .await
    .unwrap();
  // Keys are per user: another user's identical key is a different request.
  let other_user = db
    .create_space(&new_space(ids.next_id().unwrap(), bob.id, Some(key)))
    .await
    .unwrap();
  // Without a key every call creates.
  let keyless = db
    .create_space(&new_space(ids.next_id().unwrap(), alice.id, None))
    .await
    .unwrap();

  assert!(first.is_new && !second.is_new);
  assert_eq!(first.id, second.id);
  assert!(other_user.is_new && other_user.id != first.id);
  assert!(keyless.is_new);
  let spaces: i64 = sqlx::query_scalar("SELECT count(*) FROM spaces")
    .fetch_one(&test_db.pool)
    .await
    .unwrap();
  assert_eq!(spaces, 3);

  // The same key for another kind of resource does not collide.
  let channel = NewConversation {
    id: ids.next_id().unwrap(),
    maybe_space: Some(first.id),
    kind: ConversationKind::Channel,
    maybe_name: Some("general"),
    maybe_topic: None,
    maybe_description: None,
    visibility: ConversationVisibility::Public,
    history_policy: HistoryPolicy::FromJoin,
    maybe_category: None,
    maybe_participants_key: None,
    is_integration: false,
  };
  let created = db
    .create_channel(&channel, alice.id, RoleLogFounding::Open, Some(&key))
    .await
    .unwrap();
  let again = db
    .create_channel(
      &NewConversation {
        id: ids.next_id().unwrap(),
        ..channel
      },
      alice.id,
      RoleLogFounding::Open,
      Some(&key),
    )
    .await
    .unwrap();
  assert!(created.is_new && !again.is_new);
  assert_eq!(created.id, again.id);
  test_db.drop_database().await;
}

/// Running call recordings and their audit rows (issue #2117).
mod call_recordings {
  use teamcraft_db::{AuditQuery, CallRecordingStart, RecordingAudit, RunningCallRecording};
  use teamcraft_proto::CallId;

  use super::*;

  #[tokio::test]
  async fn a_start_announced_again_keeps_one_entry_and_the_stop_has_the_peak_and_the_duration() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let db = &test_db.db;
    let ids = SnowflakeGenerator::new(1).unwrap();
    let alice = db
      .upsert_oidc_user(&identity("a", "Alice"), ids.next_id().unwrap())
      .await
      .unwrap();
    let channel = channel(db, &ids, alice.id).await;
    let start = CallRecordingStart {
      call_id: ids.next_id::<CallId>().unwrap(),
      device_id: ids.next_id::<DeviceId>().unwrap(),
      user_id: alice.id,
      conversation_id: channel,
      participants: 3,
    };
    let audit = |action| RecordingAudit {
      id: ids.next_snowflake().unwrap(),
      action,
    };
    assert!(
      db.start_call_recording(&start, audit("started"), "{}")
        .await
        .unwrap()
    );
    // The SFU restarted: the recorder announces the recording again, to more people now.
    let again = CallRecordingStart {
      participants: 5,
      ..start
    };
    assert!(
      !db
        .start_call_recording(&again, audit("started"), "{}")
        .await
        .unwrap()
    );
    let running = RunningCallRecording {
      call_id: start.call_id,
      device_id: start.device_id,
    };
    assert_eq!(db.running_call_recordings().await.unwrap(), [running]);

    let ended = db
      .end_call_recording(running, 4, audit("stopped"), |ended| {
        format!(r#"{{"participants":{}}}"#, ended.peak_participants)
      })
      .await
      .unwrap()
      .unwrap();
    assert_eq!(ended.peak_participants, 5, "the most of every report");
    assert_eq!(ended.user_id, alice.id);
    assert_eq!(ended.conversation_id, channel);
    assert!(ended.duration_secs < 60);
    let twice = db
      .end_call_recording(running, 4, audit("stopped"), |_| String::new())
      .await
      .unwrap();
    assert_eq!(twice, None, "a stop that came twice writes nothing");
    assert!(db.running_call_recordings().await.unwrap().is_empty());

    let rows = db
      .list_audit(&AuditQuery {
        limit: 10,
        ..AuditQuery::default()
      })
      .await
      .unwrap();
    let actions: Vec<&str> = rows.iter().map(|row| row.action.as_str()).collect();
    assert_eq!(actions, ["stopped", "started"], "one start and one stop");
    assert_eq!(rows[0].maybe_actor, Some(alice.id));
    assert_eq!(rows[0].maybe_target_kind.as_deref(), Some("conversation"));
    assert_eq!(rows[0].maybe_target_id, Some(channel.snowflake().get()));
    assert_eq!(rows[0].detail, r#"{"participants": 5}"#);
    test_db.drop_database().await;
  }

  /// A DM of Alice's and Bob's: the call's conversation.
  async fn channel(db: &teamcraft_db::Database, ids: &SnowflakeGenerator, alice: UserId) -> ConversationId {
    let bob = db
      .upsert_oidc_user(&identity("b", "Bob"), ids.next_id().unwrap())
      .await
      .unwrap()
      .id;
    let participants = [alice, bob];
    let (id, _) = db
      .get_or_create_participants_conversation(
        &NewConversation {
          id: ids.next_id().unwrap(),
          maybe_space: None,
          kind: ConversationKind::Dm,
          maybe_name: None,
          maybe_topic: None,
          maybe_description: None,
          visibility: ConversationVisibility::Private,
          history_policy: HistoryPolicy::FromJoin,
          maybe_category: None,
          maybe_participants_key: Some(participants_key(&participants)),
          is_integration: false,
        },
        &participants,
      )
      .await
      .unwrap();
    id
  }
}
