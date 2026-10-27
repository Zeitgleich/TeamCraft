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
use teamcraft_client_api::actions::{FileFailure, FileInfo, FileTransfer, LinkPreview, PixelSize};
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::links::{LINKS_PER_PAGE, LinkInfo};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::areas::{self, Place, lazy_area};
use crate::channel_access::can_share_files;
use crate::clock::format_date_time;
use crate::components::attachment_menu;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::lightbox::prefers_reduced_motion;
use crate::components::message_actions::copy_to_clipboard;
use crate::components::sheet::PopoverBackdrop;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::{I18n, use_i18n};
use crate::lightbox::{MediaKey, MediaKind};
use crate::media_cache::{self, Rendition};
use crate::media_formats::animation_label;
use crate::media_layout::{
  EmbedImage, MAX_INLINE_VIDEO_BYTES, fit_picture, format_duration, inline_size, loads_inline, thumbnail_size,
};
use crate::model::{ConversationId, MessageId};
use crate::object_url::{FetchError, ObjectUrl};
use crate::prefs::ImageSize;
use crate::preview::{self, PreviewKind};
use crate::state::{AppState, Overlay, use_app_state};

/// Most tiles a grid shows; the last one counts the pictures it doesn't show ("+3").
const MAX_GRID_TILES: usize = 4;
/// From this size on, a transfer's progress is told in bytes ("12 MB / 48 MB"), below in percent.
const LARGE_TRANSFER_BYTES: u64 = 1024 * 1024;

/// The files attached to one message: pictures and videos inline (one large, several in a grid),
/// everything else as cards. Pictures still going up show where they will be, from their local
/// thumbnail.
#[component]
pub fn MessageFiles(files: Vec<FileInfo>) -> impl IntoView {
  // Files that finished uploading and are not transferring right now can be saved together.
  let downloadable: Vec<FileInfo> = files
    .iter()
    .filter(|file| {
      file.file_id.parse::<u64>().is_ok()
        && !matches!(
          file.transfer,
          FileTransfer::Uploading { .. }
            | FileTransfer::UploadStalled { .. }
            | FileTransfer::Downloading { .. }
        )
    })
    .cloned()
    .collect();
  let download_all = (downloadable.len() > 1).then(|| view! { <DownloadAll files=downloadable /> });
  let (media, cards): (Vec<FileInfo>, Vec<FileInfo>) = files
    .into_iter()
    .partition(|file| MediaKey::of(file).is_some() || is_media_upload(file));
  let media = match media.len() {
    0 => ().into_any(),
    1 => media
      .into_iter()
      .map(|file| tile(file, TileLayout::Single))
      .collect_view()
      .into_any(),
    _ => view! { <MediaGrid files=media /> }.into_any(),
  };
  view! {
    <div class="msg-files">
      {media}
      {cards.into_iter().map(|file| view! { <FileCard file=file /> }).collect_view()}
      {download_all}
    </div>
  }
}

/// "3 files · Download all" under several attachments, once more than one is on the server.
#[component]
fn DownloadAll(files: Vec<FileInfo>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = files.len();
  let download = move |_| {
    for file in &files {
      FileActions::new(state, file).download();
    }
  };
  view! {
    <div class="files-bar">
      <span class="files-count">{move || i18n.t_count("files-count", count)}</span>
      <button type="button" class="link-btn" on:click=download>
        <Icon name=IconName::Download />
        {move || i18n.t("files-download-all")}
      </button>
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
      tile(file, layout)
    })
    .collect_view();
  view! {
    <div class="media-grid" data-tiles=count.min(MAX_GRID_TILES).to_string()>
      {tiles}
    </div>
  }
}

/// A sent picture or video, or one still going up.
fn tile(file: FileInfo, layout: TileLayout) -> AnyView {
  match MediaKey::of(&file) {
    Some(_) => view! { <MediaTile file=file layout=layout /> }.into_any(),
    None => view! { <UploadTile file=file layout=layout /> }.into_any(),
  }
}

/// A picture or video of the user's own message that is going up, or failed to.
fn is_media_upload(file: &FileInfo) -> bool {
  MediaKind::of(&file.media_type).is_some()
    && matches!(
      file.transfer,
      FileTransfer::Uploading { .. } | FileTransfer::UploadStalled { .. } | FileTransfer::UploadFailed { .. }
    )
}

/// A picture of a message that is still going up: its local thumbnail where the sent picture will
/// be, at the same size, so nothing moves when it is sent. Over it a turning ring while it goes up,
/// or why it stopped and "Try again".
#[component]
fn UploadTile(file: FileInfo, layout: TileLayout) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let kind = MediaKind::of(&file.media_type).unwrap_or(MediaKind::Image);
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(ObjectUrl::from_thumbnail);
  let maybe_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the row leaves the screen.
  StoredValue::new_local(thumbnail);
  let maybe_thumbnail_size = file.maybe_thumbnail_png.as_deref().and_then(thumbnail_size);
  let maybe_dimensions = file.maybe_dimensions;
  let maybe_size = move || {
    (layout == TileLayout::Single)
      .then(|| {
        let image_size = state.settings.prefs.with(|prefs| prefs.display.image_size);
        tile_size(maybe_dimensions, maybe_thumbnail_size, image_size)
      })
      .flatten()
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
  let actions = FileActions::new(state, &file);
  let name = file.file_name.clone();
  let picture = match maybe_src {
    Some(src) => view! { <img src=src alt=name class="is-preview" draggable="false" /> }.into_any(),
    None => view! { <span class="media-placeholder"><Icon name=file_icon(Some(kind)) /></span> }.into_any(),
  };
  let status = match file.transfer {
    FileTransfer::Uploading {
      done_bytes,
      total_bytes,
      failed_attempts,
    // Quiet while it goes up or retries by itself (#742): a small ring in the corner around the
    // cancel button; the words are for screen readers.
    } if done_bytes < total_bytes || total_bytes == 0 => view! {
      <span class="upload-progress">
        <TransferRing done_bytes=done_bytes total_bytes=total_bytes />
        <span class="visually-hidden" role="status">{upload_label(i18n, done_bytes, total_bytes, failed_attempts)}</span>
        <CancelUploadButton actions=actions />
      </span>
    }
    .into_any(),
    FileTransfer::UploadStalled { reason } => view! {
      <span class="upload-state is-failed" role="alert">
        <span>{move || i18n.t(stall_key(reason, StallPlace::Tile))}</span>
        <span class="upload-actions">
          <button class="btn btn-small" on:click=move |_| actions.retry_upload()>
            <Icon name=IconName::Rotate />
            <span>{move || i18n.t("file-retry")}</span>
          </button>
          <CancelUploadButton actions=actions />
        </span>
      </span>
    }
    .into_any(),
    FileTransfer::UploadFailed { reason } => {
      let key = match reason {
        FileFailure::TooLarge => "file-too-large",
        // The browser no longer has the picked file (a reload without a kept copy), or it changed.
        FileFailure::LocalFile => "file-upload-unreadable",
        _ => "file-upload-failed",
      };
      view! {
        <span class="upload-state is-failed" role="alert">
          <span>{move || i18n.t(key)}</span>
        </span>
      }
      .into_any()
    },
    _ => ().into_any(),
  };
  let has_size = maybe_dimensions.is_some() || maybe_thumbnail_size.is_some();
  view! {
    <div
      class="media-tile is-uploading"
      class:media-single=layout == TileLayout::Single
      class:has-size=has_size
      style:--media-w=media_width
      style:--media-h=media_height
    >
      <span class="media-open">{picture}</span>
      {status}
    </div>
  }
}

/// How far a transfer got, as a ring; a turning one before the first bytes moved.
#[component]
fn TransferRing(done_bytes: u64, total_bytes: u64) -> impl IntoView {
  let percent = percent_of(done_bytes, total_bytes);
  match done_bytes {
    0 => view! { <span class="spinner" aria-hidden="true"></span> }.into_any(),
    _ => view! {
      <span class="transfer-ring" style:--media-progress=percent.to_string() aria-hidden="true"></span>
    }
    .into_any(),
  }
}

/// "Cancel" on a file that is still going up.
#[component]
fn CancelUploadButton(actions: FileActions) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      class="icon-btn upload-cancel"
      aria-label=move || i18n.t("file-cancel-upload")
      data-tip=move || i18n.t("file-cancel-upload")
      on:click=move |_| actions.cancel_upload()
    >
      <Icon name=IconName::Close />
    </button>
  }
}

/// A transfer's progress in words: bytes of large files ("12 MB / 48 MB"), percent of small ones.
fn transfer_label(i18n: I18n, done_bytes: u64, total_bytes: u64) -> String {
  if total_bytes >= LARGE_TRANSFER_BYTES {
    return i18n.t_with(
      "file-progress-bytes",
      &[
        ("done", &format_bytes(done_bytes)),
        ("total", &format_bytes(total_bytes)),
      ],
    );
  }
  i18n.t_with(
    "file-progress-percent",
    &[("percent", &percent_of(done_bytes, total_bytes).to_string())],
  )
}

/// An upload's progress in words; before its first bytes after a failed attempt, that it is retried.
fn upload_label(i18n: I18n, done_bytes: u64, total_bytes: u64, failed_attempts: u32) -> String {
  match (done_bytes, failed_attempts) {
    (0, 1..) => i18n.t("file-upload-retrying"),
    _ => transfer_label(i18n, done_bytes, total_bytes),
  }
}

fn percent_of(done_bytes: u64, total_bytes: u64) -> u64 {
  match total_bytes {
    0 => 0,
    total => u64::try_from(u128::from(done_bytes.min(total)) * 100 / u128::from(total)).unwrap_or(100),
  }
}

/// The inline size of a single picture: the sender's exact size, else its thumbnail's.
fn tile_size(
  maybe_dimensions: Option<PixelSize>,
  maybe_thumbnail_size: Option<(u32, u32)>,
  image_size: ImageSize,
) -> Option<(u32, u32)> {
  match (maybe_dimensions, maybe_thumbnail_size) {
    (Some(picture), _) => Some(fit_picture(picture.width, picture.height, image_size)),
    (None, Some((width, height))) => Some(inline_size(width, height, image_size)),
    (None, None) => None,
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
    maybe_content_sha256: file
      .maybe_content_sha256
      .as_deref()
      .and_then(teamcraft_client_api::media::sha256_from_hex),
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
  let maybe_menu = attachment_menu::use_attachment_menu();
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
    tile_size(maybe_dimensions, maybe_thumbnail_size, image_size)
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
  // A large picture or video on its way down shows how far it got.
  let downloading = file.with_value(|file| match file.transfer {
    FileTransfer::Downloading {
      done_bytes,
      total_bytes,
    } if total_bytes >= LARGE_TRANSFER_BYTES => Some((done_bytes, total_bytes)),
    _ => None,
  });
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
  let maybe_duration = file
    .with_value(|file| file.maybe_duration_ms)
    .map(format_duration);
  let playback = InlinePlayback::new(
    state,
    conversation,
    key,
    file.with_value(|file| file.media_type.clone()),
    // Videos play in the row only as a single tile (not in a grid) and when the player can hold
    // the whole file.
    kind == MediaKind::Video && is_single && size_bytes <= MAX_INLINE_VIDEO_BYTES,
  );
  let open_lightbox = move || {
    playback.stop();
    state.overlay.set(Overlay::Lightbox(conversation, key));
  };
  let tile = move || {
    let (name, label, badge) = (name.clone(), label.clone(), badge.clone());
    let player_label = label.clone();
    let maybe_duration = maybe_duration.clone();
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
          // A long-press shows the actions. A right click opens the app's menu in the desktop app
          // (#719); the browser keeps its own ("Save image as", "Copy image").
          if is_touch.get_value() {
            ev.prevent_default();
            is_pinned.update(|pinned| *pinned = !*pinned);
          } else {
            file.with_value(|file| attachment_menu::open_on_right_click(maybe_menu, &ev, file, false));
          }
        }
      >
        <button
          class="media-open"
          data-media-key=key.dom_key()
          aria-label=move || match playback.is_inline {
            true => i18n.t_with("video-play", &[("name", &label)]),
            false => i18n.t_with("media-open", &[("name", &label)]),
          }
          on:click=move |_| match playback.is_inline {
            true => playback.start(),
            false => open_lightbox(),
          }
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
          {maybe_duration.map(|duration| view! { <span class="media-duration" aria-hidden="true">{duration}</span> })}
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
        {downloading.map(|(done, total)| view! {
          <span class="upload-state">
            <TransferRing done_bytes=done total_bytes=total />
            <span class="upload-label" role="status">{transfer_label(i18n, done, total)}</span>
          </span>
        })}
        {move || playback.is_playing.get().then(|| view! { <InlineVideo playback=playback label=player_label.clone() on_expand=Callback::new(move |()| open_lightbox()) /> })}
        {match playback.is_inline {
          true => view! { <MediaToolbar file=file.get_value() on_expand=Callback::new(move |()| open_lightbox()) /> }.into_any(),
          false => view! { <MediaToolbar file=file.get_value() /> }.into_any(),
        }}
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

/// Playing a video inside its message row (#483): one at a time, paused when scrolled out of view.
#[derive(Clone, Copy)]
struct InlinePlayback {
  state: AppState,
  conversation: ConversationId,
  key: MediaKey,
  media_type: StoredValue<String>,
  /// The video is small enough to play in the row (else a click opens the lightbox).
  is_inline: bool,
  /// The player is on screen, loading or playing.
  is_playing: RwSignal<bool>,
  url: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
}

impl InlinePlayback {
  fn new(
    state: AppState,
    conversation: ConversationId,
    key: MediaKey,
    media_type: String,
    is_inline: bool,
  ) -> Self {
    let playback = Self {
      state,
      conversation,
      key,
      media_type: StoredValue::new(media_type),
      is_inline,
      is_playing: RwSignal::new(false),
      url: RwSignal::new_local(None),
    };
    // Another video started, or the lightbox opened: this one makes room.
    Effect::new(move |_| {
      if state.active_inline_video.get() != Some(key) {
        playback.is_playing.set(false);
      }
    });
    playback
  }

  /// Loads the decrypted file (the cache the lightbox shares) and plays it in the row.
  fn start(self) {
    let Self {
      state,
      conversation,
      key,
      is_playing,
      url,
      ..
    } = self;
    state.active_inline_video.set(Some(key));
    is_playing.set(true);
    if url.with_untracked(Option::is_some) {
      return;
    }
    let media_type = self.media_type.get_value();
    spawn_local(async move {
      let is_wanted = move || is_playing.try_get_untracked().unwrap_or(false);
      match media_cache::load(conversation, key, &media_type, Rendition::Full, None, is_wanted).await {
        Ok(Some(loaded)) => drop(url.try_set(Some(loaded))),
        Ok(None) => {},
        Err(error) => {
          tracing::debug!(%error, conversation_id = %conversation, "inline video not loaded");
          // The lightbox says what went wrong and offers a retry.
          if is_playing.try_set(false).is_none() {
            state.overlay.try_set(Overlay::Lightbox(conversation, key));
          }
        },
      }
    });
  }

  fn stop(self) {
    self.is_playing.set(false);
    if self.state.active_inline_video.get_untracked() == Some(self.key) {
      self.state.active_inline_video.set(None);
    }
  }
}

/// The player over a video tile; "expand" opens the lightbox.
#[component]
fn InlineVideo(playback: InlinePlayback, label: String, on_expand: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let video = NodeRef::<html::Video>::new();
  pause_when_hidden(video);
  // The browser can't play it (a codec it lacks): the lightbox explains.
  let on_error = Callback::new(move |()| on_expand.run(()));
  view! {
    <div class="media-video">
      {move || match playback.url.get() {
        Some(url) => {
          // The player's code loads with its first use (areas.rs).
          let src = url.as_str().to_owned();
          let label = label.clone();
          lazy_area(Place::Pane, move || areas::inline_video_player(src.clone(), video, label.clone(), on_error))
        },
        None => view! { <span class="spinner" role="status" aria-label=move || i18n.t("loader-loading")></span> }.into_any(),
      }}
    </div>
  }
}

/// The observer and the callback it needs kept alive.
type VisibilityWatch = (web_sys::IntersectionObserver, Closure<dyn Fn(js_sys::Array)>);

/// Pauses the video when it scrolls out of view: a message row's video must not play on, unheard of,
/// behind the list.
fn pause_when_hidden(video: NodeRef<html::Video>) {
  let observer: StoredValue<Option<VisibilityWatch>, LocalStorage> = StoredValue::new_local(None);
  Effect::new(move |_| {
    let Some(element) = video.get() else {
      return;
    };
    let target = element.clone();
    let callback = Closure::<dyn Fn(js_sys::Array)>::new(move |entries: js_sys::Array| {
      let is_hidden = entries
        .iter()
        .filter_map(|entry| entry.dyn_into::<web_sys::IntersectionObserverEntry>().ok())
        .any(|entry| !entry.is_intersecting());
      if is_hidden {
        drop(target.pause());
      }
    });
    if let Ok(created) = web_sys::IntersectionObserver::new(callback.as_ref().unchecked_ref()) {
      created.observe(&element);
      observer.set_value(Some((created, callback)));
    }
  });
  on_cleanup(move || {
    observer.update_value(|maybe_observer| {
      if let Some((created, _)) = maybe_observer.take() {
        created.disconnect();
      }
    });
  });
}

/// The inline rendition of one picture and what became of loading it.
#[derive(Clone, Copy)]
struct InlineLoad {
  conversation: ConversationId,
  key: MediaKey,
  media_type: StoredValue<String>,
  /// What the message names as the file's ciphertext hash: keys the persisted rendition (#749).
  maybe_content_sha256: Option<[u8; 32]>,
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
      maybe_content_sha256,
      full,
      is_broken,
      is_unavailable,
      has_thumbnail,
      ..
    } = self;
    let media_type = self.media_type.get_value();
    spawn_local(async move {
      let is_wanted = move || full.try_with_untracked(|_| ()).is_some();
      match media_cache::load(
        conversation,
        key,
        &media_type,
        Rendition::Inline,
        maybe_content_sha256,
        is_wanted,
      )
      .await
      {
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
fn MediaToolbar(file: FileInfo, #[prop(optional, into)] on_expand: Option<Callback<()>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let actions = FileActions::new(state, &file);
  let is_saved = actions.is_saved();
  view! {
    <div class="media-toolbar" role="group" aria-label=move || i18n.t("lightbox-toolbar")>
      {on_expand.map(|expand| view! {
        <ToolbarButton label="video-expand" icon=IconName::Maximize on_click=move || expand.run(()) />
      })}
      <ToolbarButton label="file-download" icon=IconName::Download on_click=move || actions.download() />
      {actions.can_share.then(|| view! { <ToolbarButton label="file-share" icon=IconName::Link on_click=move || actions.share() /> })}
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
  let maybe_menu = attachment_menu::use_attachment_menu();
  let menu_file = StoredValue::new(file.clone());
  let on_context_menu = move |ev: web_sys::MouseEvent| {
    let is_touch = crate::viewport::is_touch_primary();
    menu_file.with_value(|file| attachment_menu::open_on_right_click(maybe_menu, &ev, file, is_touch));
  };
  // Markdown, HTML and PDF files open in the app (#557) unless they are too large.
  let maybe_document_key = MediaKey::for_file(&file).filter(|_| {
    is_on_server
      && PreviewKind::of(&file.media_type, &file.file_name).is_some()
      && preview::is_within_limit(file.size_bytes)
  });
  let show_download = is_on_server
    && !matches!(
      file.transfer,
      FileTransfer::Uploading { .. } | FileTransfer::Downloading { .. }
    );
  view! {
    <div class="file-card" on:contextmenu=on_context_menu>
      {preview}
      <div class="file-meta">
        <span class="file-name" translate="no" title=file.file_name.clone()>{file.file_name.clone()}</span>
        <span class="file-size">{format_bytes(file.size_bytes)}</span>
        {status}
      </div>
      {maybe_document_key.map(|key| view! {
        <button
          class="icon-btn file-action"
          aria-label=move || i18n.t("file-preview")
          data-tip=move || i18n.t("file-preview")
          on:click=move |_| state.overlay.set(Overlay::FilePreview(conversation, key))
        >
          <Icon name=IconName::Eye />
        </button>
      })}
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
    FileTransfer::Uploading {
      done_bytes,
      total_bytes,
      failed_attempts,
    } => {
      let (done, total) = (*done_bytes, *total_bytes);
      view! {
        <span class="file-transfer">
          <progress class="file-progress" max=total.max(1) value=done aria-label=move || i18n.t("file-progress")></progress>
          <span class="file-state">{upload_label(i18n, done, total, *failed_attempts)}</span>
          <CancelUploadButton actions=actions />
        </span>
      }
      .into_any()
    },
    FileTransfer::Downloading {
      done_bytes,
      total_bytes,
    } => {
      let (done, total) = (*done_bytes, *total_bytes);
      view! {
        <span class="file-transfer">
          <progress class="file-progress" max=total.max(1) value=done aria-label=move || i18n.t("file-progress")></progress>
          <span class="file-state">{transfer_label(i18n, done, total)}</span>
        </span>
      }
      .into_any()
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
    FileTransfer::UploadStalled { reason } => {
      let key = stall_key(*reason, StallPlace::Card);
      view! {
      <span class="file-error" role="alert">{move || i18n.t(key)}</span>
      <button class="link-btn" on:click=move |_| actions.retry_upload()>{move || i18n.t("file-retry")}</button>
      <button class="link-btn" on:click=move |_| actions.cancel_upload()>{move || i18n.t("file-cancel-upload")}</button>
      }
      .into_any()
    },
    FileTransfer::UploadFailed { reason } => {
      let key = match reason {
        FileFailure::TooLarge => "file-too-large",
        // The browser no longer has the picked file (a reload without a kept copy), or it changed.
        FileFailure::LocalFile => "file-upload-unreadable",
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
  let menu = NodeRef::<html::Div>::new();
  let is_saved = actions.is_saved();
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
        <PopoverBackdrop on_close=Callback::new(move |()| is_open.set(false)) popover=menu />
        <div class="popover file-menu" role="menu" node_ref=menu aria-label=move || i18n.t("lightbox-more") on:keydown=on_keydown>
          <button class="menu-item" role="menuitem" on:click=item(FileActions::download)>
            <Icon name=IconName::Download />
            <span class="menu-label">{move || i18n.t("file-download")}</span>
          </button>
          {actions.can_share.then(|| view! {
            <button class="menu-item" role="menuitem" on:click=item(FileActions::share)>
              <Icon name=IconName::Link />
              <span class="menu-label">{move || i18n.t("file-share")}</span>
            </button>
          })}
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

/// Saves one file of a message. In the desktop app the system's save dialog asks where, and the
/// core decrypts the file to exactly that place (#719); the file's card shows the progress. In the
/// browser the core hands the decrypted file to the browser's download. A failure says so.
pub(crate) fn save_attachment(
  state: AppState,
  i18n: I18n,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  file_name: String,
) {
  if !crate::auth::tauri::is_available() {
    state.write(|backend| backend.download_attachment(conversation, message, &file_id));
    return;
  }
  spawn_local(async move {
    let args = serde_json::json!({
      "conversation": conversation,
      "message": message,
      "file_id": file_id,
      "file_name": file_name,
    });
    match crate::auth::tauri::call::<bool>("core_save_attachment", &args).await {
      Ok(true) => state.show_brief_toast(i18n.t("lightbox-saving")),
      // The user closed the dialog.
      Ok(false) => {},
      Err(error) => state.show_error(i18n.t("file-save-failed"), error),
    }
  });
}

/// What the toolbar, the card, the "…" menu and the right-click menu do with one file.
#[derive(Clone, Copy)]
pub(crate) struct FileActions {
  state: AppState,
  i18n: I18n,
  conversation: ConversationId,
  message: MessageId,
  file_id: StoredValue<String>,
  file_name: StoredValue<String>,
  media_type: StoredValue<String>,
  /// Share links are offered only to people who may make them (issue #691).
  can_share: bool,
}

impl FileActions {
  pub(crate) fn new(state: AppState, file: &FileInfo) -> Self {
    Self {
      state,
      i18n: use_i18n(),
      conversation: file.conversation,
      message: file.message,
      file_id: StoredValue::new(file.file_id.clone()),
      file_name: StoredValue::new(file.file_name.clone()),
      media_type: StoredValue::new(file.media_type.clone()),
      can_share: can_share_files(state, file.conversation),
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

  /// Saves the file: in the desktop app where the user picks in the save dialog, in the browser as
  /// its download (see [`save_attachment`]).
  pub(crate) fn download(self) {
    save_attachment(
      self.state,
      self.i18n,
      self.conversation,
      self.message,
      self.file_id.get_value(),
      self.file_name.get_value(),
    );
  }

  /// Whether "Copy image" applies: a picture the webview can draw.
  pub(crate) fn can_copy_image(self) -> bool {
    self
      .media_type
      .with_value(|media_type| crate::copy_image::can_copy(media_type))
  }

  /// Puts the picture on the clipboard (see [`crate::copy_image`]) and says how it went. Starts
  /// within the click: the web Clipboard API allows it only then.
  pub(crate) fn copy_image(self) {
    let picture = crate::copy_image::CopiedPicture {
      conversation: self.conversation,
      message: self.message,
      file_id: self.file_id.get_value(),
      media_type: self.media_type.get_value(),
    };
    let copying = crate::copy_image::copy(picture);
    let (state, i18n) = (self.state, self.i18n);
    spawn_local(async move {
      match copying.await {
        true => state.show_brief_toast(i18n.t("lightbox-image-copied")),
        false => state.show_error(
          i18n.t("lightbox-copy-failed"),
          "the picture could not be put on the clipboard".to_owned(),
        ),
      }
    });
  }

  /// Opens the options and the list of the file's share links.
  fn share(self) {
    let maybe_file = self
      .file_id
      .with_value(|file_id| file_id.parse::<teamcraft_proto::AttachmentId>().ok());
    if let Some(file) = maybe_file {
      self
        .state
        .overlay
        .set(Overlay::ShareFile(self.conversation, self.message, file));
    }
  }

  pub(crate) fn forward(self) {
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

  pub(crate) fn prefetch_permalink(self) {
    self
      .state
      .read(|backend| drop(backend.permalink(self.conversation, self.message)));
  }

  pub(crate) fn copy_message_link(self) {
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

  /// Stops the file's upload; the message goes out without it.
  fn cancel_upload(self) {
    let file_id = self.file_id.get_value();
    self
      .state
      .write(|backend| backend.cancel_upload(self.conversation, self.message, &file_id));
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

/// What the "Files & links" tab lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SharedKind {
  Files,
  Links,
}

impl SharedKind {
  const ALL: [Self; 2] = [Self::Files, Self::Links];

  fn label_key(self) -> &'static str {
    match self {
      Self::Files => "shared-kind-files",
      Self::Links => "shared-kind-links",
    }
  }
}

/// Everything shared in the conversation: the "Files & links" tab of the channel header.
#[component]
pub fn ChannelFiles(
  conversation: ConversationId,
  /// The panel's element ID, which its tab controls.
  panel_id: String,
  /// The element ID of the tab that labels the panel.
  labelled_by: String,
  /// Shows a message in the conversation (leaves this tab).
  on_show_message: Callback<MessageId>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let kind = RwSignal::new(SharedKind::Files);
  let files = Memo::new(move |_| state.read(|backend| backend.conversation_files(conversation)));
  view! {
    <div class="channel-files panel-scroll" role="tabpanel" id=panel_id aria-labelledby=labelled_by>
      <div class="segmented shared-kind" role="group" aria-label=move || i18n.t("shared-kind-label")>
        <SlidingIndicator />
        {SharedKind::ALL
          .into_iter()
          .map(|option| {
            view! {
              <button aria-pressed=move || (kind.get() == option).to_string() on:click=move |_| kind.set(option)>
                {move || i18n.t(option.label_key())}
              </button>
            }
          })
          .collect_view()}
      </div>
      {move || match kind.get() {
        SharedKind::Files => view! {
          <div class="files-list">
            <For each=move || files.get() key=|file| (file.message, file.file_id.clone()) let:file>
              <FileCard file=file />
            </For>
          </div>
          <Show when=move || files.get().is_empty() fallback=|| ()>
            <EmptyState icon=IconName::Paperclip title_key="files-empty-title" body_key="files-empty" />
          </Show>
        }
          .into_any(),
        SharedKind::Links => view! { <ChannelLinks conversation=conversation on_show_message=on_show_message /> }
          .into_any(),
      }}
    </div>
  }
}

/// The links shared in the conversation, newest first, a page more at a time.
#[component]
fn ChannelLinks(conversation: ConversationId, on_show_message: Callback<MessageId>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let limit = RwSignal::new(LINKS_PER_PAGE);
  let page = Memo::new(move |_| state.read(|backend| backend.conversation_links(conversation, limit.get())));
  view! {
    <ul class="links-list" aria-label=move || i18n.t("shared-kind-links")>
      <For each=move || page.get().links key=|link| (link.message, link.url.clone()) let:link>
        <LinkRow link=link on_show_message=on_show_message />
      </For>
    </ul>
    <Show when=move || page.with(|page| page.links.is_empty()) fallback=|| ()>
      <EmptyState icon=IconName::Link title_key="links-empty-title" body_key="links-empty" />
    </Show>
    <Show when=move || page.with(|page| page.has_more) fallback=|| ()>
      <div class="links-more">
        <button type="button" class="btn" on:click=move |_| limit.update(|limit| *limit += LINKS_PER_PAGE)>
          {move || i18n.t("links-more")}
        </button>
      </div>
    </Show>
  }
}

#[component]
fn LinkRow(link: LinkInfo, on_show_message: Callback<MessageId>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let sender = state
    .read(|backend| backend.user(link.sender))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let when = format_date_time(link.sent_at, i18n.locale().tag());
  let title = link
    .maybe_preview
    .as_ref()
    .and_then(|preview| preview.maybe_title.clone())
    .unwrap_or_else(|| link.url.clone());
  let message = link.message;
  view! {
    <li class="link-row">
      <span class="link-row-icon" aria-hidden="true"><Icon name=IconName::Link /></span>
      <div class="link-row-text">
        <a
          class="link-row-title"
          href=link.url.clone()
          target="_blank"
          rel="noopener noreferrer nofollow"
          translate="no"
          aria-label=move || i18n.t("link-open-new")
        >
          {title}
        </a>
        <span class="link-row-url" translate="no">{link.url.clone()}</span>
        <span class="link-row-meta"><span translate="no">{sender}</span>" · "<time>{when}</time></span>
      </div>
      <button
        type="button"
        class="btn btn-small"
        on:click=move |_| on_show_message.run(message)
      >
        {move || i18n.t("links-show-message")}
      </button>
    </li>
  }
}

fn file_icon(maybe_kind: Option<MediaKind>) -> IconName {
  match maybe_kind {
    Some(MediaKind::Video) => IconName::Video,
    Some(MediaKind::Image | MediaKind::Animated) => IconName::Image,
    None => IconName::File,
  }
}

/// Where a stalled upload is shown: over its picture (short) or on a file card (with the note
/// that the message waits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StallPlace {
  Tile,
  Card,
}

/// Why an upload stopped retrying by itself, in words (#742).
fn stall_key(reason: FileFailure, place: StallPlace) -> &'static str {
  match (reason, place) {
    (FileFailure::StorageUnavailable, _) => "file-upload-storage-unavailable",
    (_, StallPlace::Tile) => "file-upload-failed",
    (_, StallPlace::Card) => "file-upload-stalled",
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_stalled_upload_says_when_the_storage_is_down() {
    for place in [StallPlace::Tile, StallPlace::Card] {
      assert_eq!(
        stall_key(FileFailure::StorageUnavailable, place),
        "file-upload-storage-unavailable"
      );
    }
    assert_eq!(
      stall_key(FileFailure::Network, StallPlace::Tile),
      "file-upload-failed"
    );
    assert_eq!(
      stall_key(FileFailure::Network, StallPlace::Card),
      "file-upload-stalled"
    );
  }
}
