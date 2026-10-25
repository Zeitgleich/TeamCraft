//! Attachment downloads beside a host's loop (issue #491): the web and desktop cores start them
//! with the client and then let them run, so a large file doesn't hold up reads, sends or gateway
//! frames. At most a few run at once; the rest wait their turn in order.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{AttachmentId, ConversationId};

use crate::api::ServerApi;
use crate::client::Client;
use crate::error::CoreError;
use crate::store::{AttachmentRef, LocalStore};

/// Downloads running at once by default: enough for an album's pictures and the viewer, few enough
/// for a slow link.
pub const DEFAULT_PARALLEL_DOWNLOADS: usize = 4;

/// The downloads of a host: waiting, and running beside its loop.
pub struct Downloads {
  limit: usize,
  waiting: VecDeque<Job>,
  running: FuturesUnordered<Pin<Box<dyn Future<Output = Finished>>>>,
}

/// What a finished download leaves for the host.
pub enum Finished {
  /// A file for the viewer was handed to whoever asked for it.
  Viewed,
  /// A file to save; [`Downloads::finish`] records where it went.
  Saved {
    attachment: AttachmentId,
    saved: Result<String, CoreError>,
  },
}

/// Called with the opened file (or why it couldn't be opened).
pub type Deliver = Box<dyn FnOnce(Result<SecretBytes, CoreError>)>;

enum Job {
  View {
    reference: AttachmentRef,
    deliver: Deliver,
  },
  Save {
    conversation: ConversationId,
    attachment: AttachmentId,
  },
}

impl Downloads {
  /// `limit`: downloads running at once (at least one).
  pub fn new(limit: usize) -> Self {
    Self {
      limit: limit.max(1),
      waiting: VecDeque::new(),
      running: FuturesUnordered::new(),
    }
  }

  /// Queues opening a file for the viewer or an inline picture.
  pub fn view(&mut self, reference: AttachmentRef, deliver: Deliver) {
    self.waiting.push_back(Job::View { reference, deliver });
  }

  /// Queues saving a file to disk (in the browser: to its download folder).
  pub fn save(&mut self, conversation: ConversationId, attachment: AttachmentId) {
    self.waiting.push_back(Job::Save {
      conversation,
      attachment,
    });
  }

  pub fn is_running(&self) -> bool {
    !self.running.is_empty()
  }

  /// Starts waiting downloads while fewer than the limit run. Starting asks the server for the
  /// download URL (a short request); a failure goes to whoever waits. Returns whether the store
  /// changed (a file to save is marked as downloading).
  pub async fn start_due<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    let mut is_changed = false;
    while self.running.len() < self.limit {
      let Some(job) = self.waiting.pop_front() else {
        break;
      };
      match job {
        Job::View { reference, deliver } => match client.start_viewer_download(&reference).await {
          Ok(download) => self.running.push(Box::pin(async move {
            deliver(download.open().await);
            Finished::Viewed
          })),
          Err(error) => deliver(Err(error)),
        },
        Job::Save {
          conversation,
          attachment,
        } => {
          is_changed = true;
          match client.start_download_to_disk(conversation, attachment).await {
            Ok(download) => self.running.push(Box::pin(async move {
              Finished::Saved {
                attachment,
                saved: download.save().await,
              }
            })),
            Err(error) => tracing::warn!(%error, "could not start saving a file"),
          }
        },
      }
    }
    is_changed
  }

  /// The next download that finishes; never completes while none runs.
  pub async fn next_finished(&mut self) -> Finished {
    match self.running.next().await {
      Some(finished) => finished,
      None => std::future::pending().await,
    }
  }

  /// Records what a finished download left (where a saved file went); returns whether the store
  /// changed.
  pub fn finish<A: ServerApi, S: LocalStore>(client: &mut Client<A, S>, finished: Finished) -> bool {
    let Finished::Saved { attachment, saved } = finished else {
      return false;
    };
    match client.finish_download_to_disk(attachment, saved) {
      Ok(_) => {},
      Err(error) => tracing::warn!(%error, "saving a file failed"),
    }
    true
  }
}

impl Default for Downloads {
  fn default() -> Self {
    Self::new(DEFAULT_PARALLEL_DOWNLOADS)
  }
}
