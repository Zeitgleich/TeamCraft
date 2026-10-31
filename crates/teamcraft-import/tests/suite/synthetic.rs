//! A synthetic Synology export with every channel and post kind, imported into the fake server:
//! channels, DMs, notes, threads, files, stickers, mentions, reactions, read state, people without
//! an account, a member who sets up the app only afterwards, and a second run that changes nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

fn init_logs() {
  drop(
    tracing_subscriber::fmt()
      .with_test_writer()
      .with_env_filter("info")
      .try_init(),
  );
}

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, FakeServerApi};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::imported::ImportedAuthor;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_import::export::Export;
use teamcraft_import::mapping::Mapping;
use teamcraft_import::plan::{SkipReason, Target};
use teamcraft_import::progress::Progress;
use teamcraft_import::run::{Importer, RunOptions};
use teamcraft_proto::{ConversationId, ListMessagesQuery};

type TestClient = Client<FakeServerApi, Arc<MemoryStore>>;

/// Synology users: anna (7) and ben (8) are mapped, carl (9) is not; 50 is a webhook.
const CHANNELS: &[&str] = &[
  r#"{"id":1,"name":"general","display_name":"General","purpose":"Talk","type":1,"create_at":1,"close_at":null,"creator_id":7,"members":[7,8,9]}"#,
  r#"{"id":2,"name":"","type":2,"create_at":1,"members":[7,8]}"#,
  r#"{"id":3,"name":"","type":2,"create_at":1,"members":[7,9]}"#,
  r#"{"id":4,"name":"","type":3,"create_at":1,"members":[7]}"#,
  r#"{"id":5,"name":"bot","type":5,"create_at":1,"members":[7]}"#,
  r#"{"id":6,"name":"old","display_name":"Old","type":1,"create_at":1,"close_at":5,"members":[7,8]}"#,
  r#"{"id":7,"name":"monitoring","display_name":"Monitoring","type":1,"create_at":1,"members":[7]}"#,
  r#"{"id":8,"name":"empty","display_name":"Empty","type":1,"create_at":1,"members":[7]}"#,
  r#"{"id":9,"name":"skipme","display_name":"Skip me","type":1,"create_at":1,"members":[7]}"#,
];
/// Posts in channel and time order; IDs as Synology makes them don't matter here.
const POSTS: &[&str] = &[
  r#"{"id":100,"channel_id":1,"user_id":7,"type":1,"message":"Hello @u:8 and @u:9 :thumbsup: <https://example.com|docs>","create_at":1600000000000,"thread_id":100,"comment_count":1,"mentions":[8,9]}"#,
  r#"{"id":101,"channel_id":1,"user_id":9,"type":1,"message":"reply from somebody gone","create_at":1600000001000,"thread_id":100}"#,
  r#"{"id":102,"channel_id":1,"user_id":8,"type":2,"message":"report.pdf","create_at":1600000002000,"file_props":{"name":"report.pdf","size":5,"type":"pdf"}}"#,
  r#"{"id":103,"channel_id":1,"user_id":50,"type":1,"message":"build failed","create_at":1600000003000}"#,
  r#"{"id":104,"channel_id":1,"user_id":8,"type":3,"message":":dog_01:","create_at":1600000004000}"#,
  r#"{"id":105,"channel_id":1,"user_id":8,"type":2,"message":"lost.png","create_at":1600000005000,"file_props":{"name":"lost.png","size":9,"is_image":true,"type":"png"}}"#,
  r#"{"id":106,"channel_id":1,"user_id":7,"type":1,"message":"edited later","create_at":1600000006000,"update_at":1600000900000}"#,
  r#"{"id":200,"channel_id":2,"user_id":8,"type":1,"message":"just us","create_at":1600000010000}"#,
  r#"{"id":300,"channel_id":3,"user_id":9,"type":1,"message":"dm with carl","create_at":1600000020000}"#,
  r#"{"id":400,"channel_id":4,"user_id":7,"type":1,"message":"note to self","create_at":1600000030000}"#,
  r#"{"id":500,"channel_id":5,"user_id":7,"type":1,"message":"hi bot","create_at":1600000040000}"#,
  r#"{"id":600,"channel_id":6,"user_id":7,"type":1,"message":"archived talk","create_at":1600000050000}"#,
  r#"{"id":700,"channel_id":7,"user_id":50,"type":1,"message":"disk 91%","create_at":1600000060000}"#,
  r#"{"id":900,"channel_id":9,"user_id":7,"type":1,"message":"not wanted","create_at":1600000070000}"#,
];
const CHANNEL_MAPPING: &str = "channel_id,name,visibility,webhook,members,posts,last_post,created,archived,space,teamcraft_channel,import_history,import
1,general,private,,3,7,,,,Team,,yes,
5,bot,type5,,1,1,,,,,,yes,f
6,old,private,,2,1,,,yes,Team,,yes,
7,monitoring,private,yes,1,1,,,,Ops,,no,
8,empty,private,,1,0,,,,Team,,yes,
9,skipme,private,,1,1,,,,Team,,yes,no
";
const USER_MAPPING: &str = "synology_id,username,nickname,type,deleted,posts,last_post,teamcraft_user
7,a,,1,f,1,,anna
8,b,,1,f,1,,ben
9,c,,1,t,1,,
";

#[tokio::test]
async fn a_synthetic_export_comes_over_whole_and_a_second_run_changes_nothing() {
  init_logs();
  let dir = export_dir("whole");
  let server = FakeServer::new();
  let importer_api = server.add_user("Import");
  server.set_admin(importer_api.user_id());
  let mut anna = enroll(server.add_user("Anna"), "Anna").await;
  let ben_api = server.add_user("Ben");
  let ben_id = ben_api.user_id();
  let mut import_client = enroll(importer_api, "Import").await;

  let export = Export::open(&dir, None).unwrap();
  let mapping = Mapping::read(&dir.join("channels.csv"), &dir.join("users.csv")).unwrap();
  let mut progress = Progress::open(&dir.join("progress.sqlite")).unwrap();
  let plan = {
    let mut importer = Importer::start(&mut import_client, &export, &mapping, &mut progress, options())
      .await
      .unwrap();
    importer.run().await.unwrap()
  };

  // What was decided: channels, the DM, the notes; the rest skipped with reasons.
  let targets: Vec<(i64, &Target)> = plan
    .items
    .iter()
    .map(|item| (item.source, &item.target))
    .collect();
  assert!(matches!(
    targets.iter().find(|(source, _)| *source == 1),
    Some((_, Target::NewChannel { .. }))
  ));
  assert!(matches!(
    targets.iter().find(|(source, _)| *source == 2),
    Some((_, Target::Dm { .. }))
  ));
  assert!(matches!(
    targets.iter().find(|(source, _)| *source == 4),
    Some((_, Target::Notes { .. }))
  ));
  let reason_of = |source| {
    plan
      .skips
      .iter()
      .find(|skip| skip.source == source)
      .map(|skip| skip.reason)
  };
  assert_eq!(reason_of(3), Some(SkipReason::UnmappedParticipant));
  assert_eq!(reason_of(5), Some(SkipReason::Chatbot));
  assert_eq!(reason_of(8), Some(SkipReason::NoPosts));
  assert_eq!(reason_of(9), Some(SkipReason::Excluded));
  assert!(
    !plan
      .items
      .iter()
      .find(|item| item.source == 7)
      .unwrap()
      .has_history
  );

  // Anna reads the channel with its origin, mentions, links, emoji, files and the thread.
  let general = progress.conversation("synology:1").unwrap().unwrap();
  sync(&mut anna).await;
  anna.catch_up_messages(general).await.unwrap();
  let general_texts = texts(&anna, general);
  assert_eq!(
    general_texts[0],
    "Hello @ben and @Deleted user \u{1f44d} [docs](https://example.com)"
  );
  assert!(general_texts.contains(&"reply from somebody gone".to_string()));
  let stored = anna.store().messages(general, MessageFilter::All).unwrap();
  let origin_of = |body: &str| {
    stored
      .iter()
      .find_map(|message| match &message.content {
        MessageContent::Text { body: text, meta, .. } if text == body => meta.maybe_imported.clone(),
        _ => None,
      })
      .unwrap()
  };
  assert_eq!(
    origin_of("reply from somebody gone").author,
    ImportedAuthor::Deleted
  );
  assert_eq!(
    origin_of("build failed").author,
    ImportedAuthor::Named {
      name: "Jenkins".into()
    }
  );
  assert_eq!(
    origin_of("edited later").maybe_edited_at_ms,
    Some(1_600_000_900_000)
  );
  let first = origin_of(&general_texts[0]);
  assert_eq!(first.sent_at_ms, 1_600_000_000_000);
  assert_eq!(first.reactions.len(), 1);
  // The file came over, the lost picture is named as missing, the sticker is a picture.
  let attachments: Vec<String> = stored
    .iter()
    .filter_map(|message| match &message.content {
      MessageContent::Text { meta, .. } => Some(meta.attachments.iter().map(|file| file.file_name.clone())),
      _ => None,
    })
    .flatten()
    .collect();
  assert!(attachments.contains(&"report.pdf".to_string()), "{attachments:?}");
  assert!(attachments.contains(&"dog_01.png".to_string()), "{attachments:?}");
  let lost = stored.iter().find_map(|message| match &message.content {
    MessageContent::Text { meta, .. } => meta
      .maybe_imported
      .as_ref()
      .filter(|origin| origin.missing_files.contains(&"lost.png".to_string()))
      .cloned(),
    _ => None,
  });
  assert!(lost.is_some(), "the lost picture is named on its message");
  let reply = stored
    .iter()
    .find(|message| matches!(&message.content, MessageContent::Text { body, .. } if body == "reply from somebody gone"))
    .unwrap();
  assert!(
    reply.maybe_thread_id.is_some(),
    "the reply is in the root's thread"
  );

  // The DM and the notes.
  let dm = progress.conversation("synology:2").unwrap().unwrap();
  anna.catch_up_messages(dm).await.unwrap();
  assert_eq!(texts(&anna, dm), ["just us"]);
  let notes = progress.conversation("synology:4").unwrap().unwrap();
  anna.catch_up_messages(notes).await.unwrap();
  assert_eq!(texts(&anna, notes), ["note to self"]);

  // Read state from the export.
  assert!(
    server.read_position(general, anna.user_id()).is_some(),
    "{:?}",
    progress.notes("synology:1").unwrap()
  );

  // Ben sets up the app only now: the import device adds him with the history, then leaves.
  let mut ben = enroll(ben_api, "Ben").await;
  sync(&mut ben).await;
  {
    let mut importer = Importer::start(&mut import_client, &export, &mapping, &mut progress, options())
      .await
      .unwrap();
    for _ in 0..3 {
      importer.tend(&plan).await.unwrap();
      sync(&mut ben).await;
      sync(&mut anna).await;
    }
  }
  ben.catch_up_messages(general).await.unwrap();
  assert_eq!(texts(&ben, general).len(), texts(&anna, general).len());
  ben.catch_up_messages(dm).await.unwrap();
  assert_eq!(texts(&ben, dm), ["just us"]);
  assert!(ben.group_users(general).unwrap().contains(&ben_id));
  // Everybody is in: the import account left the channel, the DM and the notes.
  for conversation in [general, dm, notes] {
    sync(&mut anna).await;
    assert!(
      !anna
        .group_users(conversation)
        .unwrap()
        .contains(&import_client.user_id()),
      "{conversation}"
    );
  }

  // A second run sends nothing new.
  let before = server_count(&anna, general).await;
  {
    let mut importer = Importer::start(&mut import_client, &export, &mapping, &mut progress, options())
      .await
      .unwrap();
    importer.run().await.unwrap();
  }
  assert_eq!(server_count(&anna, general).await, before);
}

fn options() -> RunOptions {
  RunOptions {
    deleted_name: "Deleted user".into(),
    join_wait: std::time::Duration::from_millis(50),
  }
}

async fn server_count(client: &TestClient, conversation: ConversationId) -> usize {
  client
    .api()
    .list_messages(
      conversation,
      &ListMessagesQuery {
        maybe_limit: Some(1000),
        ..ListMessagesQuery::default()
      },
    )
    .await
    .unwrap()
    .messages
    .len()
}

async fn sync(client: &mut TestClient) {
  client.full_sync().await.unwrap();
  client.apply_pending_changes().await.unwrap();
}

fn texts(client: &TestClient, conversation: ConversationId) -> Vec<String> {
  client
    .store()
    .messages(conversation, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Text { body, .. } if !body.is_empty() => Some(body),
      _ => None,
    })
    .collect()
}

async fn enroll(api: FakeServerApi, name: &str) -> TestClient {
  PendingEnrollment::new()
    .complete(
      api,
      Arc::new(MemoryStore::new()),
      Box::new(ManualClock::new(1_800_000_000_000)),
      ClientSettings {
        pending_addition_expiry: std::time::Duration::MAX,
        creates_notes_to_self: false,
        ..ClientSettings::default()
      },
      EnrollmentOptions {
        device_name: name.into(),
        secret: EnrollmentSecret::Passphrase(SecretBytes::new(b"correct horse battery".to_vec())),
        argon2: Argon2Settings {
          memory_kib: 64,
          iterations: 1,
          parallelism: 1,
        },
        maybe_attestation: None,
      },
    )
    .await
    .unwrap()
}

fn export_dir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!(
    "teamcraft-import-synthetic-{name}-{}",
    std::process::id()
  ));
  drop(std::fs::remove_dir_all(&dir));
  std::fs::create_dir_all(dir.join("raw/uploads/1")).unwrap();
  std::fs::create_dir_all(dir.join("stickers")).unwrap();
  write(&dir, "channels.ndjson", CHANNELS);
  write(&dir, "posts.ndjson", POSTS);
  write(
    &dir,
    "users.ndjson",
    &[
      r#"{"id":7,"username":"a","deleted":false}"#,
      r#"{"id":8,"username":"b","deleted":false}"#,
      r#"{"id":9,"username":"c","deleted":true}"#,
    ],
  );
  write(
    &dir,
    "bots.ndjson",
    &[r#"{"id":50,"name":"Jenkins","kind":"webhook_incoming"}"#],
  );
  write(
    &dir,
    "reactions.ndjson",
    &[
      r#"{"post_id":100,"channel_id":1,"user_id":8,"name":"thumbsup"}"#,
      r#"{"post_id":100,"channel_id":1,"user_id":9,"name":"thumbsup"}"#,
    ],
  );
  write(
    &dir,
    "reads.ndjson",
    &[r#"{"channel_id":1,"user_id":7,"last_view_at":1600000003500}"#],
  );
  write(&dir, "favorites.ndjson", &[r#"{"channel_id":1,"user_id":7}"#]);
  write(
    &dir,
    "notifications.ndjson",
    &[r#"{"channel_id":1,"user_id":8,"desktop":"all","mute":true}"#],
  );
  write(
    &dir,
    "webhooks_incoming.ndjson",
    &[r#"{"id":50,"name":"Jenkins","channel_id":7,"disabled":false,"create_at":1}"#],
  );
  std::fs::write(dir.join("raw/uploads/1/102"), b"%PDF-").unwrap();
  // A one-pixel PNG for the sticker.
  std::fs::write(dir.join("stickers/dog_01.png"), PIXEL_PNG).unwrap();
  std::fs::write(dir.join("channels.csv"), CHANNEL_MAPPING).unwrap();
  std::fs::write(dir.join("users.csv"), USER_MAPPING).unwrap();
  dir
}

fn write(dir: &Path, name: &str, lines: &[&str]) {
  let mut file = std::fs::File::create(dir.join(name)).unwrap();
  for line in lines {
    writeln!(file, "{line}").unwrap();
  }
}

const PIXEL_PNG: &[u8] = &[
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
  0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
  0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xff, 0xff, 0x3f, 0x00, 0x05, 0xfe, 0x02, 0xfe, 0xa7,
  0x35, 0x81, 0x84, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
