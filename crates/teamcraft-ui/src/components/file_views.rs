//! Files and link previews: inline pictures, picture grids and compact file cards under a message,
//! and the channel's files tab.
//!
//! - A picture shows inline at up to 360 px, in its aspect ratio: its thumbnail at once, the full
//!   file once it is downloaded and decrypted ([`crate::media_cache`], shared with the lightbox).
//!   Several pictures form a grid. A click opens the lightbox (`components::lightbox`).
//! - Actions float over a picture's top right on hover, focus or long-press; nothing is shown
//!   permanently. Other files are a compact card: icon, name, size, a download button and "…".
//!
//! Downloads to disk run in the client core, which reports progress and the local path
//! (`FileInfo::transfer`).

use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{FileFailure, FileInfo, FileTransfer, LinkPreview, ShareLinkOptions};
use teamcraft_client_api::admin::format_bytes;

use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::lightbox::prefers_reduced_motion;
use crate::components::message_actions::copy_to_clipboard;
use crate::i18n::{I18n, use_i18n};
use crate::lightbox::{MediaKey, MediaKind};
use crate::media_cache::{self, Rendition};
use crate::media_formats::animation_label;
use crate::media_layout::{EmbedImage, fit_picture, inline_size, loads_inline, thumbnail_size};
use crate::model::{ConversationId, MessageId};
use crate::object_url::{FetchError, ObjectUrl};
use crate::state::{AppState, Overlay, use_app_state};

/// Most tiles a grid shows; the last one counts the pictures it doesn't show ("+3").
const MAX_GRID_TILES: usize = 4;

/// The files attached to one message: pictures and videos inline (one large, several in a grid),
/// everything else as cards.
#[component]
pub fn MessageFiles(files: Vec<FileInfo>) -> impl IntoView {
  let (media, cards): (Vec<FileInfo>, Vec<FileInfo>) =
    files.into_iter().partition(|file| MediaKey::of(file).is_some());
  let media = match media.len() {
    0 => ().into_any(),
    1 => media
      .into_iter()
      .map(|file| view! { <MediaTile file=file layout=TileLayout::Single /> })
      .collect_view()
      .into_any(),
    _ => view! { <MediaGrid files=media /> }.into_any(),
  };
  view! {
    <div class="msg-files">
      {media}
      {cards.into_iter().map(|file| view! { <FileCard file=file /> }).collect_view()}
    </div>
  }
}

/// How a picture is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileLayout {
  /// Alone: its own aspect ratio within the inline box.
  Single,
  /// In a grid: a square crop; the last tile may count the hidden ones.
  Grid { maybe_hidden: Option<usize> },
}

/// Several pictures of a message as a grid; each tile has its own actions.
#[component]
fn MediaGrid(files: Vec<FileInfo>) -> impl IntoView {
  let count = files.len();
  let hidden = count.saturating_sub(MAX_GRID_TILES);
  let tiles = files
    .into_iter()
    .take(MAX_GRID_TILES)
    .enumerate()
    .map(|(index, file)| {
      let layout = TileLayout::Grid {
        maybe_hidden: (index + 1 == MAX_GRID_TILES && hidden > 0).then_some(hidden),
      };
      view! { <MediaTile file=file layout=layout /> }
    })
    .collect_view();
  view! {
    <div class="media-grid" data-tiles=count.min(MAX_GRID_TILES).to_string()>
      {tiles}
    </div>
  }
}

/// One picture or video: the thumbnail at once, the full picture when it is loaded, a click opens
/// the lightbox, and the actions float over it.
#[component]
fn MediaTile(file: FileInfo, layout: TileLayout) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // `MessageFiles` only puts files with a key here.
  let Some(key) = MediaKey::of(&file) else {
    return ().into_any();
  };
  let kind = MediaKind::of(&file.media_type).unwrap_or(MediaKind::Image);
  let conversation = file.conversation;
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(ObjectUrl::from_thumbnail);
  let maybe_thumbnail_size = file.maybe_thumbnail_png.as_deref().and_then(thumbnail_size);
  // The sender's size of the picture is exact; older messages only have the thumbnail's.
  let maybe_dimensions = file.maybe_dimensions;
  let thumbnail_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
  let has_thumbnail = thumbnail_src.is_some();
  // Revoked when the row leaves the screen.
  StoredValue::new_local(thumbnail);
  let is_motion_reduced = prefers_reduced_motion();
  let loads = loads_inline(kind, file.size_bytes, is_motion_reduced);
  // Nothing to show and nothing to load (a format this platform may not decode, too large to fetch
  // by itself): a file card instead of a grey box (issue #337). Videos keep their tile and play in
  // the lightbox.
  if !has_thumbnail && !loads && kind != MediaKind::Video {
    return view! { <FileCard file=file /> }.into_any();
  }
  let inline = InlineLoad {
    conversation,
    key,
    media_type: StoredValue::new(file.media_type.clone()),
    full: RwSignal::new_local(media_cache::cached(key, Rendition::Inline)),
    is_broken: RwSignal::new(false),
    is_unavailable: RwSignal::new(false),
    has_thumbnail,
  };
  let InlineLoad {
    full,
    is_broken,
    is_unavailable,
    ..
  } = inline;
  if full.get_untracked().is_none() && loads {
    inline.start();
  }
  // A WebP or APNG that turns out to move stays on its thumbnail under reduced motion, like a GIF.
  let shown_full = move || {
    full.with(|maybe_url| {
      maybe_url
        .as_ref()
        .filter(|url| !(is_motion_reduced && url.is_animated()))
        .map(|url| url.as_str().to_owned())
    })
  };
  let is_full = move || shown_full().is_some();
  let src = move || shown_full().or_else(|| thumbnail_src.clone());
  // The browser couldn't decode what it was given: the picture, or the thumbnail before the picture
  // was there. Either way this platform shows it as a file.
  let on_error = move |_| {
    if is_full() {
      // Back to the thumbnail if there is one; the lightbox explains the rest.
      full.set(None);
      is_broken.set(!has_thumbnail);
    } else if full.with_untracked(Option::is_none) {
      is_broken.set(true);
    }
  };
  // Long-press on touch screens opens the context menu event: it shows the actions instead.
  let is_pinned = RwSignal::new(false);
  let is_touch = StoredValue::new(false);
  let file = StoredValue::new(file);
  let name = file.with_value(|file| file.file_name.clone());
  let label = name.clone();
  // Follows the "Image size" setting. Set as CSSOM properties, not a `style` attribute: the CSP
  // (`style-src 'self'`) blocks those.
  let maybe_size = move || {
    if layout != TileLayout::Single {
      return None;
    }
    let image_size = state.settings.prefs.with(|prefs| prefs.display.image_size);
    match (maybe_dimensions, maybe_thumbnail_size) {
      (Some(picture), _) => Some(fit_picture(picture.width, picture.height, image_size)),
      (None, Some((width, height))) => Some(inline_size(width, height, image_size)),
      (None, None) => None,
    }
  };
  let media_width = move || {
    maybe_size()
      .map(|(width, _)| format!("{width}px"))
      .unwrap_or_default()
  };
  let media_height = move || {
    maybe_size()
      .map(|(_, height)| format!("{height}px"))
      .unwrap_or_default()
  };
  let maybe_hidden = match layout {
    TileLayout::Grid { maybe_hidden } => maybe_hidden,
    TileLayout::Single => None,
  };
  let is_single = layout == TileLayout::Single;
  let has_size = maybe_dimensions.is_some() || maybe_thumbnail_size.is_some();
  let badge = file.with_value(|file| animation_label(&file.media_type));
  let size_bytes = file.with_value(|file| file.size_bytes);
  let has_badge = move || {
    let is_still_gif =
      kind == MediaKind::Animated && (is_motion_reduced || !loads_inline(kind, size_bytes, false));
    let is_still_animation =
      full.with(|maybe_url| maybe_url.as_ref().is_some_and(|url| url.is_animated())) && is_motion_reduced;
    is_still_gif || is_still_animation
  };
  let tile = move || {
    let (name, label, badge) = (name.clone(), label.clone(), badge.clone());
    let src = src.clone();
    view! {
      <div
        class="media-tile"
        class:media-single=is_single
        class:has-size=has_size
        class:actions-pinned=move || is_pinned.get()
        style:--media-w=media_width
        style:--media-h=media_height
        on:pointerdown=move |ev: web_sys::PointerEvent| is_touch.set_value(ev.pointer_type() == "touch")
        on:contextmenu=move |ev: web_sys::MouseEvent| {
          // A long-press shows the actions; a right click keeps the browser's menu ("Save image as").
          if is_touch.get_value() {
            ev.prevent_default();
            is_pinned.update(|pinned| *pinned = !*pinned);
          }
        }
      >
        <button
          class="media-open"
          data-media-key=key.dom_key()
          aria-label=move || i18n.t_with("media-open", &[("name", &label)])
          on:click=move |_| state.overlay.set(Overlay::Lightbox(conversation, key))
        >
          {move || match src() {
            Some(src) => view! {
              <img
                src=src
                alt=name.clone()
                class:is-preview=move || !is_full()
                decoding="async"
                draggable="false"
                on:error=on_error
              />
            }
            .into_any(),
            None => view! { <span class="media-placeholder"><Icon name=file_icon(Some(kind)) /></span> }.into_any(),

          }}
          {(kind == MediaKind::Video).then(|| view! { <span class="media-play-badge" aria-hidden="true"><Icon name=IconName::Play /></span> })}
          {move || has_badge().then(|| view! { <span class="media-gif-badge" aria-hidden="true">{badge.clone()}</span> })}
          {maybe_hidden.map(|hidden| view! { <span class="media-more" aria-hidden="true">{format!("+{hidden}")}</span> })}
        </button>
        // Not loaded (offline, server trouble) and nothing else to show: try again on demand.
        {move || is_unavailable.get().then(|| view! {
          <button
            class="btn btn-small media-retry"
            on:click=move |_| {
              is_unavailable.set(false);
              inline.start();
            }
          >
            <Icon name=IconName::Rotate />
            <span>{move || i18n.t("file-retry")}</span>
          </button>
        })}
        <MediaToolbar file=file.get_value() />
      </div>
    }
  };
  view! {
    <Show when=move || !is_broken.get() fallback=move || view! { <FileCard file=file.get_value() /> }>
      {tile()}
    </Show>
  }
  .into_any()
}

/// The inline rendition of one picture and what became of loading it.
#[derive(Clone, Copy)]
struct InlineLoad {
  conversation: ConversationId,
  key: MediaKey,
  media_type: StoredValue<String>,
  full: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
  /// This platform can't decode it: shown as a file card from then on.
  is_broken: RwSignal<bool>,
  /// Not loaded and no thumbnail to show instead: the tile offers "Try again".
  is_unavailable: RwSignal<bool>,
  has_thumbnail: bool,
}

impl InlineLoad {
  /// Loads the rendition in the background and shows it once it is there; a row that left the
  /// screen in the meantime doesn't download it. Without a thumbnail, a file this platform can't
  /// decode becomes a file card, and one that didn't arrive (offline, server trouble) can be tried
  /// again.
  fn start(self) {
    let Self {
      conversation,
      key,
      full,
      is_broken,
      is_unavailable,
      has_thumbnail,
      ..
    } = self;
    let media_type = self.media_type.get_value();
    spawn_local(async move {
      let is_wanted = move || full.try_with_untracked(|_| ()).is_some();
      match media_cache::load(conversation, key, &media_type, Rendition::Inline, is_wanted).await {
        // The row may be gone by now: `try_set` drops the URL then.
        Ok(Some(url)) => drop(full.try_set(Some(url))),
        Ok(None) => {},
        // With a thumbnail it stays; the lightbox shows the error and a retry.
        Err(error) => {
          tracing::debug!(%error, conversation_id = %conversation, "inline picture not loaded");
          match error {
            _ if has_thumbnail => {},
            FetchError::Undecodable(_) => {
              is_broken.try_set(true);
            },
            FetchError::Unavailable(_) => {
              is_unavailable.try_set(true);
            },
          }
        },
      }
    });
  }
}

/// The floating actions over a picture: download, share link, forward, save, and "…".
#[component]
fn MediaToolbar(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let actions = FileActions::new(state, &file);
  let is_saved = actions.is_saved();
  view! {
    <div class="media-toolbar" role="group" aria-label=move || i18n.t("lightbox-toolbar")>
      <ToolbarButton label="file-download" icon=IconName::Download on_click=move || actions.download() />
      <ToolbarButton label="file-copy-share" icon=IconName::Link on_click=move || actions.share() />
      <ToolbarButton label="lightbox-forward" icon=IconName::Forward on_click=move || actions.forward() />
      <button
        class="icon-btn media-tool"
        class:is-active=move || is_saved.get()
        aria-pressed=move || is_saved.get().to_string()
        aria-label=move || if is_saved.get() { i18n.t("action-unsave") } else { i18n.t("action-save") }
        data-tip=move || if is_saved.get() { i18n.t("action-unsave") } else { i18n.t("action-save") }
        on:click=move |_| actions.toggle_saved()
      >
        <Icon name=IconName::Bookmark />
      </button>
      <FileMenu actions=actions />
    </div>
  }
}

#[component]
fn ToolbarButton(label: &'static str, icon: IconName, on_click: impl Fn() + 'static) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      class="icon-btn media-tool"
      aria-label=move || i18n.t(label)
      data-tip=move || i18n.t(label)
      on:click=move |_| on_click()
    >
      <Icon name=icon />
    </button>
  }
}

/// One file as a compact card: icon (or a small preview), name, size, transfer state, a download
/// button and "…". Pictures use it in the files panel; inline they are [`MediaTile`]s.
#[component]
pub fn FileCard(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let conversation = file.conversation;
  let maybe_kind = MediaKind::of(&file.media_type);
  let maybe_key = MediaKey::of(&file);
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(ObjectUrl::from_thumbnail);
  let maybe_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the card leaves the screen.
  StoredValue::new_local(thumbnail);
  // Files still uploading, or that failed to, aren't on the server: no download, no menu.
  let is_on_server = file.file_id.parse::<u64>().is_ok();
  let actions = FileActions::new(state, &file);
  let preview = match (maybe_src, maybe_key) {
    (Some(src), Some(key)) => view! {
      <button
        class="file-thumb"
        data-media-key=key.dom_key()
        aria-label=move || i18n.t("file-preview")
        on:click=move |_| state.overlay.set(Overlay::Lightbox(conversation, key))
      >
        <img src=src alt="" loading="lazy" />
      </button>
    }
    .into_any(),
    (Some(src), None) => {
      view! { <span class="file-thumb is-static"><img src=src alt="" loading="lazy" /></span> }.into_any()
    },
    (None, _) => {
      view! { <span class="file-icon" aria-hidden="true"><Icon name=file_icon(maybe_kind) /></span> }
        .into_any()
    },
  };
  let status = transfer_status(&file.transfer, actions);
  let show_download = is_on_server
    && !matches!(
      file.transfer,
      FileTransfer::Uploading { .. } | FileTransfer::Downloading { .. }
    );
  view! {
    <div class="file-card">
      {preview}
      <div class="file-meta">
        <span class="file-name" translate="no" title=file.file_name.clone()>{file.file_name.clone()}</span>
        <span class="file-size">{format_bytes(file.size_bytes)}</span>
        {status}
      </div>
      {show_download.then(|| view! {
        <button
          class="icon-btn file-action"
          aria-label=move || i18n.t("file-download")
          data-tip=move || i18n.t("file-download")
          on:click=move |_| actions.download()
        >
          <Icon name=IconName::Download />
        </button>
      })}
      {is_on_server.then(|| view! { <FileMenu actions=actions /> })}
    </div>
  }
}

/// The transfer state under a card's name; failures say why and offer a retry where one helps.
fn transfer_status(transfer: &FileTransfer, actions: FileActions) -> AnyView {
  let i18n = use_i18n();
  match transfer {
    FileTransfer::Remote => ().into_any(),
    FileTransfer::Uploading { done_bytes, total_bytes } | FileTransfer::Downloading { done_bytes, total_bytes } => {
      let (done, total) = (*done_bytes, (*total_bytes).max(1));
      view! { <progress class="file-progress" max=total value=done aria-label=move || i18n.t("file-progress")></progress> }.into_any()
    },
    FileTransfer::Local { path } => view! {
      <span class="file-state" translate="no" title=path.clone()>{move || i18n.t("file-saved")}</span>
    }
    .into_any(),
    FileTransfer::Failed { reason } => {
      let key = failure_key(*reason);
      view! {
        <span class="file-error" role="alert">{move || i18n.t(key)}</span>
        <button class="link-btn" on:click=move |_| actions.download()>{move || i18n.t("file-retry")}</button>
      }
      .into_any()
    },
    FileTransfer::UploadStalled { .. } => view! {
      <span class="file-error" role="alert">{move || i18n.t("file-upload-stalled")}</span>
      <button class="link-btn" on:click=move |_| actions.retry_upload()>{move || i18n.t("file-retry")}</button>
    }
    .into_any(),
    FileTransfer::UploadFailed { reason } => {
      let key = match reason {
        FileFailure::TooLarge => "file-too-large",
        _ => "file-upload-failed",
      };
      view! { <span class="file-error" role="alert">{move || i18n.t(key)}</span> }.into_any()
    },
  }
}

/// The "…" menu of a file: share link, forward, save, the link to the message.
#[component]
fn FileMenu(actions: FileActions) -> impl IntoView {
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let trigger = NodeRef::<html::Button>::new();
  let is_saved = actions.is_saved();
  let share_link = actions.share_link();
  let close = move || {
    is_open.set(false);
    if let Some(trigger) = trigger.get_untracked() {
      drop(trigger.focus());
    }
  };
  let toggle = move |_| {
    let is_opening = !is_open.get_untracked();
    is_open.set(is_opening);
    if is_opening {
      // Starts fetching the permalink so "Copy link to message" has it when it is clicked.
      actions.prefetch_permalink();
    }
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.key() == "Escape" {
      ev.stop_propagation();
      close();
    }
  };
  let item = move |run: fn(FileActions)| {
    move |_| {
      run(actions);
      close();
    }
  };
  view! {
    <div class="file-menu-wrap">
      <button
        class="icon-btn media-tool"
        node_ref=trigger
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || i18n.t("lightbox-more")
        data-tip=move || i18n.t("lightbox-more")
        on:click=toggle
      >
        <Icon name=IconName::More />
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <div class="popover-backdrop" on:mousedown=move |_| is_open.set(false)></div>
        <div class="popover file-menu" role="menu" aria-label=move || i18n.t("lightbox-more") on:keydown=on_keydown>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::download)>
            <Icon name=IconName::Download />
            <span class="menu-label">{move || i18n.t("file-download")}</span>
          </button>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::share)>
            <Icon name=IconName::Link />
            <span class="menu-label">
              {move || if share_link.get().is_some() { i18n.t("file-copy-share") } else { i18n.t("file-share") }}
            </span>
          </button>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::forward)>
            <Icon name=IconName::Forward />
            <span class="menu-label">{move || i18n.t("lightbox-forward")}</span>
          </button>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::toggle_saved)>
            <Icon name=IconName::Bookmark />
            <span class="menu-label">
              {move || if is_saved.get() { i18n.t("action-unsave") } else { i18n.t("action-save") }}
            </span>
          </button>
          <div class="menu-sep" role="separator"></div>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::copy_message_link)>
            <Icon name=IconName::Link />
            <span class="menu-label">{move || i18n.t("lightbox-copy-link")}</span>
          </button>
        </div>
      </Show>
    </div>
  }
}

/// What the toolbar, the card and the "…" menu do with one file.
#[derive(Clone, Copy)]
struct FileActions {
  state: AppState,
  i18n: I18n,
  conversation: ConversationId,
  message: MessageId,
  file_id: StoredValue<String>,
}

impl FileActions {
  fn new(state: AppState, file: &FileInfo) -> Self {
    Self {
      state,
      i18n: use_i18n(),
      conversation: file.conversation,
      message: file.message,
      file_id: StoredValue::new(file.file_id.clone()),
    }
  }

  fn is_saved(self) -> Memo<bool> {
    Memo::new(move |_| {
      self
        .state
        .read(|backend| backend.message_extras(self.conversation, &[self.message]))
        .into_iter()
        .next()
        .is_some_and(|extras| extras.is_saved)
    })
  }

  fn share_link(self) -> Memo<Option<String>> {
    Memo::new(move |_| {
      self.file_id.with_value(|file_id| {
        self
          .state
          .read(|backend| backend.share_links(self.conversation, self.message, file_id))
          .into_iter()
          .next()
          .map(|link| link.url)
      })
    })
  }

  /// The core downloads and decrypts the file again and saves it (the download folder on the
  /// desktop, the browser's download on the web).
  fn download(self) {
    self.file_id.with_value(|file_id| {
      self
        .state
        .write(|backend| backend.download_attachment(self.conversation, self.message, file_id));
    });
  }

  /// Copies the file's share link, creating one first if there is none yet.
  fn share(self) {
    let existing = self.file_id.with_value(|file_id| {
      self
        .state
        .read(|backend| backend.share_links(self.conversation, self.message, file_id))
        .into_iter()
        .next()
    });
    let key = match existing {
      Some(link) => {
        copy_to_clipboard(&link.url);
        "lightbox-share-copied"
      },
      None => {
        self.file_id.with_value(|file_id| {
          self.state.write(|backend| {
            backend.create_share_link(
              self.conversation,
              self.message,
              file_id,
              ShareLinkOptions::default(),
            )
          });
        });
        "lightbox-share-created"
      },
    };
    self.state.show_brief_toast(self.i18n.t(key));
  }

  fn forward(self) {
    self
      .state
      .overlay
      .set(Overlay::Forward(self.conversation, self.message));
  }

  fn toggle_saved(self) {
    let is_saved = self
      .state
      .read(|backend| backend.message_extras(self.conversation, &[self.message]))
      .into_iter()
      .next()
      .is_some_and(|extras| extras.is_saved);
    self
      .state
      .write(|backend| backend.set_saved(self.conversation, self.message, !is_saved));
    let key = match is_saved {
      true => "lightbox-saved-removed",
      false => "lightbox-saved-added",
    };
    self.state.show_brief_toast(self.i18n.t(key));
  }

  fn prefetch_permalink(self) {
    self
      .state
      .read(|backend| drop(backend.permalink(self.conversation, self.message)));
  }

  fn copy_message_link(self) {
    let key = match self
      .state
      .read(|backend| backend.permalink(self.conversation, self.message))
    {
      Some(link) => {
        copy_to_clipboard(&link);
        "lightbox-link-copied"
      },
      None => "lightbox-link-unavailable",
    };
    self.state.show_brief_toast(self.i18n.t(key));
  }

  /// "Retry" on an upload that stopped retrying by itself; the message waits for it.
  fn retry_upload(self) {
    self
      .state
      .write(|backend| backend.retry_send(self.conversation, self.message));
  }
}

/// The card under a message that contains a link.
#[component]
pub fn LinkPreviewCard(preview: LinkPreview) -> impl IntoView {
  let i18n = use_i18n();
  let layout = EmbedImage::of(preview.maybe_image.as_deref().and_then(thumbnail_size));
  let picture = preview.maybe_image.as_deref().and_then(ObjectUrl::from_thumbnail);
  let image = picture.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the card leaves the screen; every render of a row made a new one (issue #399).
  StoredValue::new_local(picture);
  let title = preview.maybe_title.clone().unwrap_or_else(|| preview.url.clone());
  // Only web links open from here; anything else would be a way to run a script.
  let is_web_link = preview.url.starts_with("https://") || preview.url.starts_with("http://");
  // The box is reserved from the header's ratio, so the card doesn't move when the picture loads.
  let wide_image = image.clone().zip(match layout {
    EmbedImage::Wide { aspect_ratio } => Some(aspect_ratio),
    EmbedImage::Thumbnail => None,
  });
  let has_wide_image = wide_image.is_some();
  let thumbnail = match layout {
    EmbedImage::Thumbnail => image,
    EmbedImage::Wide { .. } => None,
  };
  view! {
    <div class="embed-card" class:has-wide-image=has_wide_image>
      <div class="embed-text">
        {preview.maybe_site_name.map(|site| view! { <span class="embed-site" translate="no">{site}</span> })}
        {if is_web_link {
          view! {
            <a class="embed-title" href=preview.url.clone() target="_blank" rel="noopener noreferrer nofollow" translate="no" aria-label=move || i18n.t("link-open-new")>
              {title}
            </a>
          }
          .into_any()
        } else {
          view! { <span class="embed-title" translate="no">{title}</span> }.into_any()
        }}
        {preview.maybe_description.map(|text| view! { <span class="embed-description" translate="no">{text}</span> })}
      </div>
      {thumbnail.map(|url| view! { <img class="embed-thumb" src=url alt="" loading="lazy" /> })}
      {wide_image
        .map(|(url, ratio)| {
          view! {
            <img class="embed-wide" src=url alt="" loading="lazy" style:aspect-ratio=ratio.to_string() />
          }
        })}
    </div>
  }
}

/// Everything shared in the conversation: the "Files" tab of the channel header.
#[component]
pub fn ChannelFiles(
  conversation: ConversationId,
  /// The panel's element ID, which its tab controls.
  panel_id: String,
  /// The element ID of the tab that labels the panel.
  labelled_by: String,
) -> impl IntoView {
  let state = use_app_state();
  let files = Memo::new(move |_| state.read(|backend| backend.conversation_files(conversation)));
  view! {
    <div class="channel-files panel-scroll" role="tabpanel" id=panel_id aria-labelledby=labelled_by>
      <div class="files-list">
        <For each=move || files.get() key=|file| (file.message, file.file_id.clone()) let:file>
          <FileCard file=file />
        </For>
      </div>
      <Show when=move || files.get().is_empty() fallback=|| ()>
        <EmptyState icon=IconName::Paperclip title_key="files-empty-title" body_key="files-empty" />
      </Show>
    </div>
  }
}

fn file_icon(maybe_kind: Option<MediaKind>) -> IconName {
  match maybe_kind {
    Some(MediaKind::Video) => IconName::Video,
    Some(MediaKind::Image | MediaKind::Animated) => IconName::Image,
    None => IconName::File,
  }
}

fn failure_key(reason: FileFailure) -> &'static str {
  match reason {
    FileFailure::TooLarge => "file-too-large",
    _ => "file-failed",
  }
}

/// A `blob:` URL for small image bytes; `None` where the browser refuses.
pub(crate) fn blob_url(bytes: &[u8], media_type: &str) -> Option<String> {
  let array = js_sys::Uint8Array::from(bytes);
  let parts = js_sys::Array::of1(&array);
  let options = web_sys::BlobPropertyBag::new();
  options.set_type(media_type);
  let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
  web_sys::Url::create_object_url_with_blob(&blob).ok()
}
