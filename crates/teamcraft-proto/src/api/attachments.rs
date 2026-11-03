use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AttachmentId, ConversationId, UnixMillis};

/// `POST /api/v1/attachments`: announces an upload. Clients encrypt the file themselves (fresh random
/// key per file, e2ee.md §4.5), upload the ciphertext to the returned URL and put the key into the
/// encrypted message; the server never sees a key or a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateAttachmentRequest {
  pub conversation_id: ConversationId,
  /// Size of the encrypted blob in bytes; the upload URL only accepts exactly this size.
  pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateAttachmentResponse {
  pub id: AttachmentId,
  /// Presigned `PUT` URL; send the blob with `Content-Length` equal to `size_bytes`.
  pub upload_url: String,
  pub expires_at: UnixMillis,
}

/// `GET /api/v1/attachments/{id}/download`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentDownloadResponse {
  /// Presigned `GET` URL of the encrypted blob.
  pub download_url: String,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}

/// Response of `POST /api/v1/attachments/{id}/complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentResponse {
  pub id: AttachmentId,
  pub conversation_id: ConversationId,
  pub size_bytes: u64,
  pub is_complete: bool,
}

/// Response of `POST /api/v1/attachments/multipart` (body: [`CreateAttachmentRequest`]): announces a
/// large upload that goes up in parts, each through its own presigned URL
/// (`POST /api/v1/attachments/{id}/parts`). A part that failed is sent again on its own, and an
/// interrupted upload continues at the next part, also after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateMultipartAttachmentResponse {
  pub id: AttachmentId,
  /// Every part except the last must have at least this many bytes.
  pub min_part_size_bytes: u64,
  /// No part may have more bytes than this.
  pub max_part_size_bytes: u64,
  /// Highest part number the upload may use (parts are numbered from 1); part `n` holds at most
  /// what the announced size leaves after `n - 1` parts of `min_part_size_bytes`.
  pub max_parts: u16,
  /// Until when the upload can be continued without new parts; each presigned part extends it.
  pub expires_at: UnixMillis,
}

/// `POST /api/v1/attachments/{id}/parts`: a presigned URL for one part of a multipart upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PresignAttachmentPartRequest {
  /// From 1 to `max_parts`, in blob order.
  pub part_number: u16,
  /// The part's size; the URL only accepts exactly this many bytes.
  pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AttachmentPartUrlResponse {
  /// Presigned `PUT` URL; the storage answers with the part's `ETag`, which completion needs.
  pub upload_url: String,
  pub expires_at: UnixMillis,
}

/// `POST /api/v1/attachments/{id}/multipart/complete`: joins the uploaded parts into the blob. The
/// server then checks the announced size, like `POST /api/v1/attachments/{id}/complete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompleteMultipartAttachmentRequest {
  /// Every part, numbered 1 to n without gaps.
  pub parts: Vec<UploadedPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UploadedPart {
  pub part_number: u16,
  /// The `ETag` the storage answered the part's upload with.
  pub etag: String,
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn multipart_wire_format_is_stable() {
    let request = CompleteMultipartAttachmentRequest {
      parts: vec![UploadedPart {
        part_number: 1,
        etag: "\"abc\"".into(),
      }],
    };
    assert_eq!(
      serde_json::to_value(&request).unwrap(),
      json!({ "parts": [{ "part_number": 1, "etag": "\"abc\"" }] })
    );
    let part = PresignAttachmentPartRequest {
      part_number: 2,
      size_bytes: 8,
    };
    assert_eq!(
      serde_json::to_value(&part).unwrap(),
      json!({ "part_number": 2, "size_bytes": 8 })
    );
  }
}
