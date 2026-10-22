//! Files and link previews: the cards under a message, the files panel and the image gallery.
//! Bytes never pass through the page except small thumbnails: downloads run in the client core,
//! which reports progress and the local path (`FileInfo::transfer`).

use leptos::prelude::*;
use teamcraft_client_api::actions::{FileFailure, FileInfo, FileTransfer, LinkPreview, ShareLinkOptions};
use teamcraft_client_api::admin::format_bytes;

use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, MessageId};
use crate::state::{Overlay, SidePanel, use_app_state};

/// The files attached to one message.
#[component]
pub fn MessageFiles(files: Vec<FileInfo>) -> impl IntoView {
  view! {
    <div class="msg-files">
      {files.into_iter().map(|file| view! { <FileCard file=file /> }).collect_view()}
    </div>
  }
}

/// One file: preview, name, size, transfer state and actions.
#[component]
pub fn FileCard(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let (conversation, message) = (file.conversation, file.message);
  let is_image = file.media_type.starts_with("image/");
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(|bytes| blob_url(bytes, "image/png"));
  let file_id = file.file_id.clone();
  let name = file.file_name.clone();
  let share_link = Memo::new(move |_| {
    state
      .read(|backend| backend.share_links(conversation, message))
      .into_iter()
      .find(|link| link.file_id == file_id)
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
  let open_gallery = move |_| state.overlay.set(Overlay::Gallery(conversation, message));
  view! {
    <div class="file-card">
      {thumbnail
        .map(|url| {
          view! {
            <button class="file-thumb" aria-label=move || i18n.t("file-preview") on:click=open_gallery>
              <img src=url alt=name.clone() loading="lazy" />
            </button>
          }
          .into_any()
        })
        .unwrap_or_else(|| {
          view! { <span class="file-icon"><Icon name=if is_image { IconName::Image } else { IconName::File } /></span> }.into_any()
        })}
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
              on:click=move |_| state.write(|backend| backend.create_share_link(conversation, message, ShareLinkOptions::default()))
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

/// Everything shared in the conversation, with an image grid on top.
#[component]
pub fn FilesPanel(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let files = Memo::new(move |_| state.read(|backend| backend.conversation_files(conversation)));
  view! {
    <aside class="side-panel" aria-label=move || i18n.t("files-title")>
      <header class="panel-header">
        <div class="panel-title"><h2>{move || i18n.t("files-title")}</h2></div>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.panel.set(SidePanel::Closed)>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="panel-scroll files-list">
        <For each=move || files.get() key=|file| (file.message, file.file_id.clone()) let:file>
          <FileCard file=file />
        </For>
        <Show when=move || files.get().is_empty() fallback=|| ()>
          <p class="empty-state">{move || i18n.t("files-empty")}</p>
        </Show>
      </div>
    </aside>
  }
}

/// A larger look at the images of a conversation, with previous/next.
#[component]
pub fn Gallery(conversation: ConversationId, start: MessageId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let images = Memo::new(move |_| {
    state
      .read(|backend| backend.conversation_files(conversation))
      .into_iter()
      .filter(|file| file.media_type.starts_with("image/"))
      .collect::<Vec<_>>()
  });
  let index = RwSignal::new(None::<usize>);
  let current = move || {
    let list = images.get();
    let at = index
      .get()
      .unwrap_or_else(|| list.iter().position(|file| file.message == start).unwrap_or(0));
    list.get(at).cloned().map(|file| (at, list.len(), file))
  };
  let step = move |delta: isize| {
    let len = images.with(Vec::len) as isize;
    if len == 0 {
      return;
    }
    let at = current().map_or(0, |(at, _, _)| at) as isize;
    index.set(Some((at + delta).rem_euclid(len) as usize));
  };
  view! {
    <Modal labelled_by="gallery-title" class="modal-gallery">
      <div on:keydown=move |ev| match ev.key().as_str() {
        "ArrowLeft" => step(-1),
        "ArrowRight" => step(1),
        _ => {},
      }>
        <header class="modal-head">
          <h2 id="gallery-title" translate="no">
            {move || current().map(|(_, _, file)| file.file_name).unwrap_or_default()}
          </h2>
          <button class="icon-btn" data-autofocus aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="gallery-body">
          <button class="icon-btn" aria-label=move || i18n.t("gallery-previous") on:click=move |_| step(-1)>
            <Icon name=IconName::ChevronRight />
          </button>
          {move || {
            current().map(|(_, _, file)| {
              match file.maybe_thumbnail_png.as_deref().and_then(|bytes| blob_url(bytes, "image/png")) {
                Some(url) => view! { <img class="gallery-image" src=url alt=file.file_name.clone() /> }.into_any(),
                None => view! { <Icon name=IconName::Image /> }.into_any(),
              }
            })
          }}
          <button class="icon-btn" aria-label=move || i18n.t("gallery-next") on:click=move |_| step(1)>
            <Icon name=IconName::ChevronRight />
          </button>
        </div>
        <p class="gallery-count">{move || current().map(|(at, len, _)| format!("{} / {len}", at + 1)).unwrap_or_default()}</p>
      </div>
    </Modal>
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
