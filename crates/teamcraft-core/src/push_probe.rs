//! Catching a conversation up for a notification without advancing the account (issue #839,
//! e2ee.md §7.2): the iOS notification extension runs next to an app that owns the account's state,
//! so it must neither save MLS state nor tell the server anything. When a push can't be read from
//! the store and the one fetched message (a new conversation, keys that changed, a sender's new
//! device), it catches up on a throwaway client instead:
//!
//! - its store is an [`OverlayStore`] over the real one: welcomes, commits and keys are processed in
//!   memory only, bounded in size, and dropped with it;
//! - its server is a [`ReadOnlyApi`]: every call that would change something there (a commit, a
//!   rejoin request, key packages, read positions) is refused before it is sent.
//!
//! The app processes the same welcomes and commits from the server later, as if the probe never ran:
//! the key package a welcome spends in the probe stays on disk and on the server.

use teamcraft_proto::ConversationId;

use crate::api::{ReadOnlyApi, ServerApi};
use crate::client::HistoryBackfill;
use crate::clock::Clock;
use crate::push_preview::{PreviewGap, PushPreview, open_push_wakeup, push_preview};
use crate::store::{LocalStore, OverlayStore};
use crate::{Client, ClientSettings, CoreError};

/// How much the probe's writes may take in memory: the notification extension has about 24 MB in
/// all, of which the store's caches, the MLS state and the runtime take most (docs/performance.md).
/// A catch-up that needs more stops with a full-disk error, and the notification shows what was
/// learned by then.
pub const PROBE_OVERLAY_BYTES: usize = 4 * 1024 * 1024;

/// The settings of the probe's client, from the extension's own: no folders, no thumbnails, no
/// history to page, and the MLS upkeep left to the app.
pub fn probe_settings(settings: ClientSettings) -> ClientSettings {
  ClientSettings {
    leaves_mls_upkeep_to_app: true,
    maybe_download_dir: None,
    maybe_upload_staging_dir: None,
    makes_thumbnails: false,
    history_backfill: HistoryBackfill::OFF,
    ..settings
  }
}

/// What the probe learned and how it went, for a log line: counts only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeReport {
  /// Calls that would have changed something on the server, refused.
  pub refused_calls: u64,
  /// About how many bytes the probe's writes took in memory.
  pub overlay_bytes: usize,
  /// The messages were caught up too (the groups alone didn't make the message readable).
  pub read_messages: bool,
}

/// The preview of `sealed` after catching its conversation up on `overlay` through `api` made
/// read-only. Never writes below `overlay` and never changes anything on the server; dropping the
/// future at any await leaves both as they were, so the caller may bound it by time and then read
/// what `overlay` holds by then (`push_preview` without an API). `None`: the store holds no account.
pub async fn probe_push<A: ServerApi, S: LocalStore>(
  api: A,
  overlay: &OverlayStore<S>,
  clock: Box<dyn Clock>,
  settings: ClientSettings,
  sealed: &[u8],
) -> Result<Option<(PushPreview, ProbeReport)>, CoreError> {
  let conversation: ConversationId = open_push_wakeup(overlay, sealed)?.conversation_id;
  let Some(mut client) = Client::open(ReadOnlyApi::new(api), overlay, clock, probe_settings(settings))?
  else {
    return Ok(None);
  };
  let mut report = ProbeReport::default();
  // The conversation's record and members (names), then its welcome and MLS backlog (keys). A step
  // that fails (a refused call, the memory bound) ends the catch-up: what it learned stays.
  let followed = async {
    client.push_refresh_conversation(conversation).await?;
    client.push_follow_groups(conversation).await
  }
  .await;
  log_stop("groups", &followed);
  let mut preview = push_preview(overlay, Some(client.api()), sealed).await?;
  // Still unreadable: a sender's new device, or a message that came with a change the groups didn't
  // bring. The messages and the senders' keys, as the app's catch-up reads them.
  if still_unreadable(&preview) {
    report.read_messages = true;
    let caught_up = client.push_catch_up_messages(conversation).await;
    log_stop("messages", &caught_up);
    let again = push_preview(overlay, Some(client.api()), sealed).await?;
    if !still_unreadable(&again) || preview.maybe_gap == Some(PreviewGap::NoSession) {
      preview = again;
    }
  }
  report.refused_calls = client.api().refusal_count();
  report.overlay_bytes = overlay.overlay_bytes();
  Ok(Some((preview, report)))
}

fn still_unreadable(preview: &PushPreview) -> bool {
  matches!(
    preview.maybe_gap,
    Some(PreviewGap::NotDecrypted | PreviewGap::NoSession | PreviewGap::TimedOut)
  )
}

/// Why a step stopped, by class only: an error's text can carry what a server said.
fn log_stop<T>(step: &'static str, result: &Result<T, CoreError>) {
  if let Err(error) = result {
    let cause = match error {
      CoreError::Api(_) => "api",
      CoreError::Store(_) => "store",
      _ => "other",
    };
    tracing::info!(step, cause, "push probe step stopped");
  }
}
