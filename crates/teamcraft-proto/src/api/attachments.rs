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

/// `POST /api/v1/attachments/{id}/share-links`: makes an attachment's ciphertext downloadable without
/// an account (e2ee.md §4.5.1). The key travels only in the link's `#fragment`, which the creating
/// client appends; the server never sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateShareLinkRequest {
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  #[serde(rename = "max_downloads", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_downloads: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareLinkResponse {
  /// Unguessable token in the public URL.
  pub share_id: String,
  pub attachment_id: AttachmentId,
  /// The public download page, without fragment (`https://chat.example.com/f/<share_id>`).
  pub url: String,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  #[serde(rename = "max_downloads", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_downloads: Option<u32>,
  pub download_count: u32,
  pub is_revoked: bool,
}
