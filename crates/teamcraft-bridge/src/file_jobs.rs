//! The worker's side of webhooks with files (docs/bridge.md, "Files"): uploads the files of one
//! message after another with the core's direct uploads, each file encrypted with its own key, and
//! hands the message on once all of its files are up. A failed file gives the upload up at the
//! server and answers the waiting request; its message is never sent, so there is no message with
//! files missing.
//!
//! The bytes go up beside the worker's loop ([`FileJobs::sent`] in its `select!`), so logs,
//! webhooks and the gateway keep going while a large file uploads. Messages with files wait in
//! line, at most [`MAX_WAITING_FILE_MESSAGES`]; one is uploaded at a time.

use std::collections::VecDeque;
use std::sync::Arc;

use teamcraft_core::Client;
use teamcraft_core::api::{ApiError, ServerApi};
use teamcraft_core::client::direct_uploads::{DirectUpload, DirectUploadError, DirectUploadStep};
use teamcraft_core::client::files::UploadSource;
use teamcraft_core::store::{AttachmentRef, LocalStore};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::sources::Source;
use crate::spool::SpooledFile;

/// Messages with files waiting for their upload; more are refused (`503`).
pub const MAX_WAITING_FILE_MESSAGES: usize = 32;

/// A webhook message with files, from the endpoint. Its files are deleted from the spool once it
/// is dropped: after its upload, or when it is refused.
#[derive(Debug)]
pub struct FilesMessage {
  pub source: Arc<Source>,
  pub text: String,
  pub is_alert: bool,
  pub files: Vec<SpooledFile>,
  /// The endpoint waits for this answer; it is gone when the producer hung up.
  pub reply: oneshot::Sender<Result<(), FilesFailure>>,
}

/// Why the files of a message did not go up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesFailure {
  /// A file is larger than the server takes (its attachment limit or the bot's storage).
  TooLarge,
  /// The server can't be reached.
  Unreachable,
  /// The server or the storage refused or failed an upload.
  Failed,
  /// A spooled file could not be read.
  LocalFile,
  /// Too many messages with files wait already.
  Busy,
  /// The files are up, but the bridge's store could not keep their message.
  NotQueued,
}

/// A message whose files are all up: what the worker sends.
#[derive(Debug)]
pub struct UploadedMessage {
  pub source: Arc<Source>,
  pub text: String,
  pub is_alert: bool,
  pub attachments: Vec<AttachmentRef>,
  pub reply: oneshot::Sender<Result<(), FilesFailure>>,
}

/// The messages with files: the one uploading and the ones waiting.
#[derive(Default)]
pub struct FileJobs {
  waiting: VecDeque<FilesMessage>,
  maybe_active: Option<ActiveJob>,
}

/// The message whose files go up now.
struct ActiveJob {
  message: FilesMessage,
  /// The file going up: `message.files[uploaded.len()]`.
  upload: DirectUpload,
  maybe_step: Option<DirectUploadStep>,
  uploaded: Vec<AttachmentRef>,
}

impl FileJobs {
  /// Lines the message up, or refuses it when too many wait.
  pub fn push(&mut self, message: FilesMessage) {
    if self.waiting.len() >= MAX_WAITING_FILE_MESSAGES {
      warn!(source = %message.source.name, "too many messages with files wait; refusing one");
      let _ = message.reply.send(Err(FilesFailure::Busy));
      return;
    }
    self.waiting.push_back(message);
  }

  /// Resolves once the bytes of the step on its way are sent; never while none is.
  pub async fn sent(&mut self) -> Result<(), ApiError> {
    match self.maybe_active.as_mut().and_then(|job| job.maybe_step.as_mut()) {
      Some(step) => step.sent().await,
      None => std::future::pending().await,
    }
  }

  /// Starts the next step when none is on its way: the next part or file of the active message,
  /// or the first file of the next message. Returns a message whose files are all up.
  pub async fn advance<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
  ) -> Option<UploadedMessage> {
    loop {
      if self
        .maybe_active
        .as_ref()
        .is_some_and(|job| job.maybe_step.is_some())
      {
        return None;
      }
      if self.maybe_active.is_none() {
        self.maybe_active = Some(self.next_job()?);
      }
      let job = self.maybe_active.as_mut()?;
      if job.message.reply.is_closed() {
        debug!(source = %job.message.source.name, "the producer hung up; its files are dropped");
        self.give_up(client, None).await;
        continue;
      }
      match client.start_direct_upload(&mut job.upload).await {
        Ok(step) => job.maybe_step = Some(step),
        Err(error) => self.give_up(client, Some(&error)).await,
      }
    }
  }

  /// Completes the step whose bytes were sent ([`Self::sent`] resolved). Returns a message whose
  /// files are all up.
  pub async fn finish_step<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sent: Result<(), ApiError>,
  ) -> Option<UploadedMessage> {
    let job = self.maybe_active.as_mut()?;
    let step = job.maybe_step.take()?;
    match client.finish_direct_upload(&mut job.upload, step, sent).await {
      // A part is stored; the next one follows.
      Ok(None) => None,
      Ok(Some(attachment)) => {
        job.uploaded.push(attachment);
        match job.message.files.get(job.uploaded.len()) {
          Some(next) => {
            job.upload = direct_upload(&job.message.source, next);
            None
          },
          None => self.maybe_active.take().map(ActiveJob::into_uploaded),
        }
      },
      Err(error) => {
        self.give_up(client, Some(&error)).await;
        None
      },
    }
  }

  /// Gives up whatever is under way, at shutdown.
  pub async fn abandon_all<A: ServerApi, S: LocalStore>(&mut self, client: &Client<A, S>) {
    if let Some(job) = self.maybe_active.take() {
      client.abandon_direct_upload(&job.upload).await;
    }
    self.waiting.clear();
  }

  /// The next waiting message whose producer still waits for the answer.
  fn next_job(&mut self) -> Option<ActiveJob> {
    while let Some(message) = self.waiting.pop_front() {
      if message.reply.is_closed() {
        debug!(source = %message.source.name, "the producer hung up before its files went up");
        continue;
      }
      let Some(first) = message.files.first() else {
        // The endpoint hands on only messages with files; nothing to upload is nothing to send.
        let _ = message.reply.send(Err(FilesFailure::LocalFile));
        continue;
      };
      let upload = direct_upload(&message.source, first);
      return Some(ActiveJob {
        message,
        upload,
        maybe_step: None,
        uploaded: Vec::new(),
      });
    }
    None
  }

  /// Ends the active message without sending it: gives its upload up and answers with `error`
  /// (none when nobody waits for the answer any more).
  async fn give_up<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    maybe_error: Option<&DirectUploadError>,
  ) {
    let Some(job) = self.maybe_active.take() else {
      return;
    };
    client.abandon_direct_upload(&job.upload).await;
    if let Some(error) = maybe_error {
      warn!(source = %job.message.source.name, %error, "uploading a webhook's files failed");
      let _ = job.message.reply.send(Err(failure_of(error)));
    }
    // The files already up stay at the server without a message: nothing carries their keys.
  }
}

impl ActiveJob {
  fn into_uploaded(self) -> UploadedMessage {
    let FilesMessage {
      source,
      text,
      is_alert,
      reply,
      ..
    } = self.message;
    UploadedMessage {
      source,
      text,
      is_alert,
      attachments: self.uploaded,
      reply,
    }
  }
}

fn direct_upload(source: &Source, file: &SpooledFile) -> DirectUpload {
  DirectUpload::new(
    source.conversation,
    UploadSource::Path {
      // Spool paths are UTF-8: the configuration checks the spool directory, the names are ASCII.
      path: file.path().to_string_lossy().into_owned(),
    },
    file.file_name.clone(),
    file.media_type.clone(),
    file.size_bytes,
  )
}

fn failure_of(error: &DirectUploadError) -> FilesFailure {
  match error {
    DirectUploadError::TooLarge => FilesFailure::TooLarge,
    DirectUploadError::Offline(_) => FilesFailure::Unreachable,
    DirectUploadError::Failed(_) => FilesFailure::Failed,
    DirectUploadError::LocalFile => FilesFailure::LocalFile,
  }
}

/// The whole way through, against the core's fake server: a form arrives at the endpoint, the
/// worker uploads its files, and a member of the channel decrypts the message and the files.
#[cfg(test)]
mod tests {
  use teamcraft_core::ClientSettings;
  use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
  use teamcraft_core::clock::SystemClock;
  use teamcraft_core::store::{MemoryStore, MessageContent, MessageFilter};
  use teamcraft_core::{EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
  use teamcraft_crypto::backup::Argon2Settings;
  use teamcraft_crypto::secret::SecretBytes;
  use teamcraft_proto::ConversationId;
  use tokio::sync::mpsc;

  use super::*;
  use crate::app::start_endpoints;
  use crate::config::BridgeConfig;
  use crate::multipart::tests::form;
  use crate::sources::SourceRegistry;
  use crate::worker::{Worker, WorkerSettings};

  type TestClient = Client<FakeServerApi, Arc<MemoryStore>>;

  const TOKEN: &str = "files-token-0123456789";
  const FORM_TYPE: &str = "multipart/form-data; boundary=XyZ";

  #[tokio::test]
  async fn the_files_of_a_webhook_arrive_encrypted_in_one_message() {
    let (_server, mut alice, mut gateway, bot, channel) = channel_with_bot().await;
    // Larger than one part (8 MiB): it goes up in parts.
    let large: Vec<u8> = (0..9 * 1024 * 1024).map(|index| (index % 251) as u8).collect();
    let body = form(&[
      (
        "payload_json",
        None,
        None,
        br#"{"text":"Nightly build","username":"ci"}"#.as_slice(),
      ),
      ("files[0]", Some("notes.txt"), Some("text/plain"), b"all green"),
      ("files[1]", Some("build.tar"), None, &large),
    ]);
    let answer = run_bridge(bot, channel, body).await;
    assert_eq!(answer, (reqwest::StatusCode::OK, "ok".to_owned()));

    let (text, files) = received_files(&mut alice, &mut gateway, channel).await;
    assert_eq!(text, "**ci**\nNightly build");
    let names: Vec<_> = files.iter().map(|file| file.file_name.as_str()).collect();
    assert_eq!(names, ["notes.txt", "build.tar"]);
    let notes = alice.open_attachment(&files[0]).await.unwrap();
    assert_eq!(notes.expose_secret(), b"all green");
    let build = alice.open_attachment(&files[1]).await.unwrap();
    assert_eq!(build.expose_secret(), large.as_slice());
  }

  #[tokio::test]
  async fn a_file_the_server_refuses_leaves_no_message() {
    let (server, mut alice, mut gateway, bot, channel) = channel_with_bot().await;
    server.set_max_attachment_bytes(2048);
    let body = form(&[
      ("text", None, None, b"two files".as_slice()),
      ("file", Some("small.txt"), None, b"fits"),
      ("file", Some("big.bin"), None, &[7; 4096]),
    ]);
    let (status, answer) = run_bridge(bot, channel, body).await;
    assert_eq!(status, reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert!(answer.contains("file_too_large"), "{answer}");
    alice.pump(&mut gateway).await.unwrap();
    assert!(
      !texts(&alice, channel).iter().any(|text| text == "two files"),
      "no message with a file missing"
    );
  }

  /// Starts the bridge for the bot, posts `body` as a webhook with files, waits for the answer and
  /// stops the bridge again.
  async fn run_bridge(
    bot: TestClient,
    channel: ConversationId,
    body: Vec<u8>,
  ) -> (reqwest::StatusCode, String) {
    let spool = std::env::temp_dir().join(format!("teamcraft-bridge-jobs-{}-{channel}", std::process::id()));
    let config = BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [listen]
      addrs = ["127.0.0.1:0"]
      [files]
      spool_dir = "{spool}"
      [app_sources]
      enabled = false
      [[sources]]
      name = "ci"
      token = "{TOKEN}"
      conversation_id = "{channel}"
      "#,
      spool = spool.display()
    ))
    .unwrap();
    let (sender, receiver) = mpsc::channel(16);
    let sources = SourceRegistry::new(&config);
    let endpoints = start_endpoints(&config, sources.clone(), sender).await.unwrap();
    let url = format!("http://{}/hooks/{TOKEN}", endpoints.addrs[0]);
    let settings = WorkerSettings {
      batch: config.batch_limits(),
      index_interval: None,
      maybe_app_sources: None,
    };
    let worker = Worker::new(bot, sources, receiver, settings);
    let script = async {
      let response = reqwest::Client::new()
        .post(url)
        .header("content-type", FORM_TYPE)
        .body(body)
        .send()
        .await
        .unwrap();
      let answer = (response.status(), response.text().await.unwrap());
      endpoints.shutdown().await;
      answer
    };
    let (_, answer) = tokio::join!(worker.run(), script);
    let is_spool_empty = std::fs::read_dir(&spool).map_or(true, |mut entries| entries.next().is_none());
    assert!(is_spool_empty, "the spooled files are deleted");
    answer
  }

  /// The text and the files of the first message with files that reaches `client`.
  async fn received_files(
    client: &mut TestClient,
    gateway: &mut FakeGateway,
    channel: ConversationId,
  ) -> (String, Vec<AttachmentRef>) {
    for _ in 0..50 {
      client.pump(gateway).await.unwrap();
      let found = client
        .store()
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .into_iter()
        .find_map(|message| match message.content {
          MessageContent::Text { body, meta, .. } if !meta.attachments.is_empty() => {
            Some((body, meta.attachments))
          },
          _ => None,
        });
      if let Some(found) = found {
        return found;
      }
    }
    panic!("no message with files arrived");
  }

  fn texts(client: &TestClient, channel: ConversationId) -> Vec<String> {
    client
      .store()
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter_map(|message| match message.content {
        MessageContent::Text { body, .. } => Some(body),
        _ => None,
      })
      .collect()
  }

  /// alice's channel with the bot in it; the bot has joined its encryption group.
  async fn channel_with_bot() -> (FakeServer, TestClient, FakeGateway, TestClient, ConversationId) {
    let server = FakeServer::new();
    let mut alice = enroll(server.add_user("alice")).await;
    let mut bot = enroll(server.add_user("bot")).await;
    let (mut alice_gateway, _) = alice.connect_gateway().await.unwrap();
    let (mut bot_gateway, _) = bot.connect_gateway().await.unwrap();
    let space = alice.api().list_spaces().await.unwrap().spaces[0].id;
    let channel = alice.create_channel(space, "builds").await.unwrap();
    alice.add_member(channel, bot.user_id()).await.unwrap();
    for _ in 0..50 {
      bot.pump(&mut bot_gateway).await.unwrap();
      alice.pump(&mut alice_gateway).await.unwrap();
      if bot.has_group(channel).unwrap() {
        return (server, alice, alice_gateway, bot, channel);
      }
    }
    panic!("the bot did not join");
  }

  async fn enroll(api: FakeServerApi) -> TestClient {
    let options = EnrollmentOptions {
      device_name: "test".into(),
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(b"passphrase".to_vec())),
      // Cheap Argon2id so the test stays fast.
      argon2: Argon2Settings {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
      },
      maybe_attestation: None,
    };
    PendingEnrollment::new()
      .complete(
        api,
        Arc::new(MemoryStore::new()),
        Box::new(SystemClock),
        ClientSettings::default(),
        options,
      )
      .await
      .unwrap()
  }
}
