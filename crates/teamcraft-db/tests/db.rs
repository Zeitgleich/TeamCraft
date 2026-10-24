//! Integration tests against Postgres. They run only with `--features integration` and skip (with a
//! note on stderr) when `DATABASE_URL` is not set, so `cargo test --workspace --all-features` also
//! works without the dev stack. Start it with `docker compose -f docker-compose.dev.yml up -d postgres`.
//! Every test migrates its own throwaway database on that server.
#![cfg(feature = "integration")]
// Test helpers outside `#[test]` functions are not covered by clippy.toml.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use teamcraft_db::testing::TestDatabase;
use teamcraft_db::{
  KeyPackageCounts, NewConversation, NewDevice, NewKeyPackage, NewMessage, NewSpace, OidcIdentity,
  SnowflakeGenerator, participants_key,
};
use teamcraft_proto::{
  ClientMessageId, ConversationId, ConversationKind, ConversationVisibility, DeviceId, HistoryPolicy, Id,
  IdempotencyKey, JoinPolicy, MessageId, SpaceId, UserId, UserKind,
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
  let created = db.create_channel(&channel, alice.id, Some(&key)).await.unwrap();
  let again = db
    .create_channel(
      &NewConversation {
        id: ids.next_id().unwrap(),
        ..channel
      },
      alice.id,
      Some(&key),
    )
    .await
    .unwrap();
  assert!(created.is_new && !again.is_new);
  assert_eq!(created.id, again.id);
  test_db.drop_database().await;
}
