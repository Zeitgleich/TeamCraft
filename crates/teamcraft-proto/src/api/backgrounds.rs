use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{BackgroundId, UnixMillis};

/// `POST /api/v1/me/backgrounds`: announces the upload of one of the caller's own call
/// backgrounds. Like attachments, the client encrypts the picture with a fresh key first; the key
/// travels in the user's encrypted key bag, so only the user's devices can open it (e2ee.md §4.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateBackgroundUploadRequest {
  /// Size of the encrypted picture in bytes; the upload URL only accepts exactly this size.
  pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateBackgroundUploadResponse {
  pub id: BackgroundId,
  /// Presigned `PUT` URL; send the blob with `Content-Length` equal to `size_bytes`.
  pub upload_url: String,
  pub expires_at: UnixMillis,
}

/// `GET /api/v1/me/backgrounds/{id}/download`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BackgroundDownloadResponse {
  /// Presigned `GET` URL of the encrypted picture.
  pub download_url: String,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn background_uploads_have_a_stable_wire_format() {
    let request: CreateBackgroundUploadRequest = serde_json::from_value(json!({"size_bytes": 1234})).unwrap();
    assert_eq!(request.size_bytes, 1234);
    let response = CreateBackgroundUploadResponse {
      id: "9".parse().unwrap(),
      upload_url: "https://s3.example.com/put".into(),
      expires_at: UnixMillis(5),
    };
    assert_eq!(
      serde_json::to_value(&response).unwrap(),
      json!({"id": "9", "upload_url": "https://s3.example.com/put", "expires_at": 5})
    );
  }
}
