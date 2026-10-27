//! The in-app preview of a Markdown, HTML, PDF or text attachment (#557, #821). It works from the
//! file the client core has already downloaded and decrypted (`object_url::fetch_decrypted`);
//! nothing leaves the client. The file is untrusted, so each kind is shown in a way that can't run
//! its code or reach the app (see `preview.rs`):
//!
//! - Markdown: the app's own renderer, a tree of elements.
//! - HTML: a sandboxed frame with no permissions and its own policy; its links are listed next to
//!   it and open in the system browser.
//! - PDF: the vendored PDF.js (`pdf-viewer.mjs`), loaded when the first PDF opens (`pdf_pane.rs`).
//! - Text, logs and data files: plain text in the text viewer (`text_viewer.rs`).
//!
//! A file above the preview limit (a setting, `PreviewLimit`) is not loaded: the preview says how
//! large it is and offers the download.

use std::rc::Rc;

use js_sys::Uint8Array;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::remote::AttachmentPriority;
use wasm_bindgen::JsCast;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::pdf_pane::pdf_pane;
use crate::components::rich_text::RichText;
use crate::components::text_viewer::text_viewer;
use crate::i18n::{I18n, use_i18n};
use crate::lightbox::MediaKey;
use crate::model::ConversationId;
use crate::object_url::fetch_decrypted;
use crate::preview::{self, PreviewKind};
use crate::reactive::SetIfAlive;
use crate::settings::PreviewLimit;
use crate::state::{AppState, use_app_state};

/// What the preview is showing.
#[derive(Clone)]
enum Load {
  /// Above the preview limit: not loaded at all.
  TooLarge,
  Loading,
  /// Not downloaded or decrypted (offline, server trouble).
  Unavailable,
  Ready(Content),
}

#[derive(Clone)]
enum Content {
  /// Markdown and HTML.
  Text(Rc<String>),
  /// PDF and the text viewer's files, which read the bytes themselves.
  Bytes(Uint8Array),
}

/// The preview of one file of a conversation.
#[component]
pub fn FilePreview(conversation: ConversationId, key: MediaKey) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_previewed = file_of(state, conversation, key)
    .and_then(|file| PreviewKind::of(&file.media_type, &file.file_name).map(|kind| (file, kind)));
  let Some((file, kind)) = maybe_previewed else {
    // Gone (deleted) or not previewable: nothing to show.
    state.close_overlay();
    return ().into_any();
  };
  let limit = PreviewLimit::load();
  let is_too_large = !limit.allows(file.size_bytes);
  let phase: RwSignal<Load, LocalStorage> = RwSignal::new_local(if is_too_large {
    Load::TooLarge
  } else {
    Load::Loading
  });
  let is_source = RwSignal::new(false);
  let file_id = StoredValue::new(file.file_id.clone());
  let message = key.message;

  let start = move || {
    phase.set(Load::Loading);
    let file_id = file_id.get_value();
    spawn_local(async move {
      let loaded = fetch_decrypted(conversation, message, file_id, AttachmentPriority::Now).await;
      phase.set_if_alive(match loaded {
        Ok(bytes) if kind.has_source() => {
          Load::Ready(Content::Text(Rc::new(preview::text_of(&bytes.to_vec()))))
        },
        Ok(bytes) => Load::Ready(Content::Bytes(bytes)),
        Err(error) => {
          tracing::warn!(%error, "a file could not be loaded for its preview");
          Load::Unavailable
        },
      });
    });
  };
  if !is_too_large {
    start();
  }

  let download = Callback::new(move |()| {
    file_id.with_value(|file_id| {
      state.write(|backend| backend.download_attachment(conversation, message, file_id));
    });
  });
  let name = file.file_name.clone();
  let size = format_bytes(file.size_bytes);
  let too_large = i18n.t_with("preview-too-large", &[("size", &size), ("limit", &limit.label())]);
  let viewer_name = StoredValue::new(name.clone());
  let toggles_source = kind.has_source();
  let body = move || {
    match phase.get() {
    Load::TooLarge => view! {
      <div class="preview-status" role="status">
        <p class="preview-status-title">{move || i18n.t("preview-too-large-title")}</p>
        <p>{too_large.clone()}</p>
        <button class="btn btn-primary" on:click=move |_| download.run(())>{move || i18n.t("preview-download-to-open")}</button>
      </div>
    }
    .into_any(),
    Load::Loading => view! {
      <div class="preview-status" role="status" aria-busy="true"><span class="spinner" aria-hidden="true"></span></div>
    }
    .into_any(),
    Load::Unavailable => view! {
      <div class="preview-status" role="alert">
        <p>{move || i18n.t("preview-unavailable")}</p>
        <button class="btn" on:click=move |_| start()>{move || i18n.t("file-retry")}</button>
      </div>
    }
    .into_any(),
    Load::Ready(Content::Bytes(bytes)) => match kind {
      PreviewKind::Text(format) => text_viewer(state, i18n, bytes, format, viewer_name.get_value(), download),
      _ => pdf_pane(i18n, bytes, download),
    },
    Load::Ready(Content::Text(text)) => match (kind, is_source.get()) {
      (_, true) => view! { <pre class="preview-source" translate="no">{text.as_str().to_owned()}</pre> }.into_any(),
      (PreviewKind::Html, false) => html_pane(i18n, &text),
      _ => view! { <div class="preview-markdown"><RichText body=text.as_str().to_owned() /></div> }.into_any(),
    },
  }
  };
  // Logs and data are wide: the text viewer gets more room.
  let class = if matches!(kind, PreviewKind::Text(_)) {
    "file-preview is-text"
  } else {
    "file-preview"
  };

  view! {
    <Modal labelled_by="file-preview-title" class=class>
      <header class="modal-head preview-head">
        <h2 id="file-preview-title" class="preview-name" translate="no" title=name.clone()>{name.clone()}</h2>
        <span class="preview-size">{size}</span>
        <div class="preview-tools">
          {toggles_source.then(|| view! {
            <button class="btn btn-small" aria-pressed=move || is_source.get().to_string() on:click=move |_| is_source.update(|on| *on = !*on)>
              {move || if is_source.get() { i18n.t("preview-show-formatted") } else { i18n.t("preview-show-source") }}
            </button>
          })}
          <button class="icon-btn" aria-label=move || i18n.t("file-download") data-tip=move || i18n.t("file-download") on:click=move |_| download.run(())>
            <Icon name=IconName::Download />
          </button>
          <button class="icon-btn" aria-label=move || i18n.t("close") data-autofocus on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </div>
      </header>
      <div class="preview-body">{body}</div>
    </Modal>
  }
  .into_any()
}

fn file_of(state: AppState, conversation: ConversationId, key: MediaKey) -> Option<FileInfo> {
  state
    .read(|backend| backend.message_extras(conversation, &[key.message]))
    .into_iter()
    .next()?
    .files
    .into_iter()
    .find(|file| key.matches(file))
}

/// An HTML file: the sandboxed frame, and below it the file's links, which open in the system
/// browser.
fn html_pane(i18n: I18n, html: &str) -> AnyView {
  let host = NodeRef::<html::Div>::new();
  let document = preview::html_frame_document(html);
  Effect::new(move |_| {
    if let Some(host) = host.get() {
      mount_frame(&host, &document);
    }
  });
  let links = links_of(html);
  let count = links.len();
  view! {
    <p class="preview-note">{move || i18n.t("preview-html-note")}</p>
    <div class="preview-frame-host" node_ref=host></div>
    {(count > 0).then(|| view! {
      <details class="preview-links">
        <summary>{move || i18n.t_count("preview-links", count)}</summary>
        <ul>
          {links
            .into_iter()
            .map(|url| view! {
              <li><a href=url.clone() target="_blank" rel="noopener noreferrer" translate="no">{url.clone()}</a></li>
            })
            .collect_view()}
        </ul>
      </details>
    })}
  }
  .into_any()
}

/// Puts the frame into `host`. Built by hand so that `sandbox` is on the element before it has a
/// document: attributes of a view are set in no order the policy may rest on.
fn mount_frame(host: &web_sys::HtmlElement, document: &str) {
  let Some(frame) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|page| page.create_element("iframe").ok())
  else {
    return;
  };
  // The empty value is the strictest sandbox: no scripts, no same origin (an opaque origin), no
  // forms, popups, top navigation, modals, downloads or pointer lock.
  let attributes = [
    ("sandbox", ""),
    // Chromium also enforces this policy on the frame's document, on top of the one in it.
    ("csp", preview::HTML_FRAME_CSP),
    ("referrerpolicy", "no-referrer"),
    ("allow", ""),
    ("class", "preview-frame"),
    ("title", "preview"),
    ("srcdoc", document),
  ];
  for (name, value) in attributes {
    if frame.set_attribute(name, value).is_err() {
      tracing::warn!(attribute = name, "the preview frame could not be set up");
      return;
    }
  }
  drop(host.append_child(&frame));
}

/// The links of an HTML file, read from an inert document (the parser runs no script and loads
/// nothing).
fn links_of(html: &str) -> Vec<String> {
  let Ok(parser) = web_sys::DomParser::new() else {
    return Vec::new();
  };
  let Ok(parsed) = parser.parse_from_string(html, web_sys::SupportedType::TextHtml) else {
    return Vec::new();
  };
  let Ok(anchors) = parsed.query_selector_all("a[href]") else {
    return Vec::new();
  };
  let hrefs: Vec<String> = (0..anchors.length())
    .filter_map(|index| anchors.item(index))
    .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
    .filter_map(|anchor| anchor.get_attribute("href"))
    .collect();
  preview::listed_links(hrefs.iter().map(String::as_str))
}
