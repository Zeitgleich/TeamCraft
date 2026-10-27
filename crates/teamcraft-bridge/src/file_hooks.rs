//! The endpoint side of webhooks with files (docs/bridge.md, "Files"): reads the form as it arrives
//! ([`crate::form`]), writes each file into the spool directory ([`crate::spool`]), hands the
//! message to the worker ([`crate::file_jobs`]) and answers once the files are up and the message
//! is queued, or says why not. The files are deleted from the spool whatever the answer.
//!
//! Before a byte of the body is read, the request needs a place: one of
//! `files.max_concurrent_requests`, one of its source's, and one in the worker's queue. Without one
//! it gets `503` at once. Then every piece of the body is paid from the source's and the bridge's
//! byte budgets as it arrives; a request that ends before its upload begins gets its bytes back.
//!
//! A request with an `Idempotency-Key` is read like any other, its files fingerprinted as they
//! arrive; only then is its key looked up ([`crate::idempotency`]), so a repeat is recognised by its
//! content and a repeat of an accepted request gives everything back without uploading.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use futures_util::StreamExt;
use teamcraft_core::client::direct_uploads::estimated_message_bytes;
use teamcraft_core::thumbnail::{image_dimensions, thumbnail_within};
use tokio::sync::{Semaphore, oneshot};
use tracing::warn;

use crate::config::BridgeConfig;
use crate::file_jobs::{FilesMessage, FilesReply};
use crate::form::{FileAction, FormLimits, FormReader};
use crate::http::{Accepted, HookError, HttpState, KeyClaim, Submission};
use crate::idempotency::{IdempotencyKey, MessageFingerprint};
use crate::sources::{FileBytes, FileCharge, Source};
use crate::spool::{SpoolDir, SpoolError, SpoolWriter, SpooledFile};
use crate::worker::alert_text;

/// What a file costs at least in the byte budgets: announcing, presigning and completing it is
/// work whatever its size, so a request of many tiny (or empty) files isn't free.
pub const MIN_FILE_CHARGE_BYTES: u64 = 64 * 1024;
/// Largest picture read for its thumbnail; bigger ones go without.
const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

/// How the endpoint takes files.
#[derive(Debug, Clone)]
pub struct FileSettings {
  pub spool: SpoolDir,
  /// For sources that don't set their own.
  pub default_max_files_per_message: u32,
  pub max_file_bytes: u64,
  /// The form's fields besides files together (`listen.max_body_bytes`).
  pub max_field_bytes: usize,
  /// A body that sends nothing for this long is cut off.
  pub idle_timeout: Duration,
  /// The server's message limit.
  pub max_message_bytes: usize,
  pub max_thumbnail_pixels: u64,
  /// `files.max_concurrent_requests`: the requests with files running, all sources together.
  pub requests: Arc<Semaphore>,
  /// Pictures are decoded one at a time, off the endpoint's threads.
  pub decoding: Arc<Semaphore>,
}

/// A `multipart/form-data` webhook, not read yet.
pub struct FormBody {
  /// With the boundary, as the producer wrote it.
  pub content_type: String,
  pub maybe_length: Option<u64>,
  pub body: Body,
}

impl FileSettings {
  pub fn from_config(config: &BridgeConfig) -> Self {
    let files = &config.files;
    Self {
      spool: SpoolDir::new(config.spool_dir(), files.max_spool_bytes),
      default_max_files_per_message: files.max_files_per_message,
      max_file_bytes: files.max_file_bytes,
      max_field_bytes: config.listen.max_body_bytes,
      idle_timeout: Duration::from_secs(files.idle_timeout_secs),
      max_message_bytes: files.max_message_bytes,
      max_thumbnail_pixels: files.max_thumbnail_pixels,
      requests: Arc::new(Semaphore::new(
        usize::try_from(files.max_concurrent_requests).unwrap_or(usize::MAX),
      )),
      decoding: Arc::new(Semaphore::new(1)),
    }
  }
}

/// Reads the form, spools its files and hands its message on; `Ok` once the message is queued, or
/// at once for a repeat of an accepted request with the same key.
pub async fn receive_form(
  state: &HttpState,
  source: Arc<Source>,
  form: FormBody,
  maybe_key: Option<IdempotencyKey>,
) -> Result<Accepted, HookError> {
  let settings = state.files();
  // The request's places, held until it is answered.
  let Ok(_request) = settings.requests.clone().try_acquire_owned() else {
    return Err(HookError::Busy);
  };
  let Some(_source_request) = state.sources().file_request_slot(&source) else {
    return Err(HookError::Busy);
  };
  let queue_place = state.reserve_submission()?;
  let max_request_bytes = state.sources().max_file_request_bytes();
  if form.maybe_length.is_some_and(|length| length > max_request_bytes) {
    return Err(HookError::RequestTooLarge);
  }
  let limits = FormLimits {
    max_files: source
      .maybe_max_files_per_message
      .unwrap_or(settings.default_max_files_per_message),
    max_file_bytes: settings.max_file_bytes,
    max_field_bytes: settings.max_field_bytes,
  };
  let mut reader = FormReader::new(&form.content_type, limits)?;
  let mut charge = FileCharge::new(state.sources().clone(), source.clone());
  let mut maybe_fingerprint = maybe_key.as_ref().map(|_| MessageFingerprint::default());
  let mut files = read_files(
    settings,
    &mut reader,
    form.body,
    &mut charge,
    max_request_bytes,
    maybe_fingerprint.as_mut(),
  )
  .await?;
  let fields = reader.finish()?;
  let message = fields.message(source.danger_is_alert, !files.is_empty())?;
  if !files.is_empty() {
    check_message_size(settings, &source, &message.text, message.is_alert, &files)?;
  }
  let maybe_keyed =
    maybe_key.zip(maybe_fingerprint.map(|fingerprint| fingerprint.finish(&message.text, message.is_alert)));
  let maybe_guard = match state.claim_key(&source, maybe_keyed).await? {
    KeyClaim::Post(maybe_guard) => maybe_guard,
    // Its files leave the spool and its bytes come back as it is dropped.
    KeyClaim::Repeat => return Ok(Accepted::Replayed),
  };
  if files.is_empty() {
    queue_place.send(Submission::Message {
      source,
      text: message.text,
      is_alert: message.is_alert,
    });
    if let Some(guard) = maybe_guard {
      guard.accept(Instant::now());
    }
    return Ok(Accepted::Posted);
  }
  for file in &mut files {
    add_picture_details(settings, file).await;
  }
  let (reply, answer) = oneshot::channel();
  queue_place.send(Submission::Files(FilesMessage {
    source,
    text: message.text,
    is_alert: message.is_alert,
    files,
    charge,
    reply: FilesReply::new(reply, maybe_guard),
  }));
  match answer.await {
    Ok(result) => result.map(|()| Accepted::Posted).map_err(HookError::from),
    // The worker stopped before it got to the message.
    Err(_) => Err(HookError::ShuttingDown),
  }
}

/// Reads the body to the form's end, writing the files into the spool and paying for every piece;
/// with a fingerprint, the files are fed into it too.
async fn read_files(
  settings: &FileSettings,
  reader: &mut FormReader,
  body: Body,
  charge: &mut FileCharge,
  max_request_bytes: u64,
  mut maybe_fingerprint: Option<&mut MessageFingerprint>,
) -> Result<Vec<SpooledFile>, HookError> {
  let mut files: Vec<SpooledFile> = Vec::new();
  let mut maybe_writer: Option<SpoolWriter> = None;
  let mut read_bytes: u64 = 0;
  let mut stream = body.into_data_stream();
  while !reader.is_done() {
    let next = tokio::time::timeout(settings.idle_timeout, stream.next())
      .await
      .map_err(|_| HookError::RequestTimeout)?;
    let Some(chunk) = next else {
      break;
    };
    let chunk = chunk.map_err(|_| HookError::UnreadableBody)?;
    read_bytes += chunk.len() as u64;
    if read_bytes > max_request_bytes {
      return Err(HookError::RequestTooLarge);
    }
    pay(charge, chunk.len() as u64)?;
    for action in reader.feed(&chunk)? {
      match action {
        FileAction::Start(info) => {
          if let Some(fingerprint) = maybe_fingerprint.as_deref_mut() {
            fingerprint.start_file(&info.file_name, &info.media_type);
          }
          let writer = settings.spool.create(info.file_name, info.media_type).await;
          maybe_writer = Some(writer.map_err(spool_failed)?);
        },
        FileAction::Data(bytes) => {
          if let Some(writer) = maybe_writer.as_mut() {
            writer.write(&bytes).await.map_err(spool_failed)?;
            if let Some(fingerprint) = maybe_fingerprint.as_deref_mut() {
              fingerprint.file_data(&bytes);
            }
          }
        },
        FileAction::End => {
          if let Some(writer) = maybe_writer.take() {
            let file = writer.finish().await.map_err(spool_failed)?;
            pay(charge, MIN_FILE_CHARGE_BYTES.saturating_sub(file.size_bytes))?;
            if let Some(fingerprint) = maybe_fingerprint.as_deref_mut() {
              fingerprint.end_file(file.size_bytes);
            }
            files.push(file);
          }
        },
      }
    }
  }
  Ok(files)
}

fn pay(charge: &mut FileCharge, bytes: u64) -> Result<(), HookError> {
  if bytes == 0 {
    return Ok(());
  }
  match charge.take(bytes, Instant::now()) {
    FileBytes::Taken => Ok(()),
    FileBytes::Wait(retry_after) => Err(HookError::FileRateLimited { retry_after }),
  }
}

/// Refuses a message that won't fit the server's limit once its files are in it, before anything
/// is uploaded.
fn check_message_size(
  settings: &FileSettings,
  source: &Source,
  text: &str,
  is_alert: bool,
  files: &[SpooledFile],
) -> Result<(), HookError> {
  let (text, mentions) = alert_text(text.to_owned(), is_alert, &source.mentions);
  let names: Vec<(&str, &str)> = files
    .iter()
    .map(|file| (file.file_name.as_str(), file.media_type.as_str()))
    .collect();
  if estimated_message_bytes(&text, mentions.len(), &names) > settings.max_message_bytes {
    return Err(HookError::MessageTooLarge);
  }
  Ok(())
}

/// A picture's thumbnail and size, decoded off the endpoint's threads, one picture at a time and
/// within `files.max_thumbnail_pixels`. A picture the decoder chokes on (even with a panic) goes
/// without; it never takes the bridge down.
async fn add_picture_details(settings: &FileSettings, file: &mut SpooledFile) {
  if !file.media_type.starts_with("image/") || file.size_bytes > MAX_THUMBNAIL_SOURCE_BYTES {
    return;
  }
  let Ok(_decoding) = settings.decoding.clone().acquire_owned().await else {
    return;
  };
  let path = file.path().to_owned();
  let media_type = file.media_type.clone();
  let max_pixels = settings.max_thumbnail_pixels;
  let decoded = tokio::task::spawn_blocking(move || {
    std::panic::catch_unwind(move || {
      let bytes = std::fs::read(&path).ok()?;
      Some((
        thumbnail_within(&bytes, &media_type, max_pixels),
        image_dimensions(&bytes, &media_type),
      ))
    })
  })
  .await;
  match decoded {
    Ok(Ok(Some((maybe_thumbnail, maybe_dimensions)))) => {
      file.maybe_thumbnail = maybe_thumbnail;
      file.maybe_dimensions = maybe_dimensions;
    },
    Ok(Ok(None)) => {},
    Ok(Err(_)) | Err(_) => warn!("decoding a picture for its thumbnail failed; it goes without"),
  }
}

fn spool_failed(error: SpoolError) -> HookError {
  match error {
    SpoolError::Full => HookError::SpoolFull,
    error => {
      warn!(%error, "a file could not be written to the spool directory");
      HookError::SpoolFailed
    },
  }
}
