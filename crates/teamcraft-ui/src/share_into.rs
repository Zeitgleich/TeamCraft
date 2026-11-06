//! Sharing into the app from outside it (#208): the file manager's "Send to" on the desktop, and the
//! service worker's cache in the browser, which share sheets offer through the manifest's
//! `share_target`.
//!
//! What was shared waits outside the signed-in app (the shell's list of paths, the service worker's
//! cache in `sw.js`), so it survives the sign-in and, in the browser, a closed tab. Once signed in
//! the user picks a conversation; the share lands in that conversation's composer like anything
//! attached, and goes out, or waits in the outbox, only when they press send. Limits and thumbnails
//! are the composer's. Nothing here is logged but counts: names and text are the user's content.

use std::cell::Cell;
use std::time::Duration;

use leptos::prelude::*;
use serde::Deserialize;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::failure::ActionFailure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::media_formats::media_type_for;
use crate::model::{ConversationId, TimestampMs};
use crate::reactive::SetIfAlive;
use crate::settings::{load_string, store_string};
use crate::shell_drop::{self, ShellFile};
use crate::state::{AppState, Overlay, use_app_state};

/// The shell's event when files wait to be shared (`apps/desktop/src/shared_files.rs`).
const SHARED_FILES_EVENT: &str = "shared-files";
/// The service worker's cache of shares (`SHARE_CACHE` in `sw.js`).
const SHARE_CACHE: &str = "teamcraft-share-inbox";
/// Longest shared text taken into the composer.
const MAX_SHARED_TEXT_CHARS: usize = 64 * 1024;
/// How long the app may take to come up before the notice says a share waits for a sign-in.
const NOTICE_DELAY: Duration = Duration::from_millis(1500);
/// How many of the most recently active conversations the picker offers on top.
const RECENT_COUNT: usize = 5;
/// How much of a shared text the picker shows.
const SHOWN_TEXT_CHARS: usize = 200;
/// Conversations listed below them; the filter reaches the others.
const MAX_LISTED: usize = 100;
/// A share nobody picked a conversation for goes after this long (`SHARE_KEPT_MS` in `sw.js`).
const SHARE_KEPT_MS: TimestampMs = 7 * 24 * 60 * 60 * 1000;
/// `localStorage`: who last signed in here. A share survives a session that ran out, for that person.
const SHARE_OWNER_KEY: &str = "teamcraft.share-owner";

thread_local! {
  /// The app's inbox, for the sign-out, which outlives the components it was started from.
  static CURRENT: Cell<Option<ShareInbox>> = const { Cell::new(None) };
  /// Goes up when what waits is thrown away, so a read under way can't bring it back.
  static DISCARDS: DiscardCount = const { DiscardCount::new() };
}

/// What waits to be shared, and the way into the composer.
#[derive(Clone, Copy)]
pub struct ShareInbox {
  maybe_waiting: RwSignal<Option<IncomingShare>, LocalStorage>,
  maybe_picked: RwSignal<Option<PickedShare>, LocalStorage>,
  /// i18n key of a share that couldn't be received.
  maybe_failure: RwSignal<Option<&'static str>>,
  /// The signed-in app is up, so the picker can open.
  is_app_up: RwSignal<bool>,
  is_picker_open: RwSignal<bool>,
  /// A new share (or the notice's button) asks for the picker.
  wants_picker: RwSignal<bool>,
}

/// How often what waits was thrown away.
struct DiscardCount(Cell<u64>);

impl DiscardCount {
  const fn new() -> Self {
    Self(Cell::new(0))
  }

  fn current(&self) -> u64 {
    self.0.get()
  }

  fn bump(&self) {
    self.0.set(self.0.get().wrapping_add(1));
  }

  fn has_changed_since(&self, started: u64) -> bool {
    self.0.get() != started
  }
}

/// A share until the user picked a conversation for it.
#[derive(Clone)]
struct IncomingShare {
  text: String,
  files: Vec<web_sys::File>,
  in_shell: Vec<ShellFile>,
  left_out: usize,
  /// The service worker's ID of a share from the browser.
  maybe_browser_id: Option<String>,
}

/// A share on its way into the composer of `conversation`.
pub struct PickedShare {
  conversation: ConversationId,
  pub text: String,
  pub files: Vec<web_sys::File>,
  pub in_shell: Vec<ShellFile>,
}

/// The shell's `SharedFiles`.
#[derive(Deserialize)]
struct ShellShare {
  files: Vec<ShellFile>,
  left_out: usize,
}

/// What the service worker wrote about a share (`receiveShare` in `sw.js`).
#[derive(Deserialize)]
struct BrowserShare {
  title: String,
  text: String,
  url: String,
  files: Vec<BrowserShareFile>,
  left_out: usize,
}

#[derive(Deserialize)]
struct BrowserShareFile {
  path: String,
  name: String,
  media_type: String,
}

/// A conversation in the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShareTarget {
  id: ConversationId,
  name: String,
  is_notes: bool,
  maybe_last_activity: Option<TimestampMs>,
}

/// The picker's list: the notes and the recent ones on top, unless a filter is typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ShareSections {
  top: Vec<ShareTarget>,
  others: Vec<ShareTarget>,
}

impl ShareInbox {
  /// The context the app's screens reach this through.
  pub fn provide() -> Self {
    let inbox = Self {
      maybe_waiting: RwSignal::new_local(None),
      maybe_picked: RwSignal::new_local(None),
      maybe_failure: RwSignal::new(None),
      is_app_up: RwSignal::new(false),
      is_picker_open: RwSignal::new(false),
      wants_picker: RwSignal::new(false),
    };
    provide_context(inbox);
    CURRENT.set(Some(inbox));
    inbox
  }

  /// Takes what waits now and, on the desktop, what arrives later. A running web app gets a new
  /// share as a new page load: the service worker's answer is a navigation.
  pub fn listen(self) {
    if tauri::is_available() {
      tauri::listen_text(SHARED_FILES_EVENT, move |_| self.load());
    } else if take_failure_mark() {
      self.maybe_failure.set(Some("share-into-failed"));
    }
    self.load();
  }

  /// The signed-in app is up: a waiting share opens the picker, when no other dialog is open.
  pub fn connect_app(self, state: AppState) {
    self.is_app_up.set(true);
    on_cleanup(move || self.is_app_up.set_if_alive(false));
    Effect::new(move |_| {
      if self.wants_picker.get() && matches!(state.overlay.get(), Overlay::None) {
        self.wants_picker.set(false);
        state.overlay.set(Overlay::Share);
      }
    });
  }

  /// The share the user sent to this conversation's composer, once.
  pub fn take_picked_for(self, conversation: ConversationId) -> Option<PickedShare> {
    let is_for_here = self.maybe_picked.with(|picked| {
      picked
        .as_ref()
        .is_some_and(|picked| picked.conversation == conversation)
    });
    if !is_for_here {
      return None;
    }
    self.maybe_picked.try_update(Option::take).flatten()
  }

  fn load(self) {
    leptos::task::spawn_local(async move {
      let started = DISCARDS.with(DiscardCount::current);
      let loaded = match tauri::is_available() {
        true => next_shell_share().await,
        false => next_browser_share().await,
      };
      // async-ok: DISCARDS is a thread-local counter, not a signal.
      if DISCARDS.with(|discards| discards.has_changed_since(started)) {
        return;
      }
      match loaded {
        Ok(maybe_share) => self.show(maybe_share),
        Err(error) => {
          tracing::warn!(%error, "could not read a share from another app");
          self.maybe_failure.set_if_alive(Some("share-into-read-failed"));
        },
      }
    });
  }

  fn show(self, maybe_share: Option<IncomingShare>) {
    let is_new =
      maybe_share.is_some() && self.maybe_waiting.try_with_untracked(Option::is_none) == Some(true);
    self.maybe_waiting.set_if_alive(maybe_share);
    if is_new {
      self.wants_picker.set_if_alive(true);
    }
  }

  /// Sends the waiting share to the composer of `target`. The share is released only after its
  /// small files were read, so a failure leaves it waiting. The failure is the notice's, so what
  /// goes to the `PendingAction` that guards a second pick is always a success.
  async fn pick(self, state: AppState, i18n: I18n, target: ConversationId) -> Result<(), ActionFailure> {
    let Some(share) = self.maybe_waiting.try_get_untracked().flatten() else {
      return Ok(());
    };
    let mut files = share.files;
    let mut in_shell = Vec::new();
    let maybe_limit = state.try_read(|backend| backend.max_upload_bytes()).flatten();
    for file in share.in_shell {
      if let Some(limit) = maybe_limit.filter(|limit| file.size_bytes > *limit) {
        let args = [("name", file.name.as_str()), ("limit", &format_bytes(limit))];
        let text = i18n.t_with("composer-file-too-large", &args);
        state.show_error(text, "a shared file is larger than the upload limit".to_owned());
      } else if file.is_read_whole() {
        match shell_drop::read(&file).await {
          Ok(file) => files.push(file),
          Err(error) => {
            tracing::warn!(%error, "could not read a shared file");
            self.maybe_failure.set_if_alive(Some("share-into-read-failed"));
            return Ok(());
          },
        }
      } else {
        in_shell.push(file);
      }
    }
    self.maybe_picked.set_if_alive(Some(PickedShare {
      conversation: target,
      text: share.text,
      files,
      in_shell,
    }));
    self.release(share.maybe_browser_id).await;
    if state.overlay.try_get_untracked().is_some() {
      state.close_overlay();
      state.open_conversation(target);
    }
    Ok(())
  }

  /// Forgets the waiting share, and shows the next if the browser holds another.
  async fn release(self, maybe_browser_id: Option<String>) {
    self.maybe_waiting.set_if_alive(None);
    let result = match maybe_browser_id {
      Some(id) => delete_browser_share(&id).await,
      None => finish_shell_share().await,
    };
    if let Err(error) = result {
      tracing::warn!(%error, "could not delete a share that was handled");
    }
    if !tauri::is_available() {
      self.load();
    }
  }
}

/// `user` signed in: what waits from before goes if somebody else signed in last. They would see
/// another person's plaintext. A share made while nobody was known to be signed in stays.
pub async fn claim_for(user: &str) {
  if is_share_of_another(load_string(SHARE_OWNER_KEY).as_deref(), user) {
    let had_share = CURRENT
      .get()
      .is_some_and(|inbox| inbox.maybe_waiting.try_with_untracked(Option::is_some) == Some(true));
    discard_waiting().await;
    // async-ok: CURRENT is a thread-local cell; the signal inside is written with set_if_alive.
    if had_share && let Some(inbox) = CURRENT.get() {
      inbox.maybe_failure.set_if_alive(Some("share-into-other-account"));
    }
  }
  store_string(SHARE_OWNER_KEY, user);
}

/// The waiting share goes (sign-out): what was shared doesn't outlive the session on this device.
/// Nobody is the owner of what is shared after this.
pub async fn discard_waiting() {
  store_string(SHARE_OWNER_KEY, "");
  DISCARDS.with(DiscardCount::bump);
  if let Some(inbox) = CURRENT.get() {
    inbox.maybe_waiting.set_if_alive(None);
    // Also one on its way into a composer.
    inbox.maybe_picked.set_if_alive(None);
    inbox.wants_picker.set_if_alive(false);
  }
  let result = match tauri::is_available() {
    true => finish_shell_share().await,
    false => delete_browser_shares().await,
  };
  if let Err(error) = result {
    tracing::warn!(%error, "could not delete a waiting share at sign-out");
  }
}

/// Says that something waits for a conversation (sign-in or unlock first, when that's what is
/// missing), or that a share failed. A closed picker leaves its share here, never lost.
#[component]
pub fn ShareNotice(inbox: ShareInbox) -> impl IntoView {
  let i18n = use_i18n();
  // While the app comes up there's nothing to tell yet.
  let is_due = RwSignal::new(false);
  set_timeout(move || is_due.set_if_alive(true), NOTICE_DELAY);
  let discard = move |_| {
    let maybe_id = inbox
      .maybe_waiting
      .with_untracked(|share| share.as_ref().map(|share| share.maybe_browser_id.clone()));
    if let Some(maybe_id) = maybe_id {
      leptos::task::spawn_local(inbox.release(maybe_id));
    }
  };
  move || {
    if let Some(key) = inbox.maybe_failure.get() {
      return Some(
        view! {
          <div class="share-into-notice" role="alert">
            <span>{move || i18n.t(key)}</span>
            <button
              type="button"
              class="icon-btn"
              aria-label=move || i18n.t("close")
              on:click=move |_| inbox.maybe_failure.set(None)
            >
              <Icon name=IconName::Close />
            </button>
          </div>
        }
        .into_any(),
      );
    }
    let is_hidden = inbox.is_picker_open.get() || inbox.wants_picker.get() && inbox.is_app_up.get();
    if !is_due.get() || is_hidden || inbox.maybe_waiting.with(Option::is_none) {
      return None;
    }
    let is_app_up = inbox.is_app_up.get();
    let key = match is_app_up {
      true => "share-into-waiting-choose",
      false => "share-into-waiting-signed-out",
    };
    let choose = is_app_up.then(|| {
      view! {
        <button type="button" class="btn btn-small btn-primary" on:click=move |_| inbox.wants_picker.set(true)>
          {move || i18n.t("share-into-choose")}
        </button>
      }
    });
    Some(
      view! {
        <div class="share-into-notice" role="status">
          <span>{move || i18n.t(key)}</span>
          {choose}
          <button type="button" class="btn btn-small" on:click=discard>{move || i18n.t("share-into-discard")}</button>
        </div>
      }
      .into_any(),
    )
  }
}

/// Where the share goes: the notes and the recent conversations, then the rest, filterable. A pick
/// fills that conversation's composer; closing leaves the share waiting (`ShareNotice`).
#[component]
pub fn ShareDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let Some(inbox) = use_context::<ShareInbox>() else {
    return ().into_any();
  };
  inbox.is_picker_open.set(true);
  on_cleanup(move || inbox.is_picker_open.set_if_alive(false));
  let filter = RwSignal::new(String::new());
  let targets = Memo::new(move |_| state.read(share_targets));
  let sections = Memo::new(move |_| share_sections(targets.get(), &filter.get()));
  let pending = PendingAction::new();
  let shown_text = move || {
    inbox.maybe_waiting.with(|maybe_share| {
      maybe_share
        .as_ref()
        .map(|share| share.text.chars().take(SHOWN_TEXT_CHARS).collect::<String>())
        .filter(|text| !text.is_empty())
    })
  };
  let shown_text_view = move || shown_text().map(|text| view! { <p class="share-into-summary">{text}</p> });
  let shown_files = move || {
    inbox
      .maybe_waiting
      .with(|maybe_share| maybe_share.as_ref().map(listed_files).unwrap_or_default())
      .into_iter()
      .map(|(name, size)| {
        view! {
          <li>
            <span translate="no">{name}</span>
            <span class="share-into-size">{size}</span>
          </li>
        }
      })
      .collect_view()
  };
  let left_out = move || {
    let count = inbox
      .maybe_waiting
      .with(|share| share.as_ref().map_or(0, |share| share.left_out));
    (count > 0)
      .then(|| view! { <p class="share-into-summary">{i18n.t_count("share-into-left-out", count)}</p> })
  };
  let list = move |shown: Vec<ShareTarget>| {
    let notes = i18n.t("notes-title");
    view! {
      <ul class="forward-list">
        {shown
          .into_iter()
          .map(|target| {
            let id = target.id;
            let name = if target.is_notes { notes.clone() } else { target.name };
            view! {
              <li>
                <button
                  type="button"
                  class="forward-item"
                  prop:disabled=move || pending.is_pending()
                  on:click=move |_| pending.spawn(inbox.pick(state, i18n, id))
                >
                  <span translate="no">{name}</span>
                </button>
              </li>
            }
          })
          .collect_view()}
      </ul>
    }
  };
  view! {
    <Modal labelled_by="share-into-title" class="modal-small">
      <header class="modal-head">
        <h2 id="share-into-title">{move || i18n.t("share-into-title")}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p class="share-into-summary">{move || i18n.t("share-into-from-outside")}</p>
        {shown_text_view}
        <ul class="share-into-files">{shown_files}</ul>
        {left_out}
        <label class="visually-hidden" for="share-into-filter">{move || i18n.t("share-into-filter")}</label>
        <input
          id="share-into-filter"
          class="input"
          data-autofocus
          placeholder=move || i18n.t("share-into-filter")
          prop:value=move || filter.get()
          on:input=move |ev| filter.set(event_target_value(&ev))
        />
        <div class="share-into-targets">
          {move || {
            let ShareSections { top, others } = sections.get();
            if top.is_empty() && others.is_empty() {
              let key = match targets.with(Vec::is_empty) {
                true => "share-into-empty",
                false => "share-into-no-match",
              };
              return view! { <p class="share-into-summary">{i18n.t(key)}</p> }.into_any();
            }
            let top_title = (!top.is_empty() && filter.with(|text| text.trim().is_empty()))
              .then(|| view! { <h3 class="share-into-section-title">{i18n.t("share-into-recent")}</h3> });
            let others_title = (!top.is_empty() && !others.is_empty())
              .then(|| view! { <h3 class="share-into-section-title">{i18n.t("share-into-others")}</h3> });
            view! { {top_title} {list(top)} {others_title} {list(others)} }.into_any()
          }}
        </div>
      </div>
    </Modal>
  }
  .into_any()
}

/// A share's files as the picker lists them: name and size. A site or an app chose them, so the user
/// sees what would go out.
fn listed_files(share: &IncomingShare) -> Vec<(String, String)> {
  let picked = share
    .files
    .iter()
    .map(|file| (file.name(), format_bytes(file.size() as u64)));
  let in_shell = share
    .in_shell
    .iter()
    .map(|file| (file.name.clone(), format_bytes(file.size_bytes)));
  picked.chain(in_shell).collect()
}

/// The conversations the user may post in.
fn share_targets(backend: &dyn crate::backend::Backend) -> Vec<ShareTarget> {
  let mut all = Vec::new();
  for space in backend.spaces() {
    all.extend(backend.space_conversations(space.id));
  }
  all.extend(backend.direct_conversations());
  all
    .into_iter()
    .filter(|conversation| conversation.can_post)
    .map(|conversation| ShareTarget {
      id: conversation.id,
      maybe_last_activity: backend.last_activity(conversation.id),
      is_notes: conversation.is_notes_to_self,
      name: conversation.name,
    })
    .collect()
}

/// The notes and the [`RECENT_COUNT`] most recently active on top, the rest by name, at most
/// [`MAX_LISTED`]. While filtering, every match shows once by name.
fn share_sections(mut targets: Vec<ShareTarget>, filter: &str) -> ShareSections {
  let needle = filter.trim().to_lowercase();
  targets.sort_by_key(|target| target.name.to_lowercase());
  if !needle.is_empty() {
    targets.retain(|target| target.name.to_lowercase().contains(&needle));
    targets.truncate(MAX_LISTED);
    return ShareSections {
      top: Vec::new(),
      others: targets,
    };
  }
  let mut recent: Vec<ShareTarget> = targets
    .iter()
    .filter(|target| !target.is_notes && target.maybe_last_activity.is_some())
    .cloned()
    .collect();
  recent.sort_by_key(|target| std::cmp::Reverse(target.maybe_last_activity));
  recent.truncate(RECENT_COUNT);
  let mut top: Vec<ShareTarget> = targets.iter().filter(|target| target.is_notes).cloned().collect();
  top.extend(recent);
  let mut others: Vec<ShareTarget> = targets
    .into_iter()
    .filter(|target| !top.iter().any(|shown| shown.id == target.id))
    .collect();
  others.truncate(MAX_LISTED);
  ShareSections { top, others }
}

/// The text of a shared page or note: browsers send a page's title and address in separate fields,
/// and some repeat the address in the text.
fn share_text(title: &str, text: &str, url: &str) -> String {
  let (title, text, url) = (title.trim(), text.trim(), url.trim());
  let mut parts = Vec::new();
  if !title.is_empty() && !text.contains(title) && !url.contains(title) {
    parts.push(title);
  }
  if !text.is_empty() {
    parts.push(text);
  }
  if !url.is_empty() && !text.contains(url) {
    parts.push(url);
  }
  parts.join("\n").chars().take(MAX_SHARED_TEXT_CHARS).collect()
}

/// The name of a file another app sent: its last path segment, without control and direction
/// characters (which can make `fdp.exe` read as `exe.pdf`).
fn clean_file_name(name: &str) -> String {
  let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
  let clean: String = last
    .chars()
    .filter(|symbol| {
      !symbol.is_control()
        && !matches!(symbol, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
    .collect();
  match clean.trim() {
    "" | "." | ".." => "file".to_owned(),
    trimmed => trimmed.to_owned(),
  }
}

fn is_share_of_another(maybe_owner: Option<&str>, user: &str) -> bool {
  maybe_owner.is_some_and(|owner| owner != user)
}

/// Whether a share (its ID starts with the time it arrived) is past its time; an ID without a time
/// is nothing the service worker wrote.
fn is_share_expired(id: &str, now: TimestampMs) -> bool {
  let maybe_arrived = id
    .split('-')
    .next()
    .and_then(|time| time.parse::<TimestampMs>().ok());
  maybe_arrived.is_none_or(|arrived| now - arrived >= SHARE_KEPT_MS)
}

/// The ID of a share from the address of its list in the cache (`/share/<id>/meta`).
fn share_id_of_meta(address: &str) -> Option<&str> {
  let (_, rest) = address.split_once("/share/")?;
  rest
    .strip_suffix("/meta")
    .filter(|id| !id.is_empty() && !id.contains('/'))
}

async fn next_shell_share() -> Result<Option<IncomingShare>, String> {
  let waiting: ShellShare = tauri::call("shared_files_list", &serde_json::json!({})).await?;
  if waiting.files.is_empty() {
    if waiting.left_out > 0 {
      finish_shell_share().await?;
      return Err("none of the shared files can be read".to_owned());
    }
    return Ok(None);
  }
  Ok(Some(IncomingShare {
    text: String::new(),
    files: Vec::new(),
    in_shell: waiting
      .files
      .into_iter()
      .map(|file| ShellFile {
        name: clean_file_name(&file.name),
        ..file
      })
      .collect(),
    left_out: waiting.left_out,
    maybe_browser_id: None,
  }))
}

async fn finish_shell_share() -> Result<(), String> {
  tauri::call::<()>("shared_files_finish", &serde_json::json!({})).await
}

/// The oldest share the service worker holds, with its files as the browser's handles on disk.
async fn next_browser_share() -> Result<Option<IncomingShare>, String> {
  let Some(cache) = open_share_cache().await? else {
    return Ok(None);
  };
  let keys = await_js(cache.keys()).await?;
  let mut ids: Vec<String> = keys
    .unchecked_into::<js_sys::Array>()
    .iter()
    .filter_map(|key| {
      let address = key.dyn_into::<web_sys::Request>().ok()?.url();
      share_id_of_meta(&address).map(str::to_owned)
    })
    .collect();
  ids.sort();
  let now = now_ms();
  let mut maybe_id = None;
  for id in ids {
    if !is_share_expired(&id, now) {
      maybe_id = Some(id);
      break;
    }
    delete_browser_share(&id).await?;
  }
  let Some(id) = maybe_id else {
    return Ok(None);
  };
  let list = response_at(&cache, &format!("/share/{id}/meta")).await?;
  let text = await_js(list.text().map_err(js_error)?).await?;
  let meta: BrowserShare = serde_json::from_str(&text.as_string().unwrap_or_default())
    .map_err(|error| format!("the service worker's list is unreadable: {error}"))?;
  let mut files = Vec::with_capacity(meta.files.len());
  for file in &meta.files {
    files.push(file_at(&cache, file).await?);
  }
  let text = share_text(&meta.title, &meta.text, &meta.url);
  if text.is_empty() && files.is_empty() {
    delete_browser_share(&id).await?;
    return Ok(None);
  }
  Ok(Some(IncomingShare {
    text,
    files,
    in_shell: Vec::new(),
    left_out: meta.left_out,
    maybe_browser_id: Some(id),
  }))
}

async fn file_at(cache: &web_sys::Cache, file: &BrowserShareFile) -> Result<web_sys::File, String> {
  let response = response_at(cache, &file.path).await?;
  let blob = await_js(response.blob().map_err(js_error)?).await?;
  let name = clean_file_name(&file.name);
  let options = web_sys::FilePropertyBag::new();
  options.set_type(&media_type_for(&name, &file.media_type));
  let parts = js_sys::Array::of1(&blob);
  web_sys::File::new_with_blob_sequence_and_options(&parts, &name, &options).map_err(js_error)
}

async fn response_at(cache: &web_sys::Cache, path: &str) -> Result<web_sys::Response, String> {
  await_js(cache.match_with_str(path))
    .await?
    .dyn_into()
    .map_err(|_| "a share is incomplete".to_owned())
}

async fn delete_browser_share(id: &str) -> Result<(), String> {
  let Some(cache) = open_share_cache().await? else {
    return Ok(());
  };
  let prefix = format!("/share/{id}/");
  let keys = await_js(cache.keys()).await?.unchecked_into::<js_sys::Array>();
  for key in keys.iter() {
    let Ok(key) = key.dyn_into::<web_sys::Request>() else {
      continue;
    };
    if key.url().contains(&prefix) {
      await_js(cache.delete_with_request(&key)).await?;
    }
  }
  Ok(())
}

async fn delete_browser_shares() -> Result<(), String> {
  if let Some(caches) = share_caches() {
    await_js(caches.delete(SHARE_CACHE)).await?;
  }
  Ok(())
}

/// The cache, if the browser has it and a share was ever put there. Opening it only then keeps the
/// usual start free of it.
async fn open_share_cache() -> Result<Option<web_sys::Cache>, String> {
  let Some(caches) = share_caches() else {
    return Ok(None);
  };
  if await_js(caches.has(SHARE_CACHE)).await?.as_bool() != Some(true) {
    return Ok(None);
  }
  Ok(Some(await_js(caches.open(SHARE_CACHE)).await?.unchecked_into()))
}

/// None outside a secure context, where no service worker took a share either.
fn share_caches() -> Option<web_sys::CacheStorage> {
  web_sys::window()?.caches().ok()
}

async fn await_js(promise: js_sys::Promise) -> Result<wasm_bindgen::JsValue, String> {
  JsFuture::from(promise).await.map_err(js_error)
}

fn js_error(_error: wasm_bindgen::JsValue) -> String {
  // Not the value itself: a browser's message about a file can name it.
  "the browser refused to read the share".to_owned()
}

/// Whether the service worker's answer says it couldn't keep a share (`?share-failed`); takes the
/// mark out of the address, so a reload doesn't say it again.
fn take_failure_mark() -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let location = window.location();
  if location.search().ok().as_deref() != Some("?share-failed") {
    return false;
  }
  if let Ok(history) = window.history() {
    drop(history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/")));
  }
  true
}

#[cfg(test)]
mod tests {
  use super::*;

  mod sections {
    use super::*;

    #[test]
    fn the_notes_and_the_recent_ones_come_first_and_nothing_shows_twice() {
      let mut targets = vec![target(1, "notes-name", None), target(2, "Bravo", None)];
      targets[0].is_notes = true;
      targets.extend((3..10).map(|id| target(id, &format!("Recent {id}"), Some(id))));
      let sections = share_sections(targets, "");
      let top: Vec<i64> = sections.top.iter().map(id_of).collect();
      assert_eq!(top, [1, 9, 8, 7, 6, 5]);
      let others: Vec<i64> = sections.others.iter().map(id_of).collect();
      assert_eq!(others, [2, 3, 4], "by name: Bravo, then the older recent ones");
    }

    #[test]
    fn a_filter_shows_every_match_once_by_name_in_any_case() {
      let targets = vec![
        target(1, "Zebra team", Some(5)),
        target(2, "alpha team", None),
        target(3, "Other", Some(9)),
      ];
      let sections = share_sections(targets, "  TEAM ");
      assert!(sections.top.is_empty());
      let names: Vec<&str> = sections
        .others
        .iter()
        .map(|target| target.name.as_str())
        .collect();
      assert_eq!(names, ["alpha team", "Zebra team"]);
    }

    #[test]
    fn a_long_list_is_bounded_and_the_filter_reaches_the_rest() {
      let targets: Vec<ShareTarget> = (0..MAX_LISTED as i64 + 50)
        .map(|id| target(id, &format!("Channel {id:04}"), None))
        .collect();
      assert_eq!(share_sections(targets.clone(), "").others.len(), MAX_LISTED);
      assert_eq!(share_sections(targets, "0149").others.len(), 1);
    }

    fn target(id: i64, name: &str, maybe_last_activity: Option<TimestampMs>) -> ShareTarget {
      ShareTarget {
        id: id.to_string().parse().unwrap(),
        name: name.to_owned(),
        is_notes: false,
        maybe_last_activity,
      }
    }

    fn id_of(target: &ShareTarget) -> i64 {
      target.id.to_string().parse().unwrap()
    }
  }

  #[test]
  fn shared_text_joins_title_text_and_address_without_repeating_them() {
    assert_eq!(share_text("", "", ""), "");
    assert_eq!(share_text("", "hello", ""), "hello");
    assert_eq!(
      share_text("A page", "", "https://example.com/a"),
      "A page\nhttps://example.com/a"
    );
    assert_eq!(
      share_text("A page", "Look: https://example.com/a", "https://example.com/a"),
      "A page\nLook: https://example.com/a"
    );
    assert_eq!(share_text("hello", "hello there", ""), "hello there");
  }

  #[test]
  fn shared_text_is_bounded() {
    let long = "x".repeat(MAX_SHARED_TEXT_CHARS + 10);
    assert_eq!(share_text("", &long, "").chars().count(), MAX_SHARED_TEXT_CHARS);
  }

  #[test]
  fn file_names_lose_paths_and_tricks() {
    assert_eq!(clean_file_name("report.pdf"), "report.pdf");
    assert_eq!(clean_file_name("../../etc/passwd"), "passwd");
    assert_eq!(clean_file_name("C:\\Users\\someone\\a.txt"), "a.txt");
    assert_eq!(clean_file_name("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
    assert_eq!(clean_file_name(" a\n.txt\u{0} "), "a.txt");
    for hollow in ["", "   ", "..", "dir/", "\u{202e}"] {
      assert_eq!(clean_file_name(hollow), "file", "{hollow:?}");
    }
  }

  #[test]
  fn only_a_shares_list_names_a_share() {
    assert_eq!(
      share_id_of_meta("https://a.example/share/12-ab/meta"),
      Some("12-ab")
    );
    assert_eq!(share_id_of_meta("https://a.example/share/12-ab/file/0"), None);
    assert_eq!(share_id_of_meta("https://a.example/share//meta"), None);
    assert_eq!(share_id_of_meta("https://a.example/other"), None);
  }

  #[test]
  fn a_read_started_before_a_discard_is_dropped() {
    let discards = DiscardCount::new();
    let started = discards.current();
    assert!(!discards.has_changed_since(started));
    discards.bump();
    assert!(discards.has_changed_since(started));
    assert!(
      !discards.has_changed_since(discards.current()),
      "a read after it counts"
    );
  }

  #[test]
  fn only_somebody_else_than_the_last_user_loses_a_waiting_share() {
    assert!(is_share_of_another(Some("1"), "2"));
    assert!(!is_share_of_another(Some("1"), "1"));
    assert!(
      !is_share_of_another(None, "2"),
      "nobody signed in since the sign-out"
    );
  }

  #[test]
  fn a_share_expires_by_the_time_in_its_id() {
    let now = 1_000_000_000_000;
    assert!(!is_share_expired(&format!("{}-ab", now - SHARE_KEPT_MS + 1), now));
    assert!(is_share_expired(&format!("{}-ab", now - SHARE_KEPT_MS), now));
    assert!(is_share_expired("not-a-time", now));
    assert!(is_share_expired("", now));
  }

  #[test]
  fn the_service_workers_list_is_read() {
    let list = r#"{"title":"","text":"hi","url":"","left_out":2,
      "files":[{"path":"/share/1/file/0","name":"a.png","media_type":"image/png"}]}"#;
    let meta: BrowserShare = serde_json::from_str(list).unwrap();
    assert_eq!(meta.left_out, 2);
    assert_eq!(meta.files[0].path, "/share/1/file/0");
  }
}
