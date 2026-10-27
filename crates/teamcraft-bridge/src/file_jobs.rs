//! The worker's side of webhooks with files (docs/bridge.md, "Files"): uploads the files of one
//! message after another with the core's direct uploads, each file encrypted with its own key, and
//! hands the message on once all of its files are up. The uploads are completed only then, all
//! together: until that moment each is an unfinished upload, so a failed file, or a producer who
//! hung up, gives all of them up and the server frees them. The message is never sent then, so
//! there is no message with files missing and no file without its message.
//!
//! The bytes go up beside the worker's loop ([`FileJobs::sent`] in its `select!`), so logs,
//! webhooks and the gateway keep going while a large file uploads. Messages with files wait in
//! line (no more than the endpoint lets run at once); one is uploaded at a time.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use teamcraft_core::Client;
use teamcraft_core::api::{ApiError, ServerApi};
use teamcraft_core::client::direct_uploads::{
  DirectUpload, DirectUploadError, DirectUploadProgress, DirectUploadStep,
};
use teamcraft_core::client::files::UploadSource;
use teamcraft_core::store::{AttachmentRef, LocalStore};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::idempotency::KeyGuard;
use crate::sources::{FileCharge, Source};
use crate::spool::SpooledFile;

/// A webhook message with files, from the endpoint. Its files are deleted from the spool, and its
/// bytes given back unless its upload began, once it is dropped: after its upload, or when it is
/// refused.
#[derive(Debug)]
pub struct FilesMessage {
  pub source: Arc<Source>,
  pub text: String,
  pub is_alert: bool,
  pub files: Vec<SpooledFile>,
  /// What the request took from the byte budgets.
  pub charge: FileCharge,
  /// The endpoint waits for this answer; it is gone when the producer hung up.
  pub reply: FilesReply,
}

/// The answer to a message with files, and the idempotency key of its request: the key is kept when
/// the answer says the message was sent, also to a producer that no longer waits (its retry must
/// not post it again), and freed with any other answer or none, since nothing was posted then.
#[derive(Debug)]
pub struct FilesReply {
  sender: oneshot::Sender<Result<(), FilesFailure>>,
  maybe_key: Option<KeyGuard>,
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
  /// The bot has no encryption group for the channel yet (a welcome on its way): the message
  /// couldn't be sent, so nothing is uploaded.
  ChannelNotReady,
}

/// A message whose files are all up: what the worker sends.
#[derive(Debug)]
pub struct UploadedMessage {
  pub source: Arc<Source>,
  pub text: String,
  pub is_alert: bool,
  pub attachments: Vec<AttachmentRef>,
  pub reply: FilesReply,
}

/// The messages with files: the one uploading and the ones waiting.
pub struct FileJobs {
  waiting: VecDeque<FilesMessage>,
  maybe_active: Option<ActiveJob>,
  capacity: usize,
}

/// The message whose files go up now.
struct ActiveJob {
  message: FilesMessage,
  /// Indices into `message.files` still to go up, the next one first: the largest files first, so
  /// the small ones (sent whole, and swept sooner by the server while unfinished) come last.
  queued: VecDeque<usize>,
  /// The file going up now and its upload.
  current: (usize, DirectUpload),
  maybe_step: Option<DirectUploadStep>,
  /// Files whose bytes are all up, waiting to be completed together.
  sent: Vec<(usize, DirectUpload)>,
}

impl FilesReply {
  pub fn new(sender: oneshot::Sender<Result<(), FilesFailure>>, maybe_key: Option<KeyGuard>) -> Self {
    Self { sender, maybe_key }
  }

  /// Answers the producer, keeping the key if the message was sent. `Err` gives the answer back
  /// when the producer no longer waits.
  pub fn send(self, answer: Result<(), FilesFailure>) -> Result<(), Result<(), FilesFailure>> {
    let Self { sender, maybe_key } = self;
    if let (Ok(()), Some(key)) = (&answer, maybe_key) {
      key.accept(Instant::now());
    }
    sender.send(answer)
  }

  /// Whether the producer hung up.
  pub fn is_closed(&self) -> bool {
    self.sender.is_closed()
  }
}

impl FileJobs {
  /// Room for `capacity` messages waiting.
  pub fn new(capacity: usize) -> Self {
    Self {
      waiting: VecDeque::new(),
      maybe_active: None,
      capacity,
    }
  }

  /// Lines the message up, or refuses it when too many wait.
  pub fn push(&mut self, message: FilesMessage) {
    if self.waiting.len() >= self.capacity {
      warn!(source = %message.source.name, "too many messages with files wait; refusing one");
      let _ = message.reply.send(Err(FilesFailure::Busy));
      return;
    }
    self.waiting.push_back(message);
  }

  /// Drops the waiting messages whose producers hung up: their files leave the spool at once.
  pub fn drop_abandoned(&mut self) {
    self.waiting.retain(|message| {
      let is_waited_for = !message.reply.is_closed();
      if !is_waited_for {
        debug!(source = %message.source.name, "the producer hung up before its files went up");
      }
      is_waited_for
    });
  }

  /// Resolves once the bytes of the step on its way are sent; never while none is.
  pub async fn sent(&mut self) -> Result<(), ApiError> {
    match self.maybe_active.as_mut().and_then(|job| job.maybe_step.as_mut()) {
      Some(step) => step.sent().await,
      None => std::future::pending().await,
    }
  }

  /// Starts the next step when none is on its way: the next part or file of the active message,
  /// or the first file of the next message.
  pub async fn advance<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) {
    loop {
      if self
        .maybe_active
        .as_ref()
        .is_some_and(|job| job.maybe_step.is_some())
      {
        return;
      }
      if self.maybe_active.is_none() {
        let Some(job) = self.next_job(client) else {
          return;
        };
        self.maybe_active = Some(job);
      }
      let Some(job) = self.maybe_active.as_mut() else {
        return;
      };
      if job.message.reply.is_closed() {
        debug!(source = %job.message.source.name, "the producer hung up; its files are given up");
        self.give_up(client, None).await;
        continue;
      }
      match client.start_direct_upload(&mut job.current.1).await {
        Ok(step) => {
          // Bytes leave for the server now: what the request took stays taken.
          job.message.charge.keep();
          job.maybe_step = Some(step);
        },
        Err(error) => self.give_up(client, Some(&error)).await,
      }
    }
  }

  /// Records the step whose bytes were sent ([`Self::sent`] resolved). Returns a message whose
  /// files are all up and completed.
  pub async fn finish_step<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
    sent: Result<(), ApiError>,
  ) -> Option<UploadedMessage> {
    let job = self.maybe_active.as_mut()?;
    let step = job.maybe_step.take()?;
    match client.finish_direct_upload(&mut job.current.1, step, sent).await {
      Ok(DirectUploadProgress::MoreToSend) => None,
      Ok(DirectUploadProgress::AllSent) => {
        let Some(next) = job.queued.pop_front() else {
          return self.complete(client).await;
        };
        let upload = direct_upload(&job.message.source, &job.message.files[next]);
        let done = std::mem::replace(&mut job.current, (next, upload));
        job.sent.push(done);
        None
      },
      Err(error) => {
        self.give_up(client, Some(&error)).await;
        None
      },
    }
  }

  /// Gives up whatever is under way, at shutdown.
  pub async fn abandon_all<A: ServerApi, S: LocalStore>(&mut self, client: &Client<A, S>) {
    self.give_up(client, None).await;
    self.waiting.clear();
  }

  /// The next waiting message whose producer still waits and whose channel the bot can write in.
  fn next_job<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> Option<ActiveJob> {
    while let Some(message) = self.waiting.pop_front() {
      if message.reply.is_closed() {
        debug!(source = %message.source.name, "the producer hung up before its files went up");
        continue;
      }
      if !client.has_group(message.source.conversation).unwrap_or(false) {
        let _ = message.reply.send(Err(FilesFailure::ChannelNotReady));
        continue;
      }
      let mut order: Vec<usize> = (0..message.files.len()).collect();
      order.sort_by_key(|index| std::cmp::Reverse(message.files[*index].size_bytes));
      let mut queued = VecDeque::from(order);
      let Some(first) = queued.pop_front() else {
        // The endpoint hands on only messages with files; nothing to upload is nothing to send.
        let _ = message.reply.send(Err(FilesFailure::LocalFile));
        continue;
      };
      let upload = direct_upload(&message.source, &message.files[first]);
      return Some(ActiveJob {
        message,
        queued,
        current: (first, upload),
        maybe_step: None,
        sent: Vec::new(),
      });
    }
    None
  }

  /// Every file of the active message is up: completes them all, then hands the message on with
  /// its files in the order the producer sent them.
  async fn complete<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &mut Client<A, S>,
  ) -> Option<UploadedMessage> {
    let ActiveJob {
      message,
      current,
      mut sent,
      ..
    } = self.maybe_active.take()?;
    sent.push(current);
    let mut attachments: Vec<(usize, AttachmentRef)> = Vec::with_capacity(sent.len());
    for (index, upload) in &mut sent {
      match client.complete_direct_upload(upload).await {
        Ok(attachment) => attachments.push((*index, attachment)),
        Err(error) => {
          // Files completed before this one stay at the server without a message: the window is
          // the few requests of completing, not the whole upload.
          abandon(client, sent.iter().map(|(_, upload)| upload)).await;
          report(&message, &error);
          let _ = message.reply.send(Err(failure_of(&error)));
          return None;
        },
      }
    }
    attachments.sort_by_key(|(index, _)| *index);
    let FilesMessage {
      source,
      text,
      is_alert,
      reply,
      ..
    } = message;
    Some(UploadedMessage {
      source,
      text,
      is_alert,
      attachments: attachments
        .into_iter()
        .map(|(_, attachment)| attachment)
        .collect(),
      reply,
    })
  }

  /// Ends the active message without sending it: gives all of its uploads up and answers with the
  /// error (none when nobody waits for the answer any more).
  async fn give_up<A: ServerApi, S: LocalStore>(
    &mut self,
    client: &Client<A, S>,
    maybe_error: Option<&DirectUploadError>,
  ) {
    let Some(job) = self.maybe_active.take() else {
      return;
    };
    let uploads = std::iter::once(&job.current.1).chain(job.sent.iter().map(|(_, upload)| upload));
    abandon(client, uploads).await;
    if let Some(error) = maybe_error {
      report(&job.message, error);
      let _ = job.message.reply.send(Err(failure_of(error)));
    }
  }
}

async fn abandon<'a, A: ServerApi, S: LocalStore>(
  client: &Client<A, S>,
  uploads: impl Iterator<Item = &'a DirectUpload>,
) {
  for upload in uploads {
    client.abandon_direct_upload(upload).await;
  }
}

fn report(message: &FilesMessage, error: &DirectUploadError) {
  // The core's network errors name no URL, so no presigned one reaches the log.
  warn!(source = %message.source.name, %error, "uploading a webhook's files failed");
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
  .with_picture(file.maybe_thumbnail.clone(), file.maybe_dimensions)
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
  use crate::app::{WorkerStop, shut_down, start_endpoints, worker_settings};
  use crate::config::BridgeConfig;
  use crate::idempotency::{IDEMPOTENCY_KEY_HEADER, IDEMPOTENT_REPLAYED_HEADER};
  use crate::multipart::tests::form;
  use crate::sources::SourceRegistry;
  use crate::spool::SpoolDir;
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
  async fn a_retried_webhook_with_files_is_sent_once() {
    let (server, mut alice, mut gateway, bot, channel) = channel_with_bot().await;
    let body = form(&[
      ("text", None, None, b"Nightly report".as_slice()),
      ("file", Some("report.txt"), Some("text/plain"), b"all green"),
    ]);
    // The retry comes from a new process: another boundary, the same key and content.
    let retry = String::from_utf8(body.clone())
      .unwrap()
      .replace("--XyZ", "--Retry")
      .into_bytes();
    let answers = run_bridge_requests(
      bot,
      channel,
      vec![
        (FORM_TYPE.to_owned(), Some("nightly-1"), body),
        (
          "multipart/form-data; boundary=Retry".to_owned(),
          Some("nightly-1"),
          retry,
        ),
      ],
    )
    .await;
    let ok = |is_replayed| (reqwest::StatusCode::OK, "ok".to_owned(), is_replayed);
    assert_eq!(answers, [ok(false), ok(true)]);
    let (text, files) = received_files(&mut alice, &mut gateway, channel).await;
    assert_eq!((text.as_str(), files.len()), ("Nightly report", 1));
    assert_eq!(
      texts(&alice, channel)
        .iter()
        .filter(|text| *text == "Nightly report")
        .count(),
      1,
      "one message"
    );
    assert_eq!(server.attachment_count(), 1, "the file went up once");
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
    assert_eq!(server.attachment_count(), 0, "and no file without its message");
  }

  mod hang_ups {
    use super::*;

    #[tokio::test]
    async fn a_message_whose_producer_hung_up_while_it_waited_uploads_nothing() {
      let (server, _alice, _gateway, mut bot, channel) = channel_with_bot().await;
      let (registry, source) = registry_for(channel);
      let spool = SpoolDir::new(scratch_dir("queued"), u64::MAX);
      let (message, answer) = files_message(&registry, &source, vec![spooled(&spool, "a.txt", b"a").await]);
      let mut jobs = FileJobs::new(4);
      jobs.push(message);
      drop(answer);
      jobs.drop_abandoned();
      jobs.advance(&mut bot).await;
      assert_eq!(server.attachment_count(), 0);
      assert_eq!(spool.used_bytes(), 0, "its files left the spool");
    }

    #[tokio::test]
    async fn a_producer_hanging_up_during_the_upload_gives_every_file_up() {
      let (server, _alice, _gateway, mut bot, channel) = channel_with_bot().await;
      let (registry, source) = registry_for(channel);
      let spool = SpoolDir::new(scratch_dir("active"), u64::MAX);
      let large: Vec<u8> = (0..9 * 1024 * 1024).map(|index| (index % 251) as u8).collect();
      let files = vec![
        spooled(&spool, "small.txt", b"small").await,
        spooled(&spool, "large.bin", &large).await,
      ];
      let (message, answer) = files_message(&registry, &source, files);
      let mut jobs = FileJobs::new(4);
      jobs.push(message);
      // The large file goes first; its first part goes up, then the producer hangs up.
      jobs.advance(&mut bot).await;
      let sent = jobs.sent().await;
      assert!(jobs.finish_step(&mut bot, sent).await.is_none());
      assert_eq!(server.attachment_count(), 1, "one unfinished upload");
      drop(answer);
      jobs.advance(&mut bot).await;
      assert_eq!(server.attachment_count(), 0, "given up at the server");
      assert_eq!(server.abandoned_attachments().len(), 1);
      assert_eq!(spool.used_bytes(), 0);
    }

    #[tokio::test]
    async fn a_channel_the_bot_cannot_write_in_yet_is_refused_before_any_upload() {
      let (server, mut alice, _gateway, mut bot, _channel) = channel_with_bot().await;
      // A channel the bot isn't in: it has no group for it.
      let space = alice.api().list_spaces().await.unwrap().spaces[0].id;
      let other = alice.create_channel(space, "elsewhere").await.unwrap();
      let (registry, source) = registry_for(other);
      let spool = SpoolDir::new(scratch_dir("no-group"), u64::MAX);
      let (message, answer) = files_message(&registry, &source, vec![spooled(&spool, "a.txt", b"a").await]);
      let mut jobs = FileJobs::new(4);
      jobs.push(message);
      jobs.advance(&mut bot).await;
      assert_eq!(answer.await.unwrap(), Err(FilesFailure::ChannelNotReady));
      assert_eq!(server.attachment_count(), 0);
    }
  }

  mod replies {
    use axum::http::{HeaderMap, HeaderValue};
    use teamcraft_crypto::fingerprint::Fingerprint;

    use super::*;
    use crate::config::IdempotencySection;
    use crate::idempotency::{IdempotencyKey, IdempotencyStore, KeyGuard, KeyUse};
    use crate::sources::SourceKey;

    #[test]
    fn a_sent_message_keeps_its_key_even_when_nobody_waits_and_any_other_end_frees_it() {
      let store = IdempotencyStore::new(&IdempotencySection::default());
      let source = SourceKey::Static("ci".into());
      let message = Fingerprint::of(b"the message");

      // Sent after the producer hung up: its retry must not post it again.
      let (sender, answer) = oneshot::channel();
      drop(answer);
      let reply = FilesReply::new(sender, Some(run(&store, &source, "sent", message)));
      assert!(reply.is_closed());
      assert!(reply.send(Ok(())).is_err(), "nobody waits");
      assert!(matches!(
        store.try_claim(&source, &key("sent"), message, Instant::now()),
        Ok(KeyUse::Replay)
      ));

      // Failed, or dropped unanswered (given up): nothing was posted, the key is free.
      let (sender, _answer) = oneshot::channel();
      let failed = FilesReply::new(sender, Some(run(&store, &source, "failed", message)));
      failed.send(Err(FilesFailure::Failed)).unwrap();
      let (sender, _answer) = oneshot::channel();
      drop(FilesReply::new(
        sender,
        Some(run(&store, &source, "dropped", message)),
      ));
      for free in ["failed", "dropped"] {
        assert!(
          matches!(
            store.try_claim(&source, &key(free), message, Instant::now()),
            Ok(KeyUse::Run(_))
          ),
          "{free}"
        );
      }
    }

    fn run(store: &IdempotencyStore, source: &SourceKey, name: &str, message: Fingerprint) -> KeyGuard {
      match store.try_claim(source, &key(name), message, Instant::now()) {
        Ok(KeyUse::Run(guard)) => guard,
        other => panic!("not a free key: {other:?}"),
      }
    }

    fn key(name: &str) -> IdempotencyKey {
      let mut headers = HeaderMap::new();
      headers.insert("idempotency-key", HeaderValue::from_str(name).unwrap());
      IdempotencyKey::from_headers(&headers).unwrap().unwrap()
    }
  }

  #[tokio::test]
  async fn a_stalled_producer_does_not_hold_up_the_shutdown() {
    let (_server, _alice, _gateway, bot, channel) = channel_with_bot().await;
    let config = bridge_config(channel, "drain_timeout_secs = 1");
    let (sender, receiver) = mpsc::channel(16);
    let sources = SourceRegistry::new(&config);
    let endpoints = start_endpoints(&config, sources.clone(), sender).await.unwrap();
    let address = endpoints.addrs[0];
    let worker = Worker::new(bot, sources, receiver, worker_settings(&config));
    let (stop, stopped) = WorkerStop::new();
    let script = async {
      // Headers and the start of a form, then nothing: the request stays open.
      let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
      let head = format!(
        "POST /hooks/{TOKEN} HTTP/1.1\r\nhost: bridge\r\ncontent-type: {FORM_TYPE}\r\n\
         content-length: 100000\r\n\r\n--XyZ\r\n"
      );
      tokio::io::AsyncWriteExt::write_all(&mut stream, head.as_bytes())
        .await
        .unwrap();
      let started = std::time::Instant::now();
      shut_down(endpoints, &stop, std::time::Duration::from_millis(300)).await;
      started.elapsed()
    };
    let (_, waited) = tokio::join!(worker.run_until(stopped), script);
    assert!(waited < std::time::Duration::from_secs(5), "{waited:?}");
  }

  /// The registry of a bridge with one source for `channel`, and that source.
  fn registry_for(channel: ConversationId) -> (SourceRegistry, Arc<Source>) {
    let config = bridge_config(channel, "");
    let registry = SourceRegistry::new(&config);
    let source = registry.find(TOKEN, crate::sources::Endpoint::Webhook).unwrap();
    (registry, source)
  }

  fn files_message(
    registry: &SourceRegistry,
    source: &Arc<Source>,
    files: Vec<SpooledFile>,
  ) -> (FilesMessage, oneshot::Receiver<Result<(), FilesFailure>>) {
    let (reply, answer) = oneshot::channel();
    let message = FilesMessage {
      source: source.clone(),
      text: "files".into(),
      is_alert: false,
      files,
      charge: FileCharge::new(registry.clone(), source.clone()),
      reply: FilesReply::new(reply, None),
    };
    (message, answer)
  }

  async fn spooled(spool: &SpoolDir, name: &str, content: &[u8]) -> SpooledFile {
    let mut writer = spool
      .create(name.into(), "application/octet-stream".into())
      .await
      .unwrap();
    writer.write(content).await.unwrap();
    writer.finish().await.unwrap()
  }

  fn scratch_dir(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("teamcraft-bridge-jobs-{}-{name}", std::process::id()))
  }

  /// The bridge's configuration with one source for `channel`; `listen` adds to `[listen]`.
  fn bridge_config(channel: ConversationId, listen: &str) -> BridgeConfig {
    let spool = scratch_dir(&format!("spool-{channel}"));
    BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [listen]
      addrs = ["127.0.0.1:0"]
      {listen}
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
    .unwrap()
  }

  /// Starts the bridge for the bot, posts `body` as a webhook with files, waits for the answer and
  /// stops the bridge again.
  async fn run_bridge(
    bot: TestClient,
    channel: ConversationId,
    body: Vec<u8>,
  ) -> (reqwest::StatusCode, String) {
    let request = (FORM_TYPE.to_owned(), None, body);
    let (status, answer, _) = run_bridge_requests(bot, channel, vec![request]).await.remove(0);
    (status, answer)
  }

  /// [`run_bridge`] for several requests one after another, each with its content type and maybe
  /// an `Idempotency-Key`; with each answer, whether it was given from memory.
  async fn run_bridge_requests(
    bot: TestClient,
    channel: ConversationId,
    requests: Vec<(String, Option<&str>, Vec<u8>)>,
  ) -> Vec<(reqwest::StatusCode, String, bool)> {
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
      index_interval: None,
      ..worker_settings(&config)
    };
    let worker = Worker::new(bot, sources, receiver, settings);
    let script = async {
      let mut answers = Vec::new();
      for (content_type, maybe_key, body) in requests {
        let mut request = reqwest::Client::new()
          .post(&url)
          .header("content-type", content_type);
        if let Some(key) = maybe_key {
          request = request.header(IDEMPOTENCY_KEY_HEADER, key);
        }
        let response = request.body(body).send().await.unwrap();
        let is_replayed = response.headers().contains_key(IDEMPOTENT_REPLAYED_HEADER);
        answers.push((response.status(), response.text().await.unwrap(), is_replayed));
      }
      endpoints.shutdown().await;
      answers
    };
    let (_, answers) = tokio::join!(worker.run(), script);
    let is_spool_empty = std::fs::read_dir(&spool).map_or(true, |mut entries| entries.next().is_none());
    assert!(is_spool_empty, "the spooled files are deleted");
    answers
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
