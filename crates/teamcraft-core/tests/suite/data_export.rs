//! "Export my data" (issue #211) against the fake server: the archive holds every message the
//! user can read, down to the beginning of the history the device didn't have, with threads,
//! reactions and the user's files; it is a valid ZIP with checksums; it waits out a network
//! failure and goes on after an interruption from its checkpoint.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::common::{Peer, settings, settle};
use serde_json::Value;
use teamcraft_client_api::data_export::{DataExportLabels, DataExportOptions, DataExportScope};
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::OutgoingFile;
use teamcraft_core::export::{ExportJob, ExportSink, ExportStep, MESSAGES_PER_PART, MemorySink};
use teamcraft_core::store::LocalStore;
use teamcraft_crypto::checksum::FileChecksum;
use teamcraft_proto::{ConversationId, HistoryPolicy, Id, ThreadId};

#[tokio::test]
async fn the_archive_holds_the_whole_history_with_threads_reactions_and_own_files() {
  let (server, mut alice, mut bob, channel) = bob_joins_a_busy_channel().await;
  let root = alice.client.send_text(channel, None, "a question").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let thread = thread_of(&bob, channel, root);
  bob
    .client
    .send_text(channel, Some(thread), "an answer")
    .await
    .unwrap();
  bob.client.react(channel, root, "👍", true).await.unwrap();
  bob
    .client
    .send_file(channel, None, file("notes.txt", b"bob's notes"))
    .await
    .unwrap();
  alice
    .client
    .send_file(channel, None, file("alice.txt", b"alice's file"))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  server.add_security_log_entry(bob.client.user_id(), "device_revoked");
  assert!(
    !bob.texts(channel).contains(&"old 1".to_owned()),
    "Bob's device doesn't hold the start of the history"
  );

  let archive = export(
    &mut bob,
    DataExportOptions {
      scope: DataExportScope::All,
      includes_files: true,
    },
  )
  .await;

  let lines = archive.messages(channel);
  let texts: Vec<&str> = lines.iter().filter_map(|line| line["text"].as_str()).collect();
  let expected: Vec<String> = (1..=30).map(|number| format!("old {number}")).collect();
  for text in &expected {
    assert!(texts.contains(&text.as_str()), "{text} is missing: {texts:?}");
  }
  assert!(texts.contains(&"a question") && texts.contains(&"an answer"));
  let answer = lines.iter().find(|line| line["text"] == "an answer").unwrap();
  assert_eq!(answer["thread_id"], Value::String(thread.to_string()));
  assert_eq!(answer["is_own"], true);
  let question = lines.iter().find(|line| line["text"] == "a question").unwrap();
  assert_eq!(question["reactions"][0]["emoji"], "👍");
  assert_eq!(question["is_own"], false);
  assert_eq!(question["sender_name"], "Alice");

  // Bob's own file is in the archive; Alice's is listed by name only.
  let own = lines
    .iter()
    .find(|line| line["files"][0]["name"] == "notes.txt")
    .unwrap();
  let path = own["files"][0]["path"].as_str().unwrap();
  assert_eq!(archive.file(path), b"bob's notes");
  let other = lines
    .iter()
    .find(|line| line["files"][0]["name"] == "alice.txt")
    .unwrap();
  assert!(other["files"][0].get("path").is_none());

  let manifest = archive.json("teamcraft-export/manifest.json");
  assert_eq!(manifest["format"], "teamcraft-data-export");
  assert_eq!(manifest["version"], 1);
  assert_eq!(manifest["counts"]["files"], 1);
  assert_eq!(manifest["counts"]["messages"], lines.len());
  let personal = archive.json("teamcraft-export/account/personal-data.json");
  assert_eq!(personal["account"]["id"], bob.client.user_id().to_string());
  let log = archive.text("teamcraft-export/account/security-log.ndjson");
  assert!(log.contains("device_revoked"));
  let index = archive.text("teamcraft-export/index.html");
  assert!(index.contains("busy"), "the index lists the channel");
  let conversations = archive.json("teamcraft-export/conversations.json");
  assert!(
    conversations
      .as_array()
      .unwrap()
      .iter()
      .any(|summary| summary["id"] == channel.to_string() && summary["messages"] == lines.len())
  );
  assert!(
    !archive.names.iter().any(|name| name.contains("key")),
    "no key file: {:?}",
    archive.names
  );
  let checkpoint = ExportJob::stored(&bob.store).unwrap();
  assert!(
    checkpoint.is_none(),
    "the checkpoint is gone once the archive is complete"
  );
}

#[tokio::test]
async fn the_own_scope_holds_only_what_the_user_wrote() {
  let (_server, mut alice, mut bob, channel) = bob_joins_a_busy_channel().await;
  bob.client.send_text(channel, None, "mine").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let archive = export(&mut bob, DataExportOptions::default()).await;

  let lines = archive.messages(channel);
  assert_eq!(lines.len(), 1);
  assert_eq!(lines[0]["text"], "mine");
  assert!(lines.iter().all(|line| line["is_own"] == true));
}

#[tokio::test]
async fn a_network_failure_waits_and_the_export_goes_on() {
  let (server, mut alice, mut bob, channel) = bob_joins_a_busy_channel().await;
  settle(&mut [&mut alice, &mut bob]).await;
  let mut job = start(
    &bob,
    DataExportOptions {
      scope: DataExportScope::All,
      includes_files: false,
    },
  );
  let mut sink = MemorySink::default();

  server.set_rest_failing(bob.client.user_id(), true);
  let waited = job.step(&mut bob.client, &mut sink).await.unwrap();
  assert!(matches!(waited, ExportStep::RetryAt(_)));
  assert!(job.retry_at_ms().is_some());
  server.set_rest_failing(bob.client.user_id(), false);
  job.network_back();

  run(&mut job, &mut bob, &mut sink).await;
  let archive = Archive::read(sink.bytes);
  let texts = archive.texts(channel);
  assert_eq!(texts.len(), 30, "nothing was lost to the failure: {texts:?}");
}

#[tokio::test]
async fn an_interrupted_export_goes_on_from_its_checkpoint() {
  let (_server, mut alice, mut bob, channel) = bob_joins_a_busy_channel().await;
  settle(&mut [&mut alice, &mut bob]).await;
  let options = DataExportOptions {
    scope: DataExportScope::All,
    includes_files: false,
  };
  let mut job = start(&bob, options);
  let mut sink = MemorySink::default();
  // Some entries written, then the app stops while a part of messages is half written.
  let mut is_in_a_part = false;
  for _ in 0..50 {
    job.step(&mut bob.client, &mut sink).await.unwrap();
    let saved = ExportJob::stored(&bob.store)
      .unwrap()
      .map_or(0, |checkpoint| checkpoint.len());
    if saved > 0 && (sink.bytes.len() as u64) > saved {
      is_in_a_part = true;
      break;
    }
  }
  assert!(is_in_a_part, "the export never stopped in the middle of an entry");
  let written = sink.bytes.len();
  drop(job);

  let checkpoint = ExportJob::stored(&bob.store).unwrap().unwrap();
  assert!(!checkpoint.is_empty() && (checkpoint.len() as usize) < written);
  assert_eq!(checkpoint.target, "target");
  // The host cuts its file back to the checkpoint, then goes on.
  sink.bytes.truncate(checkpoint.len() as usize);
  let mut resumed = ExportJob::resume(checkpoint);
  run(&mut resumed, &mut bob, &mut sink).await;

  let archive = Archive::read(sink.bytes);
  let texts = archive.texts(channel);
  let mut unique = texts.clone();
  unique.sort();
  unique.dedup();
  assert_eq!(texts.len(), 30, "each message once: {texts:?}");
  assert_eq!(unique.len(), 30);
  let manifest = archive.json("teamcraft-export/manifest.json");
  assert_eq!(
    manifest["counts"]["messages"],
    archive.messages(channel).len(),
    "the counts of the part written again aren't doubled"
  );
}

#[tokio::test]
async fn a_long_conversation_is_cut_into_parts_and_every_message_is_kept() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "long")
    .await
    .unwrap();
  settle(&mut [&mut alice]).await;
  let count = MESSAGES_PER_PART + 150;
  for number in 0..count {
    alice
      .client
      .send_text(channel, None, &format!("message {number}"))
      .await
      .unwrap();
  }

  let archive = export(&mut alice, DataExportOptions::default()).await;

  let parts: Vec<&String> = archive
    .names
    .iter()
    .filter(|name| name.contains(&channel.to_string()) && name.ends_with(".ndjson"))
    .collect();
  assert_eq!(parts.len(), 2, "{parts:?}");
  let lines = archive.messages(channel);
  assert_eq!(lines.len() as u64, count);
  let texts: Vec<&str> = lines.iter().map(|line| line["text"].as_str().unwrap()).collect();
  assert_eq!(texts.first(), Some(&"message 0"));
  assert_eq!(
    texts.last().copied(),
    Some(format!("message {}", count - 1).as_str())
  );
}

/// Alice's channel with 30 older messages, which Bob joins with the full history; his device holds
/// only the newest page and reads pages of five.
async fn bob_joins_a_busy_channel() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let small_pages = ClientSettings {
    history_page_size: 5,
    ..settings()
  };
  let mut bob = Peer::enroll_with_settings(&server, "Bob", small_pages).await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "busy")
    .await
    .unwrap();
  for number in 1..=30 {
    alice
      .client
      .send_text(channel, None, &format!("old {number}"))
      .await
      .unwrap();
  }
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  (server, alice, bob, channel)
}

fn thread_of(peer: &Peer, conversation: ConversationId, root: teamcraft_proto::ClientMessageId) -> ThreadId {
  let stored = peer.store.message(conversation, root).unwrap().unwrap();
  ThreadId::from_snowflake(stored.maybe_server_id.unwrap().snowflake())
}

fn file(name: &str, bytes: &[u8]) -> OutgoingFile {
  OutgoingFile {
    file_name: name.into(),
    media_type: "text/plain".into(),
    content: bytes.to_vec(),
  }
}

fn start(peer: &Peer, options: DataExportOptions) -> ExportJob {
  ExportJob::start(&peer.client, options, labels(), "target".into()).unwrap()
}

fn labels() -> DataExportLabels {
  DataExportLabels {
    language: "en".into(),
    title: "Your TeamCraft data".into(),
    made_on: "Made on {date}".into(),
    ..DataExportLabels::default()
  }
}

async fn export(peer: &mut Peer, options: DataExportOptions) -> Archive {
  let mut job = start(peer, options);
  let mut sink = MemorySink::default();
  run(&mut job, peer, &mut sink).await;
  Archive::read(sink.bytes)
}

async fn run(job: &mut ExportJob, peer: &mut Peer, sink: &mut dyn ExportSink) {
  for _ in 0..100_000 {
    match job.step(&mut peer.client, sink).await.unwrap() {
      ExportStep::Done => return,
      ExportStep::Continue => {},
      ExportStep::RetryAt(_) => panic!("the fake server doesn't fail here"),
    }
  }
  panic!("the export didn't end");
}

/// An archive read back with a ZIP reader that isn't ours; every checksum of the manifest checked.
struct Archive {
  names: Vec<String>,
  files: HashMap<String, Vec<u8>>,
}

impl Archive {
  fn read(bytes: Vec<u8>) -> Self {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut names = Vec::new();
    let mut files = HashMap::new();
    for index in 0..zip.len() {
      let mut entry = zip.by_index(index).unwrap();
      let name = entry.name().unwrap().into_owned();
      let mut content = Vec::new();
      entry.read_to_end(&mut content).unwrap();
      names.push(name.clone());
      files.insert(name, content);
    }
    let archive = Self { names, files };
    let manifest = archive.json("teamcraft-export/manifest.json");
    for listed in manifest["files"].as_array().unwrap() {
      let path = listed["path"].as_str().unwrap();
      let mut checksum = FileChecksum::new();
      checksum.update(archive.file(path));
      assert_eq!(
        checksum.finish_hex(),
        listed["sha256"].as_str().unwrap(),
        "{path}"
      );
      assert_eq!(archive.file(path).len() as u64, listed["size"].as_u64().unwrap());
    }
    assert_eq!(
      manifest["files"].as_array().unwrap().len() + 1,
      archive.names.len(),
      "the manifest lists every other file"
    );
    archive
  }

  fn file(&self, path: &str) -> &[u8] {
    self
      .files
      .get(path)
      .unwrap_or_else(|| panic!("no {path} in {:?}", self.names))
  }

  fn text(&self, path: &str) -> String {
    String::from_utf8(self.file(path).to_vec()).unwrap()
  }

  fn json(&self, path: &str) -> Value {
    serde_json::from_slice(self.file(path)).unwrap()
  }

  /// The texts of the conversation's messages, in order (events have none).
  fn texts(&self, conversation: ConversationId) -> Vec<String> {
    self
      .messages(conversation)
      .iter()
      .filter_map(|line| line["text"].as_str().map(str::to_owned))
      .collect()
  }

  /// Every line of the conversation's parts, in order.
  fn messages(&self, conversation: ConversationId) -> Vec<Value> {
    let prefix = format!("teamcraft-export/conversations/{conversation}/");
    let mut parts: Vec<&String> = self
      .names
      .iter()
      .filter(|name| name.starts_with(&prefix))
      .collect();
    parts.sort();
    parts
      .into_iter()
      .flat_map(|part| {
        self
          .text(part)
          .lines()
          .map(|line| serde_json::from_str::<Value>(line).unwrap())
          .collect::<Vec<_>>()
      })
      .collect()
  }
}
