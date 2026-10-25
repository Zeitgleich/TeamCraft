//! The public download page of a share link (`/f/<share-id>#<fragment>`, e2ee.md §4.5.1): what the
//! page asks the server and how it opens the file. The fragment carries the file key and never
//! reaches the server; the server only knows the share ID and serves the ciphertext.

#[cfg(target_arch = "wasm32")]
pub use browser::{ShareClient, download_to_blob};

use crate::object_url::OPAQUE_TYPE;

const PATH_PREFIX: &str = "/f/";
/// Longest share ID the page accepts; the server's IDs are far shorter.
const MAX_ID_LEN: usize = 128;
/// The page decrypts the stream into the browser's blob storage; larger files go to the app.
pub const MAX_PAGE_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// The decrypted file is never a type the browser would render or run (a shared `.html` must not
/// open in the page's origin): `application/octet-stream`, whatever the name says.
pub const DOWNLOAD_BLOB_TYPE: &str = OPAQUE_TYPE;
/// Shown and saved when the name is empty after cleaning.
const FALLBACK_FILE_NAME: &str = "download";

/// What went wrong, in terms the downloader can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareError {
  /// Expired, used up, revoked, switched off or never existed (the server does not say which).
  Unavailable,
  /// The file is larger than the page opens.
  TooLarge,
  /// The server or the storage could not be reached.
  Network,
  /// The download did not decrypt: a link for another file, or a damaged download.
  Damaged,
}

/// The share ID and fragment of a download link, or `None` when the page isn't one.
pub fn parse_share_link(path: &str, fragment: &str) -> Option<(String, String)> {
  let id = path.strip_prefix(PATH_PREFIX)?.trim_end_matches('/');
  let is_plain = !id.is_empty()
    && id.len() <= MAX_ID_LEN
    && id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
  let fragment = fragment.trim_start_matches('#');
  (is_plain && !fragment.is_empty()).then(|| (id.to_owned(), fragment.to_owned()))
}

/// The name of a shared file as the page shows and saves it. It comes from the sender and is only
/// text: bidirectional control characters (which can make `evil\u{202E}gpj.exe` read as `evilexe.jpg`)
/// and other control characters are dropped, and path separators become `_`.
pub fn display_file_name(name: &str) -> String {
  let cleaned: String = name
    .chars()
    .filter(|c| !c.is_control() && !is_bidi_control(*c))
    .map(|c| if matches!(c, '/' | '\\') { '_' } else { c })
    .collect();
  let cleaned = cleaned.trim();
  if cleaned.is_empty() {
    FALLBACK_FILE_NAME.to_owned()
  } else {
    cleaned.to_owned()
  }
}

fn is_bidi_control(c: char) -> bool {
  matches!(
    c,
    '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
  )
}

/// The part that needs the browser and the core's crypto (wasm only, like `web_core`).
#[cfg(target_arch = "wasm32")]
mod browser {
  use js_sys::{Array, Function, Reflect, Uint8Array};
  use teamcraft_crypto::attachment::AttachmentStreamDecryptor;
  use teamcraft_crypto::share_link::OpenedShareLink;
  use teamcraft_proto::{ErrorBody, ShareDownloadRequest, ShareDownloadResponse, ShareInfoResponse};
  use wasm_bindgen::{JsCast, JsValue};
  use wasm_bindgen_futures::JsFuture;

  use super::{DOWNLOAD_BLOB_TYPE, MAX_PAGE_DOWNLOAD_BYTES, ShareError};
  use crate::auth::AppConfig;
  use crate::auth::fetch::fetch;

  /// Requests of the download page. No account: the share ID is the only credential.
  #[derive(Clone)]
  pub struct ShareClient {
    config: AppConfig,
  }

  impl ShareClient {
    pub fn new(config: AppConfig) -> Self {
      Self { config }
    }

    pub async fn info(&self, id: &str) -> Result<ShareInfoResponse, ShareError> {
      let response = self.call("GET", &format!("/api/v1/shares/{id}"), None).await?;
      serde_json::from_slice(&response).map_err(|_| ShareError::Network)
    }

    /// Counts the download and returns where the ciphertext is. `maybe_grant` is the grant of an
    /// earlier answer when this is the retry of a download that failed: the server then doesn't
    /// count it again.
    pub async fn start_download(
      &self,
      id: &str,
      maybe_grant: Option<String>,
    ) -> Result<ShareDownloadResponse, ShareError> {
      let body =
        serde_json::to_vec(&ShareDownloadRequest { maybe_grant }).map_err(|_| ShareError::Network)?;
      let response = self
        .call("POST", &format!("/api/v1/shares/{id}/download"), Some(body))
        .await?;
      serde_json::from_slice(&response).map_err(|_| ShareError::Network)
    }

    async fn call(
      &self,
      method: &str,
      path: &str,
      maybe_body: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, ShareError> {
      let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
      if maybe_body.is_some() {
        headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      let response = fetch(method, &self.config.api_url(path), &headers, maybe_body)
        .await
        .map_err(|_| ShareError::Network)?;
      if response.status < 300 {
        return Ok(response.body);
      }
      let maybe_code = serde_json::from_slice::<ErrorBody>(&response.body)
        .ok()
        .map(|body| body.error_code.as_str().to_owned());
      Err(match (response.status, maybe_code.as_deref()) {
        (404 | 410, _) | (_, Some("share_unavailable")) => ShareError::Unavailable,
        _ => ShareError::Network,
      })
    }
  }

  /// Fetches the ciphertext from `url` as a stream and decrypts it piece by piece into a `Blob`
  /// (type `application/octet-stream`): the page never holds the whole ciphertext or the whole
  /// plaintext as one buffer. Waiting for the next piece hands the event loop back, so the page
  /// stays responsive. Nothing is offered before the end of the stream authenticated.
  pub async fn download_to_blob(url: &str, opened: &OpenedShareLink) -> Result<web_sys::Blob, ShareError> {
    let reader = open_stream(url).await?;
    let parts = Array::new();
    let mut decryptor = AttachmentStreamDecryptor::new(&opened.key);
    let mut received: u64 = 0;
    let mut sink = |chunk: &[u8]| {
      parts.push(&Uint8Array::from(chunk));
    };
    while let Some(piece) = next_piece(&reader).await? {
      received = received.saturating_add(piece.len() as u64);
      if received > MAX_PAGE_DOWNLOAD_BYTES.saturating_add(1024 * 1024) {
        cancel(&reader);
        return Err(ShareError::TooLarge);
      }
      decryptor
        .push(&piece, &mut sink)
        .map_err(|_| ShareError::Damaged)?;
    }
    decryptor.finish(&mut sink).map_err(|_| ShareError::Damaged)?;
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(DOWNLOAD_BLOB_TYPE);
    web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(|_| ShareError::Damaged)
  }

  /// The body's reader. Redirects are refused, like for the API: the URL is the server's word.
  async fn open_stream(url: &str) -> Result<JsValue, ShareError> {
    let window = web_sys::window().ok_or(ShareError::Network)?;
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    init.set_redirect(web_sys::RequestRedirect::Error);
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(|_| ShareError::Network)?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
      .await
      .map_err(|_| ShareError::Network)?
      .dyn_into()
      .map_err(|_| ShareError::Network)?;
    match response.status() {
      200..=299 => {},
      // A presigned URL that ran out or was refused.
      403 | 404 | 410 => return Err(ShareError::Unavailable),
      _ => return Err(ShareError::Network),
    }
    let body = response.body().ok_or(ShareError::Network)?;
    call_method(&body, "getReader")
  }

  /// The next piece of the stream, or `None` at its end.
  async fn next_piece(reader: &JsValue) -> Result<Option<Vec<u8>>, ShareError> {
    let result = JsFuture::from(js_sys::Promise::from(call_method(reader, "read")?))
      .await
      .map_err(|_| ShareError::Network)?;
    let is_done = Reflect::get(&result, &"done".into())
      .map_err(|_| ShareError::Network)?
      .is_truthy();
    if is_done {
      return Ok(None);
    }
    let value = Reflect::get(&result, &"value".into()).map_err(|_| ShareError::Network)?;
    let bytes: Uint8Array = value.dyn_into().map_err(|_| ShareError::Network)?;
    Ok(Some(bytes.to_vec()))
  }

  fn cancel(reader: &JsValue) {
    // The download is abandoned either way; a failing cancel changes nothing.
    drop(call_method(reader, "cancel"));
  }

  fn call_method(target: &JsValue, name: &str) -> Result<JsValue, ShareError> {
    let function: Function = Reflect::get(target, &name.into())
      .map_err(|_| ShareError::Network)?
      .dyn_into()
      .map_err(|_| ShareError::Network)?;
    function.call0(target).map_err(|_| ShareError::Network)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_link_needs_a_plain_id_and_a_fragment() {
    assert_eq!(
      parse_share_link("/f/Ab-9_x", "#abc"),
      Some(("Ab-9_x".into(), "abc".into()))
    );
    assert_eq!(
      parse_share_link("/f/Ab9/", "abc").map(|link| link.0),
      Some("Ab9".into())
    );
  }

  #[test]
  fn the_page_never_hands_out_a_renderable_type() {
    assert_eq!(DOWNLOAD_BLOB_TYPE, "application/octet-stream");
  }

  #[test]
  fn file_names_lose_direction_tricks_controls_and_separators() {
    assert_eq!(display_file_name("report.pdf"), "report.pdf");
    // "invoice\u{202E}gpj.exe" would read as "invoiceexe.jpg" on screen.
    assert_eq!(display_file_name("invoice\u{202E}gpj.exe"), "invoicegpj.exe");
    assert_eq!(
      display_file_name("a\u{200E}b\u{200F}c\u{2066}d\u{2069}e\u{061C}f"),
      "abcdef"
    );
    assert_eq!(
      display_file_name("a\u{202A}b\u{202B}c\u{202C}d\u{202D}e"),
      "abcde"
    );
    assert_eq!(display_file_name("../etc/passwd"), ".._etc_passwd");
    assert_eq!(display_file_name("a\\b"), "a_b");
    assert_eq!(display_file_name("tab\there\nnewline"), "tabherenewline");
    assert_eq!(display_file_name(" \u{202E} "), "download");
    assert_eq!(display_file_name(""), "download");
    assert_eq!(display_file_name("Grüße 日本語.txt"), "Grüße 日本語.txt");
  }

  #[test]
  fn other_pages_and_hostile_ids_are_no_links() {
    for (path, fragment) in [
      ("/f/abc", ""),
      ("/f/abc", "#"),
      ("/f/", "#abc"),
      ("/f/a/b", "#abc"),
      ("/f/a%2Fb", "#abc"),
      ("/f/../x", "#abc"),
      ("/meeting/1", "#abc"),
      ("/", "#abc"),
    ] {
      assert_eq!(parse_share_link(path, fragment), None, "{path}{fragment}");
    }
    let long = format!("/f/{}", "a".repeat(MAX_ID_LEN + 1));
    assert_eq!(parse_share_link(&long, "#abc"), None);
  }
}
