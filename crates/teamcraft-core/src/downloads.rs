//! Attachment downloads beside a host's loop (issue #491): the web and desktop cores start them
//! with the client and then let them run, so a large file doesn't hold up reads, sends or gateway
//! frames. At most a few run at once; the rest wait their turn in order, except what the user opened
//! (the image viewer), which goes first. Saves to disk that failed on the network come back when
//! their retry is due, and those an app left running when it stopped continue once it starts
//! again (issue #153, `client::disk_downloads`).

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{AttachmentId, ConversationId};

use crate::api::ServerApi;
use crate::client::Client;
use crate::client::disk_downloads::{DownloadTarget, due_downloads};
use crate::client::files::failure_of;
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
  /// Files being saved: one transfer per file, so two never write the same partial file.
  saving: HashSet<AttachmentId>,
  /// When to look for saves to continue (client clock): at the start, after a save finished and
  /// when the next retry is due.
  maybe_next_scan_ms: Option<i64>,
  /// The first scan also continues the saves an app left running when it stopped.
  has_resumed: bool,
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
pub type Deliver = Box<dyn FnOnce(Result<SecretBytes, String>)>;

/// Whether anybody still wants the result (the caller gave up: the row scrolled away, the viewer
/// closed). A file nobody wants isn't started.
pub type IsWanted = Box<dyn Fn() -> bool>;

/// One who asked for a file.
struct Waiter {
  deliver: Deliver,
  is_wanted: IsWanted,
}

/// How soon a file for the viewer is wanted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadPriority {
  /// A preview in the conversation: in turn.
  #[default]
  Background,
  /// The user opened it (the image viewer): ahead of the previews, in a slot of its own.
  Now,
}

enum Job {
  View {
    reference: AttachmentRef,
    priority: DownloadPriority,
    /// Everybody who asked for the file while it waited.
    delivers: Vec<Waiter>,
  },
  Save {
    conversation: ConversationId,
    attachment: AttachmentId,
    target: DownloadTarget,
  },
}

impl Downloads {
  /// `limit`: downloads running at once (at least one).
  pub fn new(limit: usize) -> Self {
    Self {
      limit: limit.max(1),
      waiting: VecDeque::new(),
      running: FuturesUnordered::new(),
      saving: HashSet::new(),
      maybe_next_scan_ms: Some(i64::MIN),
      has_resumed: false,
    }
  }

  /// Queues opening a file for the viewer or an inline picture. A file the user opened goes ahead of
  /// the previews; asked for again while it waits, it is downloaded once for both.
  pub fn view(&mut self, reference: AttachmentRef, priority: DownloadPriority, deliver: Deliver) {
    self.view_while(reference, priority, deliver, Box::new(|| true));
  }

  /// Like [`Self::view`]; the file is dropped from the queue when `is_wanted` turns false for
  /// everybody who asked before it started.
  pub fn view_while(
    &mut self,
    reference: AttachmentRef,
    priority: DownloadPriority,
    deliver: Deliver,
    is_wanted: IsWanted,
  ) {
    let maybe_index = self
      .waiting
      .iter()
      .position(|job| job.views(reference.attachment_id));
    let mut job = maybe_index
      .and_then(|index| self.waiting.remove(index))
      .unwrap_or(Job::View {
        reference,
        priority,
        delivers: Vec::new(),
      });
    if let Job::View {
      priority: waiting,
      delivers,
      ..
    } = &mut job
    {
      delivers.push(Waiter { deliver, is_wanted });
      if priority == DownloadPriority::Now {
        *waiting = DownloadPriority::Now;
      }
    }
    if job.is_urgent() {
      // Behind other files the user opened, ahead of every preview.
      let at = self
        .waiting
        .iter()
        .position(|waiting| !waiting.is_urgent())
        .unwrap_or(self.waiting.len());
      self.waiting.insert(at, job);
    } else {
      self.waiting.push_back(job);
    }
  }

  /// Queues saving a file to disk (in the browser: to its download folder).
  pub fn save(&mut self, conversation: ConversationId, attachment: AttachmentId) {
    self.queue_save(conversation, attachment, DownloadTarget::DownloadDirectory);
  }

  /// Queues saving a file to `path`, which the user picked in a save dialog.
  pub fn save_to(&mut self, conversation: ConversationId, attachment: AttachmentId, path: PathBuf) {
    let path = path.to_string_lossy().into_owned();
    self.queue_save(conversation, attachment, DownloadTarget::File { path });
  }

  /// Queues saving a file under its own name into `folder`, which the user picked for several files
  /// at once; a name that is taken there gets a number (`name (2).jpg`).
  pub fn save_into(&mut self, conversation: ConversationId, attachment: AttachmentId, folder: PathBuf) {
    let path = folder.to_string_lossy().into_owned();
    self.queue_save(conversation, attachment, DownloadTarget::Folder { path });
  }

  pub fn is_running(&self) -> bool {
    !self.running.is_empty()
  }

  /// Starts waiting downloads while fewer than the limit run. Starting asks the server for the
  /// download URL (a short request); a failure goes to whoever waits. Returns whether the store
  /// changed (a file to save is marked as downloading).
  pub async fn start_due<A: ServerApi, S: LocalStore>(&mut self, client: &mut Client<A, S>) -> bool {
    self.queue_due_saves(client);
    let mut is_changed = false;
    // What the user opened has one slot beyond the limit, so it never waits for previews.
    while self.running.len() < self.limit
      || (self.running.len() <= self.limit && self.waiting.front().is_some_and(Job::is_urgent))
    {
      let Some(job) = self.waiting.pop_front() else {
        break;
      };
      match job {
        Job::View {
          reference, delivers, ..
        } => {
          let delivers: Vec<Waiter> = delivers.into_iter().filter(|w| (w.is_wanted)()).collect();
          if delivers.is_empty() {
            continue;
          }
          match client.start_viewer_download(&reference).await {
            Ok(download) => self.running.push(Box::pin(async move {
              deliver_all(delivers, download.open().await);
              Finished::Viewed
            })),
            Err(error) => deliver_all(delivers, Err(error)),
          }
        },
        Job::Save {
          conversation,
          attachment,
          target,
        } => {
          is_changed = true;
          match client
            .start_download_to_disk_at(conversation, attachment, target)
            .await
          {
            Ok(download) => {
              self.saving.insert(attachment);
              self.running.push(Box::pin(async move {
                Finished::Saved {
                  attachment,
                  saved: download.save().await,
                }
              }));
            },
            Err(error) => {
              // A retry may have come due: look again.
              self.maybe_next_scan_ms = Some(i64::MIN);
              tracing::warn!(
                %error,
                attachment_id = %attachment,
                reason = ?failure_of(&error),
                "could not start saving a file"
              );
            },
          }
        },
      }
    }
    is_changed
  }

  /// The next download that finishes; never completes while none runs.
  pub async fn next_finished(&mut self) -> Finished {
    match self.running.next().await {
      Some(finished) => {
        if let Finished::Saved { attachment, .. } = &finished {
          self.saving.remove(attachment);
          // It may have failed on the network: its retry is due later.
          self.maybe_next_scan_ms = Some(i64::MIN);
        }
        finished
      },
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
      Ok(_) => tracing::info!(attachment_id = %attachment, "saved a file"),
      // The card shows the reason; the log keeps the error (issue #807). Never the path: it names
      // the file.
      Err(error) => tracing::warn!(
        %error,
        attachment_id = %attachment,
        reason = ?failure_of(&error),
        "saving a file failed"
      ),
    }
    true
  }
}

impl Downloads {
  /// Queues a save unless the file is being saved or waits already.
  fn queue_save(&mut self, conversation: ConversationId, attachment: AttachmentId, target: DownloadTarget) {
    if self.saving.contains(&attachment) || self.waiting.iter().any(|job| job.saves(attachment)) {
      tracing::debug!(attachment_id = %attachment, "the file is being saved already");
      return;
    }
    self.waiting.push_back(Job::Save {
      conversation,
      attachment,
      target,
    });
  }

  /// Queues the saves whose retry came due (and, the first time, those an app left running).
  fn queue_due_saves<A: ServerApi, S: LocalStore>(&mut self, client: &Client<A, S>) {
    let now_ms = client.now_ms();
    if self.maybe_next_scan_ms.is_none_or(|next| now_ms < next) {
      return;
    }
    let includes_interrupted = !self.has_resumed;
    match due_downloads(client.store(), now_ms, includes_interrupted) {
      Ok((due, maybe_next_ms)) => {
        self.has_resumed = true;
        self.maybe_next_scan_ms = maybe_next_ms;
        if includes_interrupted {
          sweep_partials(client);
        }
        for download in due {
          tracing::info!(attachment_id = %download.attachment_id, "continuing a download");
          self.queue_save(download.conversation_id, download.attachment_id, download.target);
        }
      },
      Err(error) => {
        self.maybe_next_scan_ms = None;
        tracing::warn!(%error, "could not read the downloads to continue");
      },
    }
  }
}

/// Deletes partial downloads nothing continues any more (native: the browser keeps none).
fn sweep_partials<A: ServerApi, S: LocalStore>(client: &Client<A, S>) {
  #[cfg(not(target_arch = "wasm32"))]
  if let Some(staging) = &client.settings().maybe_upload_staging_dir {
    match crate::client::disk_downloads::sweep_partial_downloads(client.store(), staging) {
      Ok(0) => {},
      Ok(removed) => tracing::info!(removed, "deleted partial downloads nobody continues"),
      Err(error) => tracing::warn!(%error, "could not sweep the partial downloads"),
    }
  }
  #[cfg(target_arch = "wasm32")]
  let _ = client;
}

impl Job {
  fn saves(&self, attachment: AttachmentId) -> bool {
    matches!(self, Self::Save { attachment: saved, .. } if *saved == attachment)
  }

  fn is_urgent(&self) -> bool {
    matches!(
      self,
      Self::View {
        priority: DownloadPriority::Now,
        ..
      }
    )
  }

  fn views(&self, attachment: AttachmentId) -> bool {
    matches!(self, Self::View { reference, .. } if reference.attachment_id == attachment)
  }
}

/// Hands the file to everybody who asked for it.
fn deliver_all(delivers: Vec<Waiter>, opened: Result<SecretBytes, CoreError>) {
  let opened = opened.map_err(|error| error.to_string());
  for Waiter { deliver, .. } in delivers {
    let copy = match &opened {
      Ok(bytes) => Ok(SecretBytes::new(bytes.expose_secret().to_vec())),
      Err(error) => Err(error.clone()),
    };
    deliver(copy);
  }
}

#[cfg(feature = "ui")]
impl From<teamcraft_client_api::remote::AttachmentPriority> for DownloadPriority {
  fn from(priority: teamcraft_client_api::remote::AttachmentPriority) -> Self {
    use teamcraft_client_api::remote::AttachmentPriority;
    match priority {
      AttachmentPriority::Background => Self::Background,
      AttachmentPriority::Now => Self::Now,
    }
  }
}

impl Default for Downloads {
  fn default() -> Self {
    Self::new(DEFAULT_PARALLEL_DOWNLOADS)
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::rc::Rc;

  use teamcraft_proto::{Base64Bytes, Id, Snowflake};

  use super::*;

  #[test]
  fn a_file_the_user_opened_goes_ahead_of_the_previews() {
    let mut downloads = Downloads::new(2);
    downloads.view(file(1), DownloadPriority::Background, ignore());
    downloads.view(file(2), DownloadPriority::Background, ignore());
    downloads.view(file(3), DownloadPriority::Now, ignore());
    downloads.view(file(4), DownloadPriority::Now, ignore());
    assert_eq!(waiting(&downloads), [3, 4, 1, 2]);
  }

  #[test]
  fn a_preview_opened_while_it_waits_moves_up_and_is_downloaded_once() {
    let mut downloads = Downloads::new(2);
    downloads.view(file(1), DownloadPriority::Background, ignore());
    downloads.view(file(2), DownloadPriority::Background, ignore());
    let delivered = Rc::new(RefCell::new(0));
    for _ in 0..2 {
      let count = delivered.clone();
      downloads.view(
        file(2),
        DownloadPriority::Now,
        Box::new(move |_| *count.borrow_mut() += 1),
      );
    }
    assert_eq!(waiting(&downloads), [2, 1]);
    let Some(Job::View { delivers, .. }) = downloads.waiting.pop_front() else {
      panic!("not a view");
    };
    deliver_all(delivers, Ok(SecretBytes::new(b"picture".to_vec())));
    assert_eq!(*delivered.borrow(), 2, "everybody who asked got it");
  }

  #[test]
  fn a_file_nobody_wants_anymore_is_not_started() {
    let mut downloads = Downloads::new(2);
    let wanted = Rc::new(RefCell::new(true));
    let probe = wanted.clone();
    downloads.view_while(
      file(1),
      DownloadPriority::Background,
      Box::new(|_| {}),
      Box::new(move || *probe.borrow()),
    );
    *wanted.borrow_mut() = false;
    let Some(Job::View { delivers, .. }) = downloads.waiting.pop_front() else {
      panic!("not a view");
    };
    assert!(delivers.iter().all(|waiter| !(waiter.is_wanted)()));
  }

  fn waiting(downloads: &Downloads) -> Vec<i64> {
    downloads
      .waiting
      .iter()
      .filter_map(|job| match job {
        Job::View { reference, .. } => Some(reference.attachment_id.snowflake().get()),
        Job::Save { .. } => None,
      })
      .collect()
  }

  fn file(raw: i64) -> AttachmentRef {
    AttachmentRef {
      attachment_id: AttachmentId::from_snowflake(Snowflake::new(raw).unwrap()),
      key: Base64Bytes::new(Vec::new()),
      ciphertext_sha256: Base64Bytes::new(Vec::new()),
      file_name: format!("{raw}.jpg"),
      media_type: "image/jpeg".into(),
      size_bytes: 1,
      maybe_thumbnail: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
    }
  }

  fn ignore() -> Deliver {
    Box::new(|_| {})
  }
}
