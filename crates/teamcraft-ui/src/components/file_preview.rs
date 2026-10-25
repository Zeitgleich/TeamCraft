//! The in-app preview of a Markdown, HTML or PDF attachment (#557). It works from the file the
//! client core has already downloaded and decrypted (`object_url::fetch_decrypted`); nothing leaves
//! the client. The file is untrusted, so each kind is shown in a way that can't run its code or
//! reach the app (see `preview.rs`):
//!
//! - Markdown: the app's own renderer, a tree of elements.
//! - HTML: a sandboxed frame with no permissions and its own policy; its links are listed next to
//!   it and open in the system browser.
//! - PDF: the vendored PDF.js (`pdf-viewer.mjs`), loaded when the first PDF opens.

use std::rc::Rc;

use js_sys::{Function, Reflect, Uint8Array};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::remote::AttachmentPriority;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::rich_text::RichText;
use crate::i18n::{I18n, use_i18n};
use crate::lightbox::MediaKey;
use crate::model::ConversationId;
use crate::object_url::fetch_decrypted;
use crate::preview::{self, PreviewKind};
use crate::reactive::SetIfAlive;
use crate::state::{AppState, use_app_state};

/// The PDF viewer module; the version in the path is the cache key of the vendored PDF.js
/// (`vendor/pdfjs-*`, `index.html`).
const PDF_VIEWER_MODULE: &str = "/pdfjs-6.4.299/pdf-viewer.mjs";
/// One zoom step of the PDF viewer.
const PDF_ZOOM_STEP: f64 = 1.25;

#[wasm_bindgen(inline_js = "export function import_module(url) { return import(url); }")]
extern "C" {
  /// `import(url)`: loads an ES module of the page's own origin.
  fn import_module(url: &str) -> js_sys::Promise;
}

#[wasm_bindgen]
extern "C" {
  /// The viewer `pdf-viewer.mjs` returns.
  type PdfViewer;

  #[wasm_bindgen(method, getter, js_name = pageCount)]
  fn page_count(this: &PdfViewer) -> u32;

  #[wasm_bindgen(method, js_name = setScale)]
  fn set_scale(this: &PdfViewer, scale: f64) -> f64;

  #[wasm_bindgen(method, js_name = fitWidth)]
  fn fit_width(this: &PdfViewer) -> f64;

  #[wasm_bindgen(method, js_name = goTo)]
  fn go_to(this: &PdfViewer, page: u32);

  #[wasm_bindgen(method)]
  fn find(this: &PdfViewer, query: &str) -> js_sys::Promise;

  #[wasm_bindgen(method)]
  fn destroy(this: &PdfViewer);
}

/// Told by the viewer which page is in view; kept alive as long as the viewer.
type PageCallback = Closure<dyn Fn(u32)>;

/// What the preview is showing.
#[derive(Clone)]
enum Load {
  Loading,
  /// Not downloaded or decrypted (offline, server trouble).
  Unavailable,
  Ready(Content),
}

#[derive(Clone)]
enum Content {
  Text(Rc<String>),
  Pdf(Uint8Array),
}

/// The preview of one file of a conversation.
#[component]
pub fn FilePreview(conversation: ConversationId, key: MediaKey) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_file = file_of(state, conversation, key);
  let Some(file) = maybe_file.filter(|file| {
    PreviewKind::of(&file.media_type, &file.file_name).is_some() && preview::is_within_limit(file.size_bytes)
  }) else {
    // Gone (deleted) or not previewable: nothing to show.
    state.close_overlay();
    return ().into_any();
  };
  let kind = PreviewKind::of(&file.media_type, &file.file_name).unwrap_or(PreviewKind::Markdown);
  let phase: RwSignal<Load, LocalStorage> = RwSignal::new_local(Load::Loading);
  let is_source = RwSignal::new(false);
  let file_id = StoredValue::new(file.file_id.clone());
  let message = key.message;

  let start = move || {
    phase.set(Load::Loading);
    let file_id = file_id.get_value();
    spawn_local(async move {
      let loaded = fetch_decrypted(conversation, message, file_id, AttachmentPriority::Now).await;
      phase.set_if_alive(match loaded {
        Ok(bytes) if kind.is_text() => Load::Ready(Content::Text(Rc::new(preview::text_of(&bytes.to_vec())))),
        Ok(bytes) => Load::Ready(Content::Pdf(bytes)),
        Err(error) => {
          tracing::warn!(%error, "a file could not be loaded for its preview");
          Load::Unavailable
        },
      });
    });
  };
  start();

  let download = move |_| {
    file_id.with_value(|file_id| {
      state.write(|backend| backend.download_attachment(conversation, message, file_id));
    });
  };
  let name = file.file_name.clone();
  let size = format_bytes(file.size_bytes);
  let toggles_source = kind.is_text();
  let body = move || {
    match phase.get() {
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
    Load::Ready(Content::Pdf(bytes)) => pdf_pane(i18n, bytes),
    Load::Ready(Content::Text(text)) => match (kind, is_source.get()) {
      (_, true) => view! { <pre class="preview-source" translate="no">{text.as_str().to_owned()}</pre> }.into_any(),
      (PreviewKind::Html, false) => html_pane(i18n, &text),
      _ => view! { <div class="preview-markdown"><RichText body=text.as_str().to_owned() /></div> }.into_any(),
    },
  }
  };

  view! {
    <Modal labelled_by="file-preview-title" class="file-preview">
      <header class="modal-head preview-head">
        <h2 id="file-preview-title" class="preview-name" translate="no" title=name.clone()>{name.clone()}</h2>
        <span class="preview-size">{size}</span>
        <div class="preview-tools">
          {toggles_source.then(|| view! {
            <button class="btn btn-small" aria-pressed=move || is_source.get().to_string() on:click=move |_| is_source.update(|on| *on = !*on)>
              {move || if is_source.get() { i18n.t("preview-show-formatted") } else { i18n.t("preview-show-source") }}
            </button>
          })}
          <button class="icon-btn" aria-label=move || i18n.t("file-download") data-tip=move || i18n.t("file-download") on:click=download>
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

/// A PDF file in the vendored viewer.
fn pdf_pane(i18n: I18n, bytes: Uint8Array) -> AnyView {
  let container = NodeRef::<html::Div>::new();
  let viewer: StoredValue<Option<PdfViewer>, LocalStorage> = StoredValue::new_local(None);
  let is_broken = RwSignal::new(false);
  let page = RwSignal::new(1u32);
  let page_count = RwSignal::new(0u32);
  let scale = RwSignal::new(1.0f64);
  let find_result = RwSignal::new(FindResult::Idle);
  let query_input = NodeRef::<html::Input>::new();
  let on_page: StoredValue<Option<PageCallback>, LocalStorage> = StoredValue::new_local(None);
  let bytes = StoredValue::new_local(Some(bytes));

  Effect::new(move |_| {
    let (Some(container), Some(bytes)) = (container.get(), bytes.try_update_value(Option::take).flatten())
    else {
      return;
    };
    let callback = Closure::<dyn Fn(u32)>::new(move |number| page.set_if_alive(number));
    let on_page_fn: Function = callback.as_ref().unchecked_ref::<Function>().clone();
    on_page.set_value(Some(callback));
    spawn_local(async move {
      match open_viewer(&container, &bytes, &on_page_fn).await {
        Ok(opened) => {
          page_count.set_if_alive(opened.page_count());
          scale.set_if_alive(opened.fit_width());
          // Hands the viewer back when the pane is gone already: nobody is left to use it.
          if let Some(orphan) = viewer.try_set_value(Some(opened)).flatten() {
            orphan.destroy();
          }
        },
        Err(error) => {
          tracing::warn!(error = ?error, "a PDF could not be opened");
          is_broken.set_if_alive(true);
        },
      }
    });
  });
  on_cleanup(move || {
    // Frees the document, its worker and the page bitmaps.
    drop(viewer.try_update_value(|maybe_viewer| maybe_viewer.take().inspect(PdfViewer::destroy)));
  });

  let zoom = move |factor: f64| {
    viewer.with_value(|maybe_viewer| {
      if let Some(opened) = maybe_viewer {
        scale.set(opened.set_scale(scale.get_untracked() * factor));
      }
    });
  };
  let fit = move |_| {
    viewer.with_value(|maybe_viewer| {
      if let Some(opened) = maybe_viewer {
        scale.set(opened.fit_width());
      }
    });
  };
  let jump = move |ev: web_sys::Event| {
    let number = event_target_value(&ev).parse::<u32>().unwrap_or(1);
    viewer.with_value(|maybe_viewer| {
      if let Some(opened) = maybe_viewer {
        opened.go_to(number);
      }
    });
  };
  let search = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let query = query_input
      .get_untracked()
      .map(|field| field.value())
      .unwrap_or_default();
    let maybe_promise =
      viewer.with_value(|maybe_viewer| maybe_viewer.as_ref().map(|opened| opened.find(&query)));
    let Some(promise) = maybe_promise else {
      return;
    };
    find_result.set(FindResult::Searching);
    spawn_local(async move {
      let found = JsFuture::from(promise)
        .await
        .ok()
        .and_then(|number| number.as_f64())
        .unwrap_or(0.0);
      find_result.set_if_alive(if found >= 1.0 {
        FindResult::Found
      } else {
        FindResult::None
      });
    });
  };

  view! {
    <div class="pdf-tools">
      <button class="icon-btn" aria-label=move || i18n.t("preview-zoom-out") data-tip=move || i18n.t("preview-zoom-out") on:click=move |_| zoom(1.0 / PDF_ZOOM_STEP)>
        <Icon name=IconName::ZoomOut />
      </button>
      <button class="btn btn-small" on:click=fit>{move || format!("{} %", (scale.get() * 100.0).round())}</button>
      <button class="icon-btn" aria-label=move || i18n.t("preview-zoom-in") data-tip=move || i18n.t("preview-zoom-in") on:click=move |_| zoom(PDF_ZOOM_STEP)>
        <Icon name=IconName::ZoomIn />
      </button>
      <label class="pdf-page-field">
        <span class="visually-hidden">{move || i18n.t("preview-page")}</span>
        <input
          class="input pdf-page-input"
          type="number"
          min="1"
          prop:max=move || page_count.get()
          prop:value=move || page.get()
          on:change=jump
        />
        <span aria-hidden="true">{move || format!("/ {}", page_count.get())}</span>
      </label>
      <form class="pdf-find" on:submit=search>
        <input class="input" type="search" node_ref=query_input maxlength="200" placeholder=move || i18n.t("preview-find") aria-label=move || i18n.t("preview-find") />
        <span class="pdf-find-state" role="status">
          {move || match find_result.get() {
            FindResult::None => i18n.t("preview-find-none"),
            _ => String::new(),
          }}
        </span>
      </form>
    </div>
    {move || is_broken.get().then(|| view! {
      <div class="preview-status" role="alert"><p>{move || i18n.t("preview-pdf-unreadable")}</p></div>
    })}
    <div class="pdf-pages" node_ref=container tabindex="0" prop:hidden=move || is_broken.get()></div>
  }
  .into_any()
}

/// Where a search in the PDF stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FindResult {
  Idle,
  Searching,
  Found,
  None,
}

/// Loads the viewer module (the first time) and opens `bytes` in `container`.
async fn open_viewer(
  container: &web_sys::HtmlElement,
  bytes: &Uint8Array,
  on_page: &Function,
) -> Result<PdfViewer, JsValue> {
  let module = JsFuture::from(import_module(PDF_VIEWER_MODULE)).await?;
  let open: Function = Reflect::get(&module, &JsValue::from_str("openPdf"))?.dyn_into()?;
  let opened = open.call3(&JsValue::NULL, container, bytes, on_page)?;
  Ok(
    JsFuture::from(js_sys::Promise::from(opened))
      .await?
      .unchecked_into(),
  )
}
