//! Recording the call in the browser (issue #2114, e2ee.md §8.3), as the desktop shell does
//! (`apps/desktop/src/calls/recording.rs`): `teamcraft-media` records into this page's folder of the
//! origin private file system (or memory), the call's loop hands it the audio and sends the signed
//! notice that tells everybody.
//!
//! - The recorder belongs to the module, not to the call: a call that ends finishes the recording
//!   and keeps the take, for the user to post or delete.
//! - The file is encrypted while it is recorded and while it waits, under a key only this page
//!   holds. Posting decrypts it into a copy next to it, which the page hands the client core like a
//!   picked file (the core keeps its own copy for a reload, as for every attachment), and deletes
//!   the sealed file. "Save as file…" (#2116) decrypts such a copy for the page to download and
//!   keeps the take. One take at a time: a new recording replaces a take nobody posted.
//! - A page closed, reloaded or discarded while recording loses the recording (its key dies with
//!   the page); the next call module of the origin deletes what it left. The key isn't kept
//!   anywhere else on purpose (#2116): the browser has no store the disk doesn't show, so a key
//!   kept next to the sealed pieces would protect nothing (e2ee.md §8.3). Signing out, or the app
//!   moving to another tab, deletes this page's recordings.
//! - Nothing about the file is logged.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use teamcraft_client_api::calls::{CallRecordingTake, OwnCallRecording};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_media::call_web::recording::{
  RecordingError, RecordingFolder, WebCallRecorder, WebRecordingSink, WebRecordingTake, is_supported,
};
use teamcraft_proto::{ConversationId, MAX_RECORDING_MINUTES};
use wasm_bindgen_futures::spawn_local;
use web_sys::Blob;

/// The shortest maximum a recording is started with, whatever the page asks.
const MIN_RECORDING_SECS: u32 = 60;

/// This page's recording and its take.
pub(crate) struct Recordings {
  folder: RecordingFolder,
  maybe_running: RefCell<Option<Running>>,
  /// A start waits for the file to open: no second one meanwhile.
  is_starting: Cell<bool>,
  maybe_take: RefCell<Option<Take>>,
  /// Counts sign-outs: a recording finished or a take posted while one happens leaves nothing
  /// behind.
  session: Cell<u64>,
}

struct Running {
  conversation: ConversationId,
  recorder: WebCallRecorder,
}

struct Take {
  shown: CallRecordingTake,
  recorded: WebRecordingTake,
}

impl Recordings {
  /// Deletes, in the background, what pages that no longer run left behind.
  pub(crate) fn new() -> Self {
    spawn_local(RecordingFolder::sweep());
    Self {
      folder: RecordingFolder::new(),
      maybe_running: RefCell::new(None),
      is_starting: Cell::new(false),
      maybe_take: RefCell::new(None),
      session: Cell::new(0),
    }
  }

  /// Whether this browser records calls.
  pub(crate) fn can_record() -> bool {
    is_supported()
  }

  /// The take the UI offers to post or delete.
  pub(crate) fn take(&self) -> Option<CallRecordingTake> {
    self.maybe_take.borrow().as_ref().map(|take| take.shown)
  }

  /// Starts recording the call of `conversation`; returns where its loop hands the audio and what
  /// the active call shows. `on_end` is called when the recording ends by itself.
  pub(crate) async fn start(
    &self,
    conversation: ConversationId,
    max_secs: u32,
    on_end: Box<dyn Fn()>,
  ) -> Result<(WebRecordingSink, OwnCallRecording), ActionFailure> {
    if self.maybe_running.borrow().is_some() || self.is_starting.get() {
      return Err(ActionFailure::other("a recording is already running"));
    }
    let max_secs = max_secs.clamp(MIN_RECORDING_SECS, u32::from(MAX_RECORDING_MINUTES) * 60);
    let session = self.session.get();
    self.is_starting.set(true);
    let started =
      WebCallRecorder::start(&self.folder, Duration::from_secs(u64::from(max_secs)), on_end).await;
    self.is_starting.set(false);
    let recorder = started.map_err(|error| ActionFailure::other(error.to_string()))?;
    if self.session.get() != session {
      recorder.cancel();
      return Err(ActionFailure::other("signed out while the recording started"));
    }
    let sink = recorder.sink();
    self.maybe_running.replace(Some(Running {
      conversation,
      recorder,
    }));
    let own = OwnCallRecording {
      started_at: crate::unix_ms(),
      max_secs,
      is_unconfirmed: false,
    };
    Ok((sink, own))
  }

  /// Finishes the running recording, if any, and keeps its take (replacing one nobody posted).
  /// Fails when there is nothing to keep because the browser couldn't write it.
  pub(crate) async fn finish(&self) -> Result<(), ActionFailure> {
    let Some(Running {
      conversation,
      recorder,
    }) = self.maybe_running.borrow_mut().take()
    else {
      return Ok(());
    };
    let session = self.session.get();
    let recorded = match recorder.finish().await {
      Ok(recorded) => recorded,
      // Stopped before anything was recorded: nothing to keep, nothing went wrong.
      Err(RecordingError::Empty) => return Ok(()),
      Err(error) => {
        tracing::warn!(%error, "the call recording could not be finished");
        return Err(ActionFailure::other(error.to_string()));
      },
    };
    if self.session.get() != session {
      recorded.delete().await;
      return Ok(());
    }
    let shown = CallRecordingTake {
      conversation,
      duration_ms: recorded.duration_ms,
      size_bytes: recorded.size_bytes,
      is_at_limit: recorded.is_at_limit,
      is_at_memory_limit: recorded.is_at_memory_limit,
      // A page's recordings never outlive it: their key is only in its memory (e2ee.md §8.3).
      maybe_left_over: None,
    };
    let maybe_previous = self.maybe_take.replace(Some(Take { shown, recorded }));
    if let Some(previous) = maybe_previous {
      previous.recorded.delete().await;
    }
    Ok(())
  }

  /// Deletes the take the user discarded.
  pub(crate) async fn discard(&self) {
    let maybe_take = self.maybe_take.borrow_mut().take();
    if let Some(take) = maybe_take {
      take.recorded.delete().await;
    }
  }

  /// The take decrypted, for the page to post as a picked file; the sealed file is deleted and the
  /// take no longer offered. On a failure it stays offered: the user can try again or delete it.
  pub(crate) async fn file_for_post(&self) -> Result<Blob, ActionFailure> {
    let take = self
      .maybe_take
      .borrow_mut()
      .take()
      .ok_or_else(|| ActionFailure::other("no recording to post"))?;
    let session = self.session.get();
    let result = take.recorded.decrypt(&self.folder).await;
    if self.session.get() != session {
      // Signed out meanwhile: the copy and the take go with this page's folder.
      take.recorded.delete().await;
      return Err(ActionFailure::other(
        "signed out while the recording was prepared for posting",
      ));
    }
    match result {
      Ok(blob) => {
        take.recorded.delete().await;
        Ok(blob)
      },
      Err(error) => {
        tracing::warn!(%error, "the call recording could not be prepared for posting");
        if self.maybe_take.borrow().is_none() {
          self.maybe_take.replace(Some(take));
        } else {
          // A newer take finished meanwhile and is the one offered; this one goes.
          take.recorded.delete().await;
        }
        Err(ActionFailure::other(error.to_string()))
      },
    }
  }

  /// The take decrypted, for the page to download as a file the user keeps (#2116). The take stays
  /// offered: the user can still post or delete it. The copy stays in this page's folder like a
  /// copy for posting, until sign-out or the next sweep.
  pub(crate) async fn file_for_save(&self) -> Result<Blob, ActionFailure> {
    let take = self
      .maybe_take
      .borrow_mut()
      .take()
      .ok_or_else(|| ActionFailure::other("no recording to save"))?;
    let session = self.session.get();
    let result = take.recorded.decrypt(&self.folder).await;
    if self.session.get() != session {
      take.recorded.delete().await;
      return Err(ActionFailure::other(
        "signed out while the recording was prepared for saving",
      ));
    }
    if self.maybe_take.borrow().is_none() {
      self.maybe_take.replace(Some(take));
    } else {
      // A newer take finished meanwhile and is the one offered; this one goes.
      take.recorded.delete().await;
    }
    result.map_err(|error| {
      tracing::warn!(%error, "the call recording could not be prepared for saving");
      ActionFailure::other(error.to_string())
    })
  }

  /// Ends the session's recordings: a running one is cancelled, the take and this page's folder
  /// go (the user signs out, or the app moves to another tab).
  pub(crate) async fn sign_out(&self) {
    self.session.set(self.session.get() + 1);
    let maybe_running = self.maybe_running.borrow_mut().take();
    if let Some(running) = maybe_running {
      running.recorder.cancel();
    }
    let maybe_take = self.maybe_take.borrow_mut().take();
    drop(maybe_take);
    self.folder.clear().await;
  }
}
