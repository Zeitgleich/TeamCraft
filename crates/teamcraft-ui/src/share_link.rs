//! The public download page of a share link (`/f/<share-id>#<fragment>`, e2ee.md §4.5.1): what the
//! page asks the server and how it opens the file. The fragment carries the file key and never
//! reaches the server; the server only knows the share ID and serves the ciphertext.

#[cfg(target_arch = "wasm32")]
pub use browser::{ShareClient, decrypt_to_blob};

const PATH_PREFIX: &str = "/f/";
/// Longest share ID the page accepts; the server's IDs are far shorter.
const MAX_ID_LEN: usize = 128;
/// The page holds the ciphertext and builds the file from chunks, so larger files go to the app.
pub const MAX_PAGE_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;

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

/// The part that needs the browser and the core's crypto (wasm only, like `web_core`).
#[cfg(target_arch = "wasm32")]
mod browser {
  use js_sys::{Array, Uint8Array};
  use teamcraft_crypto::attachment::decrypt_attachment_chunks;
  use teamcraft_crypto::share_link::OpenedShareLink;
  use teamcraft_proto::{ErrorBody, ShareDownloadResponse, ShareInfoResponse};

  use super::ShareError;
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
      let response = self.get_json("GET", &format!("/api/v1/shares/{id}")).await?;
      serde_json::from_slice(&response).map_err(|_| ShareError::Network)
    }

    /// Counts the download and returns where the ciphertext is.
    pub async fn start_download(&self, id: &str) -> Result<ShareDownloadResponse, ShareError> {
      let response = self
        .get_json("POST", &format!("/api/v1/shares/{id}/download"))
        .await?;
      serde_json::from_slice(&response).map_err(|_| ShareError::Network)
    }

    /// Fetches the ciphertext from the storage URL the server returned.
    pub async fn ciphertext(&self, url: &str) -> Result<Vec<u8>, ShareError> {
      let response = fetch("GET", url, &[], None)
        .await
        .map_err(|_| ShareError::Network)?;
      match response.status {
        200..=299 => Ok(response.body),
        // A presigned URL that ran out between the two requests.
        403 | 404 | 410 => Err(ShareError::Unavailable),
        _ => Err(ShareError::Network),
      }
    }

    async fn get_json(&self, method: &str, path: &str) -> Result<Vec<u8>, ShareError> {
      let headers = [("Accept".to_owned(), "application/json".to_owned())];
      let response = fetch(method, &self.config.api_url(path), &headers, None)
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

  /// Decrypts the download into a browser `Blob`, chunk by chunk, so the plaintext is never one big
  /// buffer in the page's memory.
  pub fn decrypt_to_blob(opened: &OpenedShareLink, ciphertext: &[u8]) -> Result<web_sys::Blob, ShareError> {
    let parts = Array::new();
    decrypt_attachment_chunks(&opened.key, ciphertext, |chunk| {
      parts.push(&Uint8Array::from(chunk));
    })
    .map_err(|_| ShareError::Damaged)?;
    web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(|_| ShareError::Damaged)
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
