//! "Export" of the audit log as a CSV file (issue #2137, docs/data-export.md): the admin console
//! reads the log in pages, as the filters of the view say, and writes each page into the file as
//! it arrives. The file is pieces of a `Blob` the browser keeps (and may move to disk), so a long
//! log is never held in the page's memory whole; the finished file is offered as a download. The
//! desktop app's webview saves it to the downloads folder and tells the page, like the recovery kit.

use teamcraft_client_api::admin::AuditAction;
use teamcraft_client_api::audit_csv::{AuditCsvError, AuditCsvPager, AuditCsvStep};
use teamcraft_proto::{
  AuditEntryId, AuditEntryResponse, AuditExportRequest, ListAuditResponse, UnixMillis, UserId,
};
use wasm_bindgen::JsCast;

use crate::admin::AuditFilter;
use crate::admin_live::action_from_wire;
use crate::auth::ApiMethod;
use crate::feedback::ActionFailure;
use crate::rest::{RestClient, RestError};

/// What the page buffers before it hands a piece to the browser.
const PIECE_BYTES: usize = 1024 * 1024;
/// Entries asked for per page; the server may give fewer (its page size limit).
const PAGE_LIMIT: u32 = 200;
/// How long the download's object URL stays valid; the browser has the file long before.
const OBJECT_URL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(60);

/// What the console's filters ask for. The server applies the actor and the period; the action
/// and the search text are the console's own, so they are applied to each page here.
#[derive(Clone, Debug, Default)]
pub struct AuditExport {
  pub filter: AuditFilter,
  pub maybe_action: Option<AuditAction>,
  /// Lower-case search text, matched against the raw action, target and detail.
  pub search: String,
}

impl AuditExport {
  /// Whether the entry passes the filters the server doesn't apply.
  fn keeps(&self, entry: &AuditEntryResponse) -> bool {
    self
      .maybe_action
      .is_none_or(|wanted| action_from_wire(&entry.action) == wanted)
      && (self.search.is_empty() || matches_search(entry, &self.search))
  }

  fn request(&self) -> AuditExportRequest {
    AuditExportRequest {
      maybe_actor: self.filter.maybe_actor,
      maybe_action: self.maybe_action.map(|action| action.slug().to_owned()),
      maybe_since: self.filter.maybe_since.map(UnixMillis),
      maybe_until: self.filter.maybe_until.map(UnixMillis),
      has_search: !self.search.is_empty(),
    }
  }
}

/// Writes the log into a file and offers it as a download named `file_name`; returns the number of
/// rows. The export is recorded in the audit log first, so one that is cut off leaves a trace.
pub async fn export_audit_csv(
  client: &RestClient,
  export: &AuditExport,
  actor_name: impl Fn(UserId) -> String,
  file_name: &str,
) -> Result<u64, ActionFailure> {
  let body =
    serde_json::to_string(&export.request()).map_err(|error| ActionFailure::other(error.to_string()))?;
  client
    .send(ApiMethod::Post, "/api/v1/admin/audit/export", Some(body))
    .await
    .map_err(|error| error.to_failure())?;
  let mut sink = CsvSink::new();
  sink.write(&AuditCsvPager::header())?;
  let mut pager = AuditCsvPager::new();
  while let AuditCsvStep::Read(maybe_cursor) = pager.next_step() {
    let path = page_path(export.filter, maybe_cursor);
    let page: ListAuditResponse = client
      .get(&path)
      .await
      .map_err(|error: RestError| error.to_failure())?;
    let text = pager
      .accept(&page, |entry| export.keeps(entry), &actor_name)
      .map_err(|error: AuditCsvError| ActionFailure::other(error.to_string()))?;
    sink.write(&text)?;
  }
  sink.finish(file_name)?;
  Ok(pager.row_count())
}

/// `GET /api/v1/admin/audit` for one page of the export.
fn page_path(filter: AuditFilter, maybe_cursor: Option<AuditEntryId>) -> String {
  let mut path = format!("/api/v1/admin/audit?limit={PAGE_LIMIT}");
  if let Some(actor) = filter.maybe_actor {
    path.push_str(&format!("&actor={actor}"));
  }
  if let Some(since) = filter.maybe_since {
    path.push_str(&format!("&since={since}"));
  }
  if let Some(until) = filter.maybe_until {
    path.push_str(&format!("&until={until}"));
  }
  if let Some(cursor) = maybe_cursor {
    path.push_str(&format!("&before={cursor}"));
  }
  path
}

fn matches_search(entry: &AuditEntryResponse, search: &str) -> bool {
  [
    Some(entry.action.as_str()),
    entry.maybe_target_kind.as_deref(),
    entry.maybe_target_id.as_deref(),
    Some(entry.detail.as_str()),
  ]
  .into_iter()
  .flatten()
  .any(|text| text.to_lowercase().contains(search))
}

/// The file as pieces the browser holds.
struct CsvSink {
  pieces: js_sys::Array,
  buffer: String,
}

impl CsvSink {
  fn new() -> Self {
    Self {
      pieces: js_sys::Array::new(),
      buffer: String::with_capacity(PIECE_BYTES),
    }
  }

  fn write(&mut self, text: &str) -> Result<(), ActionFailure> {
    self.buffer.push_str(text);
    if self.buffer.len() >= PIECE_BYTES {
      self.seal()?;
    }
    Ok(())
  }

  fn finish(mut self, file_name: &str) -> Result<(), ActionFailure> {
    self.seal()?;
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("text/csv;charset=utf-8");
    let blob =
      web_sys::Blob::new_with_blob_sequence_and_options(&self.pieces, &options).map_err(js_failure)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js_failure)?;
    let document = web_sys::window()
      .and_then(|window| window.document())
      .ok_or_else(|| ActionFailure::other("no document"))?;
    let anchor: web_sys::HtmlAnchorElement = document
      .create_element("a")
      .map_err(js_failure)?
      .dyn_into()
      .map_err(|_| ActionFailure::other("not an anchor"))?;
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

  /// Hands the buffer to the browser as one more piece of the file.
  fn seal(&mut self) -> Result<(), ActionFailure> {
    if self.buffer.is_empty() {
      return Ok(());
    }
    let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(&self.buffer));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type("text/csv;charset=utf-8");
    let piece = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).map_err(js_failure)?;
    self.pieces.push(&piece);
    self.buffer.clear();
    Ok(())
  }
}

fn js_failure(error: wasm_bindgen::JsValue) -> ActionFailure {
  ActionFailure::other(format!("{error:?}"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{Id, Snowflake};

  #[test]
  fn the_first_page_has_no_cursor_and_the_filters_go_to_the_server() {
    assert_eq!(
      page_path(AuditFilter::default(), None),
      "/api/v1/admin/audit?limit=200"
    );
    let filter = AuditFilter {
      maybe_actor: None,
      maybe_since: Some(1000),
      maybe_until: Some(2000),
    };
    let cursor = AuditEntryId::from_snowflake(Snowflake::new(42).expect("id"));
    assert_eq!(
      page_path(filter, Some(cursor)),
      "/api/v1/admin/audit?limit=200&since=1000&until=2000&before=42"
    );
  }

  #[test]
  fn the_console_filters_apply_to_the_raw_entry() {
    let entry = entry("space_created", "{\"name\":\"Team\"}");
    let by_action = |action| AuditExport {
      maybe_action: Some(action),
      ..AuditExport::default()
    };
    assert!(by_action(AuditAction::SpaceCreated).keeps(&entry));
    assert!(!by_action(AuditAction::ChannelDeleted).keeps(&entry));
    let by_search = |text: &str| AuditExport {
      search: text.to_lowercase(),
      ..AuditExport::default()
    };
    assert!(by_search("TEAM").keeps(&entry));
    assert!(!by_search("other").keeps(&entry));
  }

  #[test]
  fn the_recorded_request_names_the_filters_but_not_the_search_text() {
    let export = AuditExport {
      maybe_action: Some(AuditAction::SpaceCreated),
      search: "secret".to_owned(),
      ..AuditExport::default()
    };
    let request = export.request();
    assert_eq!(request.maybe_action.as_deref(), Some("space-created"));
    assert!(request.has_search);
    assert!(!serde_json::to_string(&request).expect("json").contains("secret"));
  }

  fn entry(action: &str, detail: &str) -> AuditEntryResponse {
    AuditEntryResponse {
      id: AuditEntryId::from_snowflake(Snowflake::new(1).expect("id")),
      maybe_actor_user_id: None,
      action: action.to_owned(),
      maybe_target_kind: None,
      maybe_target_id: None,
      detail: detail.to_owned(),
      created_at: UnixMillis(0),
    }
  }
}
