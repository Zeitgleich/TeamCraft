//! The PDF pane of file previews (#557): the vendored PDF.js (`pdf-viewer.mjs`), loaded when the
//! first PDF opens, with its zoom, page and search tools.
//!
//! Whatever the renderer does, the preview stays closable (#835): every call into the viewer
//! catches its exceptions (one thrown back through the app's wasm leaves the app broken: closing
//! the preview called a method PDF.js no longer has, and the preview could not be closed any
//! more), and a document that can't be opened or drawn, or takes too long to open, gives way to a
//! message with the download.

use std::time::Duration;

use js_sys::{Function, Reflect, Uint8Array};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::i18n::I18n;
use crate::reactive::SetIfAlive;

/// The PDF viewer module; the version in the path is the cache key of the vendored PDF.js
/// (`vendor/pdfjs-*`, `index.html`).
const PDF_VIEWER_MODULE: &str = "/pdfjs-6.4.299/pdf-viewer.mjs";
/// One zoom step of the PDF viewer.
const PDF_ZOOM_STEP: f64 = 1.25;
/// A document still not open after this (module, worker and first page) is given up: the pane
/// offers the download instead of a spinner that never ends.
const PDF_OPEN_TIMEOUT: Duration = Duration::from_secs(45);

/// A PDF file in the vendored viewer; `download` saves the file instead.
pub fn pdf_pane(i18n: I18n, bytes: Uint8Array, download: Callback<()>) -> AnyView {
  let session = PdfSession::<PdfViewer>::new();
  let container = NodeRef::<html::Div>::new();
  let page = RwSignal::new(1u32);
  let find_result = RwSignal::new(FindResult::Idle);
  let query_input = NodeRef::<html::Input>::new();
  let callbacks: StoredValue<Option<ViewerCallbacks>, LocalStorage> = StoredValue::new_local(None);
  let bytes = StoredValue::new_local(Some(bytes));

  Effect::new(move |_| {
    let (Some(container), Some(bytes)) = (container.get(), bytes.try_update_value(Option::take).flatten())
    else {
      return;
    };
    let on_page = Closure::<dyn Fn(u32)>::new(move |number| page.set_if_alive(number));
    let on_failure = Closure::<dyn Fn()>::new(move || session.fail(PdfFailure::NotDrawn));
    let on_page_fn: Function = on_page.as_ref().unchecked_ref::<Function>().clone();
    let on_failure_fn: Function = on_failure.as_ref().unchecked_ref::<Function>().clone();
    callbacks.set_value(Some(ViewerCallbacks {
      _on_page: on_page,
      _on_failure: on_failure,
    }));
    spawn_local(async move {
      match open_viewer(&container, &bytes, &on_page_fn, &on_failure_fn).await {
        Ok(opened) => session.opened(opened),
        Err(error) => {
          tracing::warn!(error = ?error, "a PDF could not be opened");
          session.fail(PdfFailure::Unreadable);
        },
      }
    });
    set_timeout(move || session.fail(PdfFailure::TimedOut), PDF_OPEN_TIMEOUT);
  });

  let jump = move |ev: web_sys::Event| {
    session.go_to(event_target_value(&ev).parse::<u32>().unwrap_or(1));
  };
  let search = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let query = query_input
      .get_untracked()
      .map(|field| field.value())
      .unwrap_or_default();
    let Some(promise) = session.with_document(|opened| opened.find(&query).ok()).flatten() else {
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
    // Nothing to zoom, page through or search in a document that failed.
    <div class="pdf-tools" prop:hidden=move || matches!(session.phase(), PdfPhase::Failed(_))>
      <button class="icon-btn" aria-label=move || i18n.t("preview-zoom-out") data-tip=move || i18n.t("preview-zoom-out") on:click=move |_| session.zoom(1.0 / PDF_ZOOM_STEP)>
        <Icon name=IconName::ZoomOut />
      </button>
      <button class="btn btn-small" on:click=move |_| session.fit()>{move || format!("{} %", (session.scale() * 100.0).round())}</button>
      <button class="icon-btn" aria-label=move || i18n.t("preview-zoom-in") data-tip=move || i18n.t("preview-zoom-in") on:click=move |_| session.zoom(PDF_ZOOM_STEP)>
        <Icon name=IconName::ZoomIn />
      </button>
      <label class="pdf-page-field">
        <span class="visually-hidden">{move || i18n.t("preview-page")}</span>
        <input
          class="input pdf-page-input"
          type="number"
          min="1"
          prop:max=move || session.page_count()
          prop:value=move || page.get()
          on:change=jump
        />
        <span aria-hidden="true">{move || format!("/ {}", session.page_count())}</span>
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
    <div class="pdf-stage">
      // The viewer owns the children of `.pdf-pages`; what the app shows over it is a sibling.
      <div class="pdf-pages" node_ref=container tabindex="0" prop:hidden=move || matches!(session.phase(), PdfPhase::Failed(_))></div>
      {move || match session.phase() {
        PdfPhase::Opening => Some(view! {
          <div class="preview-status pdf-opening" role="status" aria-busy="true"><span class="spinner" aria-hidden="true"></span></div>
        }.into_any()),
        PdfPhase::Shown => None,
        PdfPhase::Failed(failure) => Some(view! {
          <div class="preview-status" role="alert">
            <p>{move || i18n.t(failure.message_key())}</p>
            <button class="btn btn-primary" on:click=move |_| download.run(())>{move || i18n.t("file-download")}</button>
          </div>
        }.into_any()),
      }}
    </div>
  }
  .into_any()
}

/// The pane's side of one document: what it shows, and the open viewer, which is released when
/// the pane goes (closing the preview), when it fails, or when it arrives after either. Generic over
/// the viewer so that the close paths can be tested without a browser.
pub(crate) struct PdfSession<D: 'static> {
  phase: RwSignal<PdfPhase>,
  page_count: RwSignal<u32>,
  scale: RwSignal<f64>,
  document: StoredValue<Option<D>, LocalStorage>,
}

/// Where the PDF pane stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PdfPhase {
  /// Loading the viewer module and the document: a spinner.
  Opening,
  /// Pages are on screen (drawn as they scroll into view).
  Shown,
  /// A message and the download instead of the pages.
  Failed(PdfFailure),
}

/// Why the PDF pane offers the download instead of the pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PdfFailure {
  /// Not a PDF PDF.js can read.
  Unreadable,
  /// Opened, but its pages can't be drawn here.
  NotDrawn,
  /// Still not open after [`PDF_OPEN_TIMEOUT`].
  TimedOut,
}

/// The calls the pane makes on an open document: PDF.js in the page, a fake in tests. None of them
/// may throw through the app; failures come back as errors.
pub(crate) trait PdfDocument {
  fn page_count(&self) -> Result<u32, PdfViewerError>;
  fn fit_width(&self) -> Result<f64, PdfViewerError>;
  fn set_scale(&self, scale: f64) -> Result<f64, PdfViewerError>;
  fn go_to(&self, page: u32) -> Result<(), PdfViewerError>;
  /// Frees the pages, the document and its worker.
  fn release(&self) -> Result<(), PdfViewerError>;
}

/// A call into the PDF viewer threw.
#[derive(Debug, thiserror::Error)]
#[error("the PDF viewer failed: {0}")]
pub(crate) struct PdfViewerError(String);

/// Where a search in the PDF stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FindResult {
  Idle,
  Searching,
  Found,
  None,
}

/// The app's callbacks the viewer calls, kept alive as long as the pane.
struct ViewerCallbacks {
  _on_page: Closure<dyn Fn(u32)>,
  _on_failure: Closure<dyn Fn()>,
}

impl<D> Clone for PdfSession<D> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<D> Copy for PdfSession<D> {}

impl<D: PdfDocument + Clone + 'static> PdfSession<D> {
  /// A session of the current owner (the pane), opening.
  pub(crate) fn new() -> Self {
    let session = Self {
      phase: RwSignal::new(PdfPhase::Opening),
      page_count: RwSignal::new(0),
      scale: RwSignal::new(1.0),
      document: StoredValue::new_local(None),
    };
    on_cleanup(move || session.release());
    session
  }

  pub(crate) fn phase(&self) -> PdfPhase {
    self.phase.try_get().unwrap_or(PdfPhase::Opening)
  }

  pub(crate) fn page_count(&self) -> u32 {
    self.page_count.try_get().unwrap_or_default()
  }

  /// The zoom factor (1 = 100 %).
  pub(crate) fn scale(&self) -> f64 {
    self.scale.try_get().unwrap_or(1.0)
  }

  /// The viewer opened `document`. Shown at the width of the pane, unless the pane gave up on it
  /// meanwhile or is gone: then it is released at once.
  pub(crate) fn opened(&self, document: D) {
    if self.phase.try_get_untracked() != Some(PdfPhase::Opening) {
      release(&document);
      return;
    }
    if let Some(orphan) = self.document.try_set_value(Some(document)).flatten() {
      release(&orphan);
      return;
    }
    let measured =
      self.with_document(|opened| Ok::<_, PdfViewerError>((opened.page_count()?, opened.fit_width()?)));
    match measured {
      Some(Ok((count, scale))) => {
        self.page_count.set_if_alive(count);
        self.scale.set_if_alive(scale);
        self.phase.set_if_alive(PdfPhase::Shown);
      },
      Some(Err(error)) => {
        tracing::warn!(%error, "an opened PDF could not be laid out");
        self.fail(PdfFailure::Unreadable);
      },
      None => {},
    }
  }

  /// Shows `failure` with the download instead of the pages and releases the document. The first
  /// failure stays; one that comes too late to matter (the open timeout of a shown document)
  /// changes nothing.
  pub(crate) fn fail(&self, failure: PdfFailure) {
    let Some(phase) = self.phase.try_get_untracked() else {
      return;
    };
    let next = phase.failed_with(failure);
    if next != phase {
      self.phase.set_if_alive(next);
      self.release();
    }
  }

  /// Zooms by `factor`, within the viewer's limits.
  pub(crate) fn zoom(&self, factor: f64) {
    let Some(current) = self.scale.try_get_untracked() else {
      return;
    };
    self.apply_scale(|opened| opened.set_scale(current * factor));
  }

  /// Zooms to the width of the pane.
  pub(crate) fn fit(&self) {
    self.apply_scale(PdfDocument::fit_width);
  }

  pub(crate) fn go_to(&self, page: u32) {
    if let Some(Err(error)) = self.with_document(|opened| opened.go_to(page)) {
      tracing::warn!(%error, "the PDF viewer could not go to a page");
    }
  }

  /// `read` on the open document; `None` while there is none. The document is called outside the
  /// stored value's lock: the viewer calls back into the pane (`fail`), which takes it again.
  pub(crate) fn with_document<T>(&self, read: impl FnOnce(&D) -> T) -> Option<T> {
    let maybe_document = self.document.try_with_value(Clone::clone).flatten();
    maybe_document.as_ref().map(read)
  }

  /// A viewer that can't zoom can't draw either.
  fn apply_scale(&self, change: impl FnOnce(&D) -> Result<f64, PdfViewerError>) {
    match self.with_document(change) {
      Some(Ok(scale)) => self.scale.set_if_alive(scale),
      Some(Err(error)) => {
        tracing::warn!(%error, "the PDF viewer could not zoom");
        self.fail(PdfFailure::NotDrawn);
      },
      None => {},
    }
  }

  fn release(&self) {
    if let Some(document) = self.document.try_update_value(Option::take).flatten() {
      release(&document);
    }
  }
}

impl PdfPhase {
  /// The phase after `failure`: see [`PdfSession::fail`].
  fn failed_with(self, failure: PdfFailure) -> Self {
    match (self, failure) {
      (Self::Shown, PdfFailure::TimedOut) | (Self::Failed(_), _) => self,
      (Self::Opening | Self::Shown, _) => Self::Failed(failure),
    }
  }
}

impl PdfFailure {
  fn message_key(self) -> &'static str {
    match self {
      Self::Unreadable => "preview-pdf-unreadable",
      Self::NotDrawn => "preview-pdf-not-drawn",
      Self::TimedOut => "preview-pdf-timed-out",
    }
  }
}

/// Releases `document`; a failure is only logged, the pane is going anyway.
fn release<D: PdfDocument>(document: &D) {
  if let Err(error) = document.release() {
    tracing::warn!(%error, "the PDF viewer could not be released");
  }
}

#[wasm_bindgen(inline_js = "export function import_module(url) { return import(url); }")]
extern "C" {
  /// `import(url)`: loads an ES module of the page's own origin.
  fn import_module(url: &str) -> js_sys::Promise;
}

#[wasm_bindgen]
extern "C" {
  /// The viewer `pdf-viewer.mjs` returns. Every method catches: an exception must not unwind
  /// through the app (#835).
  #[derive(Clone)]
  type PdfViewer;

  #[wasm_bindgen(method, getter, catch, js_name = pageCount)]
  fn page_count(this: &PdfViewer) -> Result<u32, JsValue>;

  #[wasm_bindgen(method, catch, js_name = setScale)]
  fn set_scale(this: &PdfViewer, scale: f64) -> Result<f64, JsValue>;

  #[wasm_bindgen(method, catch, js_name = fitWidth)]
  fn fit_width(this: &PdfViewer) -> Result<f64, JsValue>;

  #[wasm_bindgen(method, catch, js_name = goTo)]
  fn go_to(this: &PdfViewer, page: u32) -> Result<(), JsValue>;

  #[wasm_bindgen(method, catch)]
  fn find(this: &PdfViewer, query: &str) -> Result<js_sys::Promise, JsValue>;

  #[wasm_bindgen(method, catch)]
  fn destroy(this: &PdfViewer) -> Result<(), JsValue>;
}

impl PdfDocument for PdfViewer {
  fn page_count(&self) -> Result<u32, PdfViewerError> {
    PdfViewer::page_count(self).map_err(PdfViewerError::from)
  }

  fn fit_width(&self) -> Result<f64, PdfViewerError> {
    PdfViewer::fit_width(self).map_err(PdfViewerError::from)
  }

  fn set_scale(&self, scale: f64) -> Result<f64, PdfViewerError> {
    PdfViewer::set_scale(self, scale).map_err(PdfViewerError::from)
  }

  fn go_to(&self, page: u32) -> Result<(), PdfViewerError> {
    PdfViewer::go_to(self, page).map_err(PdfViewerError::from)
  }

  fn release(&self) -> Result<(), PdfViewerError> {
    self.destroy().map_err(PdfViewerError::from)
  }
}

impl From<JsValue> for PdfViewerError {
  fn from(error: JsValue) -> Self {
    Self(format!("{error:?}"))
  }
}

/// Loads the viewer module (the first time) and opens `bytes` in `container`.
async fn open_viewer(
  container: &web_sys::HtmlElement,
  bytes: &Uint8Array,
  on_page: &Function,
  on_failure: &Function,
) -> Result<PdfViewer, JsValue> {
  let module = JsFuture::from(import_module(PDF_VIEWER_MODULE)).await?;
  let open: Function = Reflect::get(&module, &JsValue::from_str("openPdf"))?.dyn_into()?;
  let arguments = js_sys::Array::of4(container, bytes, on_page, on_failure);
  let opened = open.apply(&JsValue::NULL, &arguments)?;
  Ok(
    JsFuture::from(js_sys::Promise::from(opened))
      .await?
      .unchecked_into(),
  )
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::rc::Rc;

  use super::*;
  use crate::backend::Backend;
  use crate::lightbox::MediaKey;
  use crate::mock::MockBackend;
  use crate::model::{ConversationId, TimestampMs};
  use crate::settings::Settings;
  use crate::state::{AppState, Overlay};

  const NOW: TimestampMs = 1_792_584_000_000;

  mod phases {
    use super::*;

    #[test]
    fn opening_fails_with_whatever_happens_first() {
      for failure in [PdfFailure::Unreadable, PdfFailure::NotDrawn, PdfFailure::TimedOut] {
        assert_eq!(PdfPhase::Opening.failed_with(failure), PdfPhase::Failed(failure));
      }
    }

    #[test]
    fn a_shown_document_fails_when_pages_cant_be_drawn_but_not_on_the_late_timeout() {
      assert_eq!(
        PdfPhase::Shown.failed_with(PdfFailure::NotDrawn),
        PdfPhase::Failed(PdfFailure::NotDrawn)
      );
      assert_eq!(PdfPhase::Shown.failed_with(PdfFailure::TimedOut), PdfPhase::Shown);
    }

    #[test]
    fn the_first_failure_stays() {
      let failed = PdfPhase::Failed(PdfFailure::Unreadable);
      assert_eq!(failed.failed_with(PdfFailure::NotDrawn), failed);
      assert_eq!(failed.failed_with(PdfFailure::TimedOut), failed);
    }
  }

  /// The ×, Escape and the backdrop all close the preview with `close_overlay`, which drops the
  /// pane (its owner). Each state must survive that, and a viewer that fails must not get in
  /// the way.
  mod closing {
    use super::*;

    #[test]
    fn while_opening_closes_and_releases_the_viewer_that_arrives_later() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      close(state, &pane);
      let late = FakeViewer::new();
      session.opened(late.clone());
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
      assert_eq!(late.releases(), 1);
    }

    #[test]
    fn while_shown_releases_the_viewer() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      let viewer = FakeViewer::new();
      session.opened(viewer.clone());
      assert_eq!(session.phase(), PdfPhase::Shown);
      close(state, &pane);
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
      assert_eq!(viewer.releases(), 1);
    }

    #[test]
    fn when_releasing_the_viewer_fails() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      let viewer = FakeViewer::failing();
      session.opened(viewer.clone());
      close(state, &pane);
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
      assert_eq!(viewer.releases(), 1);
    }

    #[test]
    fn after_pages_could_not_be_drawn() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      let viewer = FakeViewer::new();
      session.opened(viewer.clone());
      session.fail(PdfFailure::NotDrawn);
      assert_eq!(session.phase(), PdfPhase::Failed(PdfFailure::NotDrawn));
      assert_eq!(viewer.releases(), 1, "a failed document is freed at once");
      close(state, &pane);
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
      assert_eq!(viewer.releases(), 1);
    }

    #[test]
    fn after_the_document_could_not_be_laid_out() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      let viewer = FakeViewer::failing();
      session.opened(viewer.clone());
      assert_eq!(session.phase(), PdfPhase::Failed(PdfFailure::Unreadable));
      close(state, &pane);
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
      assert_eq!(viewer.releases(), 1);
    }

    #[test]
    fn after_opening_timed_out_and_releases_the_viewer_that_arrives_later() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      session.fail(PdfFailure::TimedOut);
      let late = FakeViewer::new();
      session.opened(late.clone());
      assert_eq!(session.phase(), PdfPhase::Failed(PdfFailure::TimedOut));
      assert_eq!(late.releases(), 1);
      close(state, &pane);
      assert_eq!(state.overlay.get_untracked(), Overlay::None);
    }

    #[test]
    fn a_failure_reported_after_closing_does_nothing() {
      let Preview {
        _app,
        state,
        pane,
        session,
      } = open_preview();
      let viewer = FakeViewer::new();
      session.opened(viewer.clone());
      close(state, &pane);
      session.fail(PdfFailure::NotDrawn);
      session.zoom(PDF_ZOOM_STEP);
      assert_eq!(viewer.releases(), 1);
    }
  }

  /// The app with a PDF preview open.
  struct Preview {
    /// The app's owner: dropping it would dispose everything.
    _app: Owner,
    state: AppState,
    /// The pane's owner, which the overlay drops when it closes.
    pane: Owner,
    session: PdfSession<FakeViewer>,
  }

  fn open_preview() -> Preview {
    let app = Owner::new();
    app.set();
    let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(NOW));
    let state = AppState::new(backend, Settings::for_tests(), None);
    let conversation: ConversationId = "1".parse().expect("valid id");
    let key = MediaKey {
      message: "2".parse().expect("valid id"),
      attachment: "3".parse().expect("valid id"),
    };
    state.overlay.set(Overlay::FilePreview(conversation, key));
    let pane = app.child();
    let session = pane.with(PdfSession::<FakeViewer>::new);
    Preview {
      _app: app,
      state,
      pane,
      session,
    }
  }

  /// What every close path does: the overlay goes, and with it the pane.
  fn close(state: AppState, pane: &Owner) {
    state.close_overlay();
    pane.cleanup();
  }

  /// A viewer that counts its releases; a failing one throws from every call.
  #[derive(Clone)]
  struct FakeViewer {
    is_failing: bool,
    released: Rc<Cell<u32>>,
  }

  impl FakeViewer {
    fn new() -> Self {
      Self {
        is_failing: false,
        released: Rc::default(),
      }
    }

    fn failing() -> Self {
      Self {
        is_failing: true,
        ..Self::new()
      }
    }

    fn releases(&self) -> u32 {
      self.released.get()
    }

    fn answer<T>(&self, value: T) -> Result<T, PdfViewerError> {
      if self.is_failing {
        Err(PdfViewerError("thrown".into()))
      } else {
        Ok(value)
      }
    }
  }

  impl PdfDocument for FakeViewer {
    fn page_count(&self) -> Result<u32, PdfViewerError> {
      self.answer(14)
    }

    fn fit_width(&self) -> Result<f64, PdfViewerError> {
      self.answer(1.53)
    }

    fn set_scale(&self, scale: f64) -> Result<f64, PdfViewerError> {
      self.answer(scale)
    }

    fn go_to(&self, _page: u32) -> Result<(), PdfViewerError> {
      self.answer(())
    }

    fn release(&self) -> Result<(), PdfViewerError> {
      self.released.set(self.released.get() + 1);
      self.answer(())
    }
  }
}
