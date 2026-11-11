//! "Export my data" in the browser (issue #211, docs/data-export.md): the core in the page writes
//! the archive into pieces of a `Blob`, and the finished archive is offered as a download.
//!
//! The page never holds the archive in its own memory: every piece of up to [`PIECE_BYTES`] goes
//! into a `Blob` at once, which the browser keeps (and may move to disk) until the download is
//! made; the buffer is wiped before it is reused. A cancelled or failed export drops the pieces.
//! A reload ends an export: the pieces are gone with the page, so there is nothing to continue,
//! and the checkpoint the core left is deleted on the next look.

use teamcraft_client_api::data_export::DataExportStatus;
use teamcraft_core::export::{ExportError, ExportJob, ExportSink, ExportStep};
use wasm_bindgen::JsCast;

use crate::web_core::WebClient;

/// What the page buffers before it hands a piece to the browser.
const PIECE_BYTES: usize = 1024 * 1024;
/// How long the download's object URL stays valid; the browser has the file long before.
const OBJECT_URL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// Where the page's export stands.
#[derive(Default)]
pub(crate) enum WebExportSlot {
  #[default]
  Idle,
  Running(Box<WebExportRun>),
  /// What the last export ended with, until the user dismisses it.
  Ended(DataExportStatus),
}

/// An export under way: the job and the pieces it wrote.
pub(crate) struct WebExportRun {
  pub(crate) job: ExportJob,
  pub(crate) sink: BlobSink,
  pub(crate) file_name: String,
}

/// One step of an export with what it reads from and writes to. The steps run in the security
/// settings' module (`areas::export_step`, #1699), next to the export's card: the archive's writers
/// (about 75 kB before wasm-opt) were in every first visit.
pub(crate) struct ExportTurn<'a> {
  pub(crate) job: &'a mut ExportJob,
  pub(crate) sink: &'a mut BlobSink,
  pub(crate) client: &'a mut WebClient,
}

/// The archive as pieces the browser holds.
pub(crate) struct BlobSink {
  pieces: js_sys::Array,
  buffer: Vec<u8>,
}

impl BlobSink {
  pub(crate) fn new() -> Self {
    Self {
      pieces: js_sys::Array::new(),
      buffer: Vec::with_capacity(PIECE_BYTES),
    }
  }

  /// Offers the complete archive as a download named `file_name`.
  pub(crate) fn finish(mut self, file_name: &str) -> Result<(), ExportError> {
    self.seal()?;
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("application/zip");
    let blob = web_sys::Blob::new_with_blob_sequence_and_options(&self.pieces, &options).map_err(js_error)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js_error)?;
    let document = web_sys::window()
      .and_then(|window| window.document())
      .ok_or(ExportError::Sink("no document".into()))?;
    let anchor: web_sys::HtmlAnchorElement = document
      .create_element("a")
      .map_err(js_error)?
      .dyn_into()
      .map_err(|_| ExportError::Sink("not an anchor".into()))?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    anchor.set_rel("noopener");
    anchor.click();
    leptos::prelude::set_timeout(
      move || drop(web_sys::Url::revoke_object_url(&url)),
      OBJECT_URL_LIFETIME,
    );
    Ok(())
  }

  /// Hands what is buffered to the browser as one piece and wipes the buffer.
  fn seal(&mut self) -> Result<(), ExportError> {
    if self.buffer.is_empty() {
      return Ok(());
    }
    let piece = piece_of(&self.buffer)?;
    self.pieces.push(&piece);
    self.buffer.fill(0);
    self.buffer.clear();
    Ok(())
  }
}

impl ExportTurn<'_> {
  pub(crate) async fn step(self) -> Result<ExportStep, ExportError> {
    Box::pin(self.job.step(self.client, self.sink)).await
  }
}

impl Drop for BlobSink {
  fn drop(&mut self) {
    self.buffer.fill(0);
  }
}

impl ExportSink for BlobSink {
  fn write_all(&mut self, bytes: &[u8]) -> Result<(), ExportError> {
    if self.buffer.len() + bytes.len() <= PIECE_BYTES {
      self.buffer.extend_from_slice(bytes);
      return Ok(());
    }
    // A large write (a file) goes to the browser in pieces, never through the buffer.
    self.seal()?;
    for chunk in bytes.chunks(PIECE_BYTES) {
      let piece = piece_of(chunk)?;
      self.pieces.push(&piece);
    }
    Ok(())
  }

  fn flush(&mut self) -> Result<(), ExportError> {
    self.seal()
  }
}

fn piece_of(bytes: &[u8]) -> Result<web_sys::Blob, ExportError> {
  let array = js_sys::Uint8Array::from(bytes);
  web_sys::Blob::new_with_u8_array_sequence(&js_sys::Array::of1(&array)).map_err(js_error)
}

fn js_error(error: wasm_bindgen::JsValue) -> ExportError {
  ExportError::Sink(format!("{error:?}"))
}
