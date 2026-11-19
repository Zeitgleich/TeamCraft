//! The composer's link preview (#2073, e2ee.md §12): the preview of the draft's first link shows
//! while the user types, made the same way as a sent message's ([`make_link_preview`]), by this
//! device, for domains the signed policy allows. The message carries exactly what the composer
//! showed ([`OutgoingLinkPreview::AsShown`]): the ready preview, or none while it loads, after it
//! failed or once the user removed it. Sending never waits for a fetch.
//!
//! Where a draft's preview stands is kept in the local store ([`Table::Account`], this device only,
//! like what the composer replies to): the app's threads that read and write it each have their own
//! backend, and a removed preview stays removed after a restart. The fetch runs in the host, beside
//! its loop ([`ComposerFetches`]), so a slow site never holds up sending or syncing.
//!
//! Links are content: nothing here logs them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures_util::future::{AbortHandle, Abortable, LocalBoxFuture};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::actions::{ComposerLinkPreview, LinkPreview, OutgoingLinkPreview};
use teamcraft_proto::{ConversationId, LinkPreviewPolicy, ThreadId};
use url::Url;

use crate::client::previews::{LinkPreviewSettings, default_fetcher, make_link_preview};
use crate::clock::{Clock, SystemClock};
use crate::link_preview::{PreviewFetcher, first_url};
use crate::records;
use crate::store::{LinkPreviewData, LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};

/// The key prefix of a draft's preview in [`Table::Account`].
const KEY_PREFIX: &str = "composer_preview/";
/// A failed fetch (offline, a slow or broken site) is tried again for the same link after this,
/// when the composer reports its draft again: coming back online brings the preview back while the
/// user types on, and a link that keeps failing isn't fetched on every keystroke.
pub const RETRY_FAILED_AFTER: Duration = Duration::from_secs(30);

/// Where the preview of one draft's link stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DraftPreview {
  /// The link: the draft's first one the policy allows.
  url: String,
  state: PreviewState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum PreviewState {
  /// The host fetches it ([`ComposerFetches`]); asked again after a restart.
  Loading,
  Ready {
    preview: LinkPreviewData,
  },
  /// No preview came (offline, timeout, no metadata, a redirect elsewhere).
  Failed {
    at_ms: i64,
  },
  /// The user removed it: this link of the draft gets none.
  Removed,
  /// A state of a newer client: treated as removed, so nothing is sent unasked.
  #[serde(other)]
  Unknown,
}

/// Runs the composer's preview fetches in the host (desktop shell, mobile engine), beside its loop:
/// [`CoreCommand::ComposerLinkChanged`] is handed here instead of to the client. One fetch per draft
/// at a time; a newer link of the draft, or none, cancels the fetch of the old one.
pub struct ComposerFetches<S> {
  store: S,
  maybe_fetcher: Option<Rc<dyn PreviewFetcher>>,
  settings: Rc<LinkPreviewSettings>,
  clock: Rc<dyn Clock>,
  running: Rc<RefCell<HashMap<String, Running>>>,
}

/// A fetch in flight for a draft.
struct Running {
  url: String,
  abort: AbortHandle,
}

impl<S: LocalStore + Clone + 'static> ComposerFetches<S> {
  /// Fetches with the platform's HTTP client; none in the browser, which can't read other sites.
  pub fn new(store: S, settings: LinkPreviewSettings) -> Self {
    let maybe_fetcher = default_fetcher(&settings).map(Rc::from);
    Self {
      store,
      maybe_fetcher,
      settings: Rc::new(settings),
      clock: Rc::new(SystemClock),
      running: Rc::default(),
    }
  }

  /// Replaces how pages are fetched and what time it is (tests).
  #[must_use]
  pub fn with_fetcher(mut self, fetcher: Box<dyn PreviewFetcher>, clock: Rc<dyn Clock>) -> Self {
    self.maybe_fetcher = Some(Rc::from(fetcher));
    self.clock = clock;
    self
  }

  /// Whether `command` is one of these; the host runs it here and not in the client.
  pub fn handles(command: &CoreCommand) -> bool {
    matches!(command, CoreCommand::ComposerLinkChanged { .. })
  }

  /// For [`CoreCommand::ComposerLinkChanged`]: cancels a fetch the draft no longer wants and
  /// returns the one it wants now, for the host to spawn. The future answers whether it changed the
  /// preview; the host then tells the UI to read it again.
  pub fn on_command(&self, command: &CoreCommand) -> Option<LocalBoxFuture<'static, bool>> {
    let CoreCommand::ComposerLinkChanged {
      conversation,
      maybe_thread,
    } = command
    else {
      return None;
    };
    let key = draft_key(*conversation, *maybe_thread);
    let maybe_wanted = read_preview(&self.store, &key)
      .filter(|draft| draft.state == PreviewState::Loading)
      .map(|draft| draft.url);
    let mut running = self.running.borrow_mut();
    if let Some(current) = running.get(&key) {
      if maybe_wanted.as_deref() == Some(current.url.as_str()) {
        return None;
      }
      current.abort.abort();
      running.remove(&key);
    }
    let url = maybe_wanted?;
    let Some(fetcher) = self.maybe_fetcher.clone() else {
      // Nothing can read the page here: the composer shows no preview.
      finish(
        &self.store,
        &key,
        &url,
        PreviewState::Failed {
          at_ms: self.clock.now_ms(),
        },
      );
      return None;
    };
    let (abort, registration) = AbortHandle::new_pair();
    running.insert(
      key.clone(),
      Running {
        url: url.clone(),
        abort,
      },
    );
    let (store, settings, clock, running) = (
      self.store.clone(),
      self.settings.clone(),
      self.clock.clone(),
      self.running.clone(),
    );
    let (fetch_store, fetch_url) = (store.clone(), url.clone());
    let fetch = async move {
      let policy = verified_link_policy(&fetch_store);
      make_link_preview(&*fetcher, &settings, &policy, &fetch_url).await
    };
    Some(Box::pin(async move {
      let outcome = Abortable::new(fetch, registration).await;
      let Ok(maybe_preview) = outcome else {
        // Cancelled: the draft wants another link, or none.
        return false;
      };
      {
        let mut running = running.borrow_mut();
        if running.get(&key).is_some_and(|current| current.url == url) {
          running.remove(&key);
        }
      }
      let state = match maybe_preview {
        Some(preview) => PreviewState::Ready { preview },
        None => PreviewState::Failed {
          at_ms: clock.now_ms(),
        },
      };
      finish(&store, &key, &url, state)
    }))
  }
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// See [`teamcraft_client_api::backend::ChatBackend::update_composer_link`].
  pub(crate) fn ui_update_composer_link(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
  ) {
    let key = draft_key(conversation, maybe_thread);
    let maybe_url = composer_url(body, &self.verified_policy().link_previews);
    let maybe_draft = read_preview(self.store_ref(), &key);
    match (maybe_url, maybe_draft) {
      (None, None) => {},
      (None, Some(_)) => self.forget_preview(conversation, maybe_thread, &key),
      (Some(url), Some(draft)) if draft.url == url.as_str() => match draft.state {
        // Asked again: a fetch lost to a restart starts anew, a running one goes on.
        PreviewState::Loading => self.fetch_preview(conversation, maybe_thread),
        PreviewState::Failed { at_ms } if self.is_retry_due(at_ms) => {
          self.start_preview(conversation, maybe_thread, &key, url.as_str());
        },
        _ => {},
      },
      (Some(url), _) => self.start_preview(conversation, maybe_thread, &key, url.as_str()),
    }
  }

  /// See [`teamcraft_client_api::backend::ChatBackend::composer_link_preview`].
  pub(crate) fn ui_composer_link_preview(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) -> ComposerLinkPreview {
    let Some(draft) = read_preview(self.store_ref(), &draft_key(conversation, maybe_thread)) else {
      return ComposerLinkPreview::None;
    };
    match draft.state {
      PreviewState::Loading => ComposerLinkPreview::Loading { url: draft.url },
      PreviewState::Ready { preview } => {
        self
          .allowed_preview(preview)
          .map_or(ComposerLinkPreview::None, |preview| ComposerLinkPreview::Ready {
            preview: api_preview(preview),
          })
      },
      PreviewState::Failed { .. } | PreviewState::Removed | PreviewState::Unknown => {
        ComposerLinkPreview::None
      },
    }
  }

  /// See [`teamcraft_client_api::backend::ChatBackend::remove_composer_link_preview`].
  pub(crate) fn ui_remove_composer_link_preview(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) {
    let key = draft_key(conversation, maybe_thread);
    let Some(draft) = read_preview(self.store_ref(), &key) else {
      return;
    };
    let was_loading = draft.state == PreviewState::Loading;
    self.write_preview(
      &key,
      &DraftPreview {
        url: draft.url,
        state: PreviewState::Removed,
      },
    );
    if was_loading {
      self.fetch_preview(conversation, maybe_thread);
    }
  }

  /// The preview a message being sent carries under `choice`, from what the composer showed for
  /// its draft, which then starts afresh. `None` for [`OutgoingLinkPreview::MadeOnSend`], which
  /// the outbox makes instead.
  pub(crate) fn take_shown_preview(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
    choice: OutgoingLinkPreview,
  ) -> Option<LinkPreviewData> {
    if choice != OutgoingLinkPreview::AsShown {
      return None;
    }
    let key = draft_key(conversation, maybe_thread);
    let draft = read_preview(self.store_ref(), &key)?;
    let maybe_url = composer_url(body, &self.verified_policy().link_previews);
    self.forget_preview(conversation, maybe_thread, &key);
    match draft.state {
      // The composer showed it for this very link (the text may have changed since the last report).
      PreviewState::Ready { preview } if maybe_url.is_some_and(|url| url.as_str() == draft.url) => {
        self.allowed_preview(preview)
      },
      _ => None,
    }
  }

  fn start_preview(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    key: &str,
    url: &str,
  ) {
    self.write_preview(
      key,
      &DraftPreview {
        url: url.to_owned(),
        state: PreviewState::Loading,
      },
    );
    self.fetch_preview(conversation, maybe_thread);
  }

  /// Takes the draft's preview away and cancels its fetch.
  fn forget_preview(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, key: &str) {
    if let Err(error) = self.store_ref().delete(Table::Account, key) {
      tracing::debug!(%error, "could not forget the composer's link preview");
    }
    self.fetch_preview(conversation, maybe_thread);
  }

  fn fetch_preview(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) {
    self.command(CoreCommand::ComposerLinkChanged {
      conversation,
      maybe_thread,
    });
  }

  fn write_preview(&self, key: &str, draft: &DraftPreview) {
    if let Err(error) = records::put_json(self.store_ref(), Table::Account, key, draft) {
      tracing::debug!(%error, "could not keep the composer's link preview");
    }
  }

  fn is_retry_due(&self, failed_at_ms: i64) -> bool {
    let retry_ms = i64::try_from(RETRY_FAILED_AFTER.as_millis()).unwrap_or(i64::MAX);
    // A clock that went back counts as due: a wrong clock never keeps the preview away.
    let elapsed = self.now_ms().saturating_sub(failed_at_ms);
    !(0..retry_ms).contains(&elapsed)
  }

  /// The preview while the verified policy still allows its domain (it may have changed since).
  fn allowed_preview(&self, preview: LinkPreviewData) -> Option<LinkPreviewData> {
    let policy = self.verified_policy().link_previews;
    Url::parse(&preview.url)
      .is_ok_and(|url| policy.allows(&url))
      .then_some(preview)
  }
}

/// The draft's link the composer previews: its first one, when the policy allows its domain.
fn composer_url(body: &str, policy: &LinkPreviewPolicy) -> Option<Url> {
  first_url(body).filter(|url| policy.allows(url))
}

fn draft_key(conversation: ConversationId, maybe_thread: Option<ThreadId>) -> String {
  match maybe_thread {
    Some(thread) => format!("{KEY_PREFIX}{conversation}/{thread}"),
    None => format!("{KEY_PREFIX}{conversation}"),
  }
}

fn read_preview(store: &impl LocalStore, key: &str) -> Option<DraftPreview> {
  records::get_json(store, Table::Account, key).unwrap_or_else(|error| {
    tracing::debug!(%error, "could not read the composer's link preview");
    None
  })
}

/// Keeps what a fetch brought, unless the draft moved on meanwhile (another link, removed, sent);
/// whether it was kept.
fn finish(store: &impl LocalStore, key: &str, url: &str, state: PreviewState) -> bool {
  let is_still_wanted =
    read_preview(store, key).is_some_and(|draft| draft.url == url && draft.state == PreviewState::Loading);
  if !is_still_wanted {
    return false;
  }
  let draft = DraftPreview {
    url: url.to_owned(),
    state,
  };
  match records::put_json(store, Table::Account, key, &draft) {
    Ok(()) => true,
    Err(error) => {
      tracing::debug!(%error, "could not keep the composer's link preview");
      false
    },
  }
}

/// The verified workspace policy's link rules (the default before one was signed).
fn verified_link_policy(store: &impl LocalStore) -> LinkPreviewPolicy {
  let maybe_state: Option<crate::workspace::WorkspaceState> =
    records::get_json(store, Table::Workspace, crate::client::WORKSPACE_KEY).unwrap_or_default();
  maybe_state.unwrap_or_default().policy.link_previews
}

fn api_preview(preview: LinkPreviewData) -> LinkPreview {
  LinkPreview {
    url: preview.url,
    maybe_title: preview.maybe_title,
    maybe_description: preview.maybe_description,
    maybe_site_name: preview.maybe_site_name,
    maybe_image: preview.maybe_image.map(|image| image.as_slice().to_vec()),
  }
}
