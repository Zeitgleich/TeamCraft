//! The endpoint side of webhooks with files (docs/bridge.md, "Files"): reads the form as it arrives
//! ([`crate::form`]), writes each file into the spool directory ([`crate::spool`]), hands the
//! message to the worker ([`crate::file_jobs`]) and answers once the files are up and the message
//! is queued, or says why not. The files are deleted from the spool whatever the answer.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use futures_util::StreamExt;
use tokio::sync::oneshot;
use tracing::warn;

use crate::config::BridgeConfig;
use crate::file_jobs::FilesMessage;
use crate::form::{FileAction, FormLimits, FormReader};
use crate::http::{HookError, HttpState, Submission, submit};
use crate::sources::{FileBytes, Source};
use crate::spool::{SpoolDir, SpoolWriter, SpooledFile};

/// How the endpoint takes files.
#[derive(Debug, Clone)]
pub struct FileSettings {
  pub spool: SpoolDir,
  /// For sources that don't set their own.
  pub default_max_files_per_message: u32,
  pub max_file_bytes: u64,
  /// The form's fields besides files together (`listen.max_body_bytes`).
  pub max_field_bytes: usize,
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
    Self {
      spool: SpoolDir::new(config.spool_dir()),
      default_max_files_per_message: config.files.max_files_per_message,
      max_file_bytes: config.files.max_file_bytes,
      max_field_bytes: config.listen.max_body_bytes,
    }
  }
}

/// Reads the form, spools its files and hands its message on; `Ok` once the message is queued.
pub async fn receive_form(state: &HttpState, source: Arc<Source>, form: FormBody) -> Result<(), HookError> {
  let settings = state.files();
  let limits = FormLimits {
    max_files: source
      .maybe_max_files_per_message
      .unwrap_or(settings.default_max_files_per_message),
    max_file_bytes: settings.max_file_bytes,
    max_field_bytes: settings.max_field_bytes,
  };
  let mut reader = FormReader::new(&form.content_type, limits)?;
  // A length known up front is taken from the byte budget at once, so a request that doesn't fit
  // is refused before it is read; otherwise every piece pays as it arrives.
  let is_paid = match form.maybe_length {
    Some(length) => {
      take_file_bytes(state, length)?;
      true
    },
    None => false,
  };
  let mut files: Vec<SpooledFile> = Vec::new();
  let mut maybe_writer: Option<SpoolWriter> = None;
  let mut stream = form.body.into_data_stream();
  while !reader.is_done() {
    let Some(chunk) = stream.next().await else {
      break;
    };
    let chunk = chunk.map_err(|_| HookError::UnreadableBody)?;
    if !is_paid {
      take_file_bytes(state, chunk.len() as u64)?;
    }
    for action in reader.feed(&chunk)? {
      match action {
        FileAction::Start(info) => {
          let writer = settings.spool.create(info.file_name, info.media_type).await;
          maybe_writer = Some(writer.map_err(spool_failed)?);
        },
        FileAction::Data(bytes) => {
          if let Some(writer) = maybe_writer.as_mut() {
            writer.write(&bytes).await.map_err(spool_failed)?;
          }
        },
        FileAction::End => {
          if let Some(writer) = maybe_writer.take() {
            files.push(writer.finish().await.map_err(spool_failed)?);
          }
        },
      }
    }
  }
  let fields = reader.finish()?;
  let message = fields.message(source.danger_is_alert, !files.is_empty())?;
  if files.is_empty() {
    return submit(
      state,
      Submission::Message {
        source,
        text: message.text,
        is_alert: message.is_alert,
      },
    );
  }
  let (reply, answer) = oneshot::channel();
  submit(
    state,
    Submission::Files(FilesMessage {
      source,
      text: message.text,
      is_alert: message.is_alert,
      files,
      reply,
    }),
  )?;
  match answer.await {
    Ok(result) => result.map_err(HookError::from),
    // The worker stopped before it got to the message.
    Err(_) => Err(HookError::ShuttingDown),
  }
}

fn take_file_bytes(state: &HttpState, bytes: u64) -> Result<(), HookError> {
  match state.sources().try_take_file_bytes(bytes, Instant::now()) {
    FileBytes::Taken => Ok(()),
    FileBytes::Wait(retry_after) => Err(HookError::FileRateLimited { retry_after }),
    FileBytes::NeverFits => Err(HookError::RequestTooLarge),
  }
}

fn spool_failed(error: std::io::Error) -> HookError {
  warn!(%error, "a file could not be written to the spool directory");
  HookError::SpoolFailed
}
