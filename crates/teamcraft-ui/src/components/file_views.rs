//! Files and link previews: the cards and image albums under a message and the channel's files tab.
//! Downloads run in the client core, which reports progress and the local path
//! (`FileInfo::transfer`); the page holds small thumbnails, and full images only while the lightbox
//! (`components::lightbox`) shows them.

use leptos::prelude::*;
use teamcraft_client_api::actions::{FileFailure, FileInfo, FileTransfer, LinkPreview, ShareLinkOptions};
use teamcraft_client_api::admin::format_bytes;

use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::i18n::use_i18n;
use crate::lightbox::{MediaKey, MediaKind};
use crate::model::ConversationId;
use crate::object_url::ObjectUrl;
use crate::state::{Overlay, use_app_state};

/// Most tiles an album shows; the last one counts the images it doesn't show ("+3").
const MAX_ALBUM_TILES: usize = 4;

/// The files attached to one message: several images form an album, everything else gets a card.
#[component]
pub fn MessageFiles(files: Vec<FileInfo>) -> impl IntoView {
  let is_album_image = |file: &FileInfo| MediaKey::of(file).is_some();
  let (album, cards): (Vec<FileInfo>, Vec<FileInfo>) =
    match files.iter().filter(|file| is_album_image(file)).count() {
      0 | 1 => (Vec::new(), files),
      _ => files.into_iter().partition(is_album_image),
    };
  view! {
    <div class="msg-files">
      {(!album.is_empty()).then(|| view! { <AlbumGrid images=album /> })}
      {cards.into_iter().map(|file| view! { <FileCard file=file /> }).collect_view()}
    </div>
  }
}

/// The images of a message as a grid of thumbnails; a tile opens the lightbox on its image.
#[component]
fn AlbumGrid(images: Vec<FileInfo>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let count = images.len();
  let hidden = count.saturating_sub(MAX_ALBUM_TILES);
  let tiles = images
    .into_iter()
    .take(MAX_ALBUM_TILES)
    .enumerate()
    .filter_map(|(index, file)| {
      let key = MediaKey::of(&file)?;
      // An image without a thumbnail still gets its tile, with a placeholder, so "+N" stays true.
      let maybe_url = file
        .maybe_thumbnail_png
        .as_deref()
        .and_then(|bytes| ObjectUrl::from_bytes(bytes, "image/png"));
      let maybe_src = maybe_url.as_ref().map(|url| url.as_str().to_owned());
      // Revoked when the row leaves the screen.
      StoredValue::new_local(maybe_url);
      let conversation = file.conversation;
      let (position, total, name) = ((index + 1).to_string(), count.to_string(), file.file_name);
      let maybe_more = (index + 1 == MAX_ALBUM_TILES && hidden > 0).then(|| format!("+{hidden}"));
      let is_video = MediaKind::of(&file.media_type) == Some(MediaKind::Video);
      Some(view! {
        <button
          class="album-tile"
          aria-label=move || i18n.t_with("album-open", &[("index", &position), ("count", &total), ("name", &name)])
          on:click=move |_| state.overlay.set(Overlay::Lightbox(conversation, key))
        >
          {match maybe_src {
            Some(src) => view! { <img src=src alt="" loading="lazy" /> }.into_any(),
            None => view! { <span class="album-placeholder"><Icon name=IconName::Image /></span> }.into_any(),
          }}
          {is_video.then(|| view! { <span class="media-play-badge" aria-hidden="true"><Icon name=IconName::Play /></span> })}
          {maybe_more.map(|more| view! { <span class="album-more" aria-hidden="true">{more}</span> })}
        </button>
      })
    })
    .collect_view();
  view! {
    <div class="msg-album" data-tiles=count.min(MAX_ALBUM_TILES).to_string()>
      {tiles}
    </div>
  }
}

/// One file: preview, name, size, transfer state and actions.
#[component]
pub fn FileCard(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let (conversation, message) = (file.conversation, file.message);
  let maybe_kind = MediaKind::of(&file.media_type);
  let maybe_key = MediaKey::of(&file);
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(|bytes| ObjectUrl::from_bytes(bytes, "image/png"));
  let maybe_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the card leaves the screen.
  StoredValue::new_local(thumbnail);
  let file_id = StoredValue::new(file.file_id.clone());
  let name = file.file_name.clone();
  let share_link = Memo::new(move |_| {
    file_id.with_value(|file_id| {
      state
        .read(|backend| backend.share_links(conversation, message, file_id))
        .into_iter()
        .next()
    })
  });
  let status = match &file.transfer {
    FileTransfer::Remote => view! {
      <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.download_file(conversation, message))>
        <Icon name=IconName::Download />
        <span>{move || i18n.t("file-download")}</span>
      </button>
    }
    .into_any(),
    FileTransfer::Uploading { done_bytes, total_bytes } | FileTransfer::Downloading { done_bytes, total_bytes } => {
      let (done, total) = (*done_bytes, (*total_bytes).max(1));
      view! {
        <progress max=total value=done aria-label=move || i18n.t("file-progress")></progress>
      }
      .into_any()
    },
    FileTransfer::Local { path } => view! { <span class="file-path" translate="no" title=path.clone()>{move || i18n.t("file-saved")}</span> }.into_any(),
    FileTransfer::Failed { reason } => {
      let key = match reason {
        FileFailure::TooLarge => "file-too-large",
        _ => "file-failed",
      };
      view! {
        <span class="file-error" role="alert">{move || i18n.t(key)}</span>
        <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.download_file(conversation, message))>
          {move || i18n.t("file-retry")}
        </button>
      }
      .into_any()
    },
  };
  view! {
    <div class="file-card">
      {match (maybe_src, maybe_key) {
        (Some(src), Some(key)) => view! {
          <button
            class="file-thumb"
            aria-label=move || i18n.t("file-preview")
            on:click=move |_| state.overlay.set(Overlay::Lightbox(conversation, key))
          >
            <img src=src alt=name.clone() loading="lazy" />
            {(maybe_kind == Some(MediaKind::Video)).then(|| view! { <span class="media-play-badge" aria-hidden="true"><Icon name=IconName::Play /></span> })}
          </button>
        }
        .into_any(),
        (Some(src), None) => view! { <span class="file-thumb is-static"><img src=src alt=name.clone() loading="lazy" /></span> }.into_any(),
        // A video or picture without a preview still opens in the lightbox.
        (None, Some(key)) => view! {
          <button
            class="file-icon file-open"
            aria-label=move || i18n.t("file-preview")
            on:click=move |_| state.overlay.set(Overlay::Lightbox(conversation, key))
          >
            <Icon name=file_icon(maybe_kind) />
          </button>
        }
        .into_any(),
        (None, None) => view! { <span class="file-icon"><Icon name=file_icon(maybe_kind) /></span> }.into_any(),
      }}
      <div class="file-meta">
        <span class="file-name" translate="no">{file.file_name.clone()}</span>
        <span class="file-size">{format_bytes(file.size_bytes)}</span>
      </div>
      <div class="file-actions">
        {status}
        {move || match share_link.get() {
          Some(link) => {
            let url = link.url.clone();
            view! {
              <button class="btn btn-small" on:click=move |_| copy_to_clipboard(&url)>
                <Icon name=IconName::Link />
                <span>{move || i18n.t("file-copy-share")}</span>
              </button>
            }
            .into_any()
          },
          None => view! {
            <button
              class="btn btn-small"
              on:click=move |_| file_id.with_value(|file_id| state.write(|backend| backend.create_share_link(conversation, message, file_id, ShareLinkOptions::default())))
            >
              <Icon name=IconName::Link />
              <span>{move || i18n.t("file-share")}</span>
            </button>
          }
          .into_any(),
        }}
      </div>
    </div>
  }
}

/// The card under a message that contains a link.
#[component]
pub fn LinkPreviewCard(preview: LinkPreview) -> impl IntoView {
  let i18n = use_i18n();
  let image = preview
    .maybe_image
    .as_deref()
    .and_then(|bytes| blob_url(bytes, "image/png"));
  let title = preview.maybe_title.clone().unwrap_or_else(|| preview.url.clone());
  // Only web links open from here; anything else would be a way to run a script.
  let is_web_link = preview.url.starts_with("https://") || preview.url.starts_with("http://");
  view! {
    <div class="link-preview">
      {image.map(|url| view! { <img class="preview-image" src=url alt="" loading="lazy" /> })}
      <div class="preview-text">
        {preview.maybe_site_name.map(|site| view! { <span class="preview-site" translate="no">{site}</span> })}
        {if is_web_link {
          view! {
            <a href=preview.url.clone() target="_blank" rel="noopener noreferrer nofollow" translate="no" aria-label=move || i18n.t("link-open-new")>
              {title}
            </a>
          }
          .into_any()
        } else {
          view! { <span translate="no">{title}</span> }.into_any()
        }}
        {preview.maybe_description.map(|text| view! { <span class="preview-description" translate="no">{text}</span> })}
      </div>
    </div>
  }
}

/// Everything shared in the conversation: the "Files" tab of the channel header.
#[component]
pub fn ChannelFiles(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let files = Memo::new(move |_| state.read(|backend| backend.conversation_files(conversation)));
  view! {
    <div class="channel-files panel-scroll" role="tabpanel" id="channel-tab-files" aria-label=move || i18n.t("files-title")>
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

/// A `blob:` URL for small image bytes; `None` where the browser refuses.
pub(crate) fn blob_url(bytes: &[u8], media_type: &str) -> Option<String> {
  let array = js_sys::Uint8Array::from(bytes);
  let parts = js_sys::Array::of1(&array);
  let options = web_sys::BlobPropertyBag::new();
  options.set_type(media_type);
  let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
  web_sys::Url::create_object_url_with_blob(&blob).ok()
}
