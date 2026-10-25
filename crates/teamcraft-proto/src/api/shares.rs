//! Share links for external download (e2ee.md §4.5.1). The link is
//! `<base>/f/<share-id>#<file-key>`; the part after `#` never reaches the server.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AttachmentId, UnixMillis};

/// `POST /api/v1/shares`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateShareLinkRequest {
  /// A completed attachment of a conversation the caller belongs to.
  pub attachment_id: AttachmentId,
  /// When the link stops working; the instance default if omitted, never later than the maximum.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// The link stops working after this many downloads.
  #[serde(rename = "max_downloads", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_downloads: Option<u32>,
  /// Alternative to `expires_at` that does not depend on the client's clock: the link lasts this
  /// long from the moment the server gets the request, at most the instance maximum (longer
  /// durations are shortened to it; the response says when the link ends). Not both.
  #[serde(
    rename = "expires_in_seconds",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_expires_in_seconds: Option<u64>,
  /// The client wrapped the file key with a password; the download page asks for it. The server
  /// never sees the password, it only tells the page to ask.
  #[serde(default)]
  pub is_password_protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareLinkResponse {
  /// Random, unguessable ID in the URL path.
  pub id: String,
  pub attachment_id: AttachmentId,
  /// `<base>/f/<id>` if the instance knows its public URL; append `#<file-key>` on the client.
  #[serde(rename = "url", default, skip_serializing_if = "Option::is_none")]
  pub maybe_url: Option<String>,
  pub created_at: UnixMillis,
  pub expires_at: UnixMillis,
  #[serde(rename = "max_downloads", default, skip_serializing_if = "Option::is_none")]
  pub maybe_max_downloads: Option<u32>,
  pub is_password_protected: bool,
  pub download_count: u32,
  #[serde(
    rename = "last_downloaded_at",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_last_downloaded_at: Option<UnixMillis>,
  pub is_revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListShareLinksResponse {
  pub links: Vec<ShareLinkResponse>,
}

/// `GET /api/v1/shares/{id}` (public): what the download page needs before it asks for the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareInfoResponse {
  pub size_bytes: u64,
  pub is_password_protected: bool,
  pub expires_at: UnixMillis,
}

/// `POST /api/v1/shares/{id}/download` (public).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareDownloadRequest {
  /// The grant of an earlier answer, to retry that download (the storage fetch failed) without
  /// counting it again. Valid for a short window, see `shares.download_retry_minutes`.
  #[serde(rename = "grant", default, skip_serializing_if = "Option::is_none")]
  pub maybe_grant: Option<String>,
}

/// Counts one download (unless the request carries the grant of one counted a moment ago) and
/// returns where the ciphertext is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShareDownloadResponse {
  /// Send back with a retry of this download so it is not counted twice.
  pub grant: String,
  pub download_url: String,
  pub size_bytes: u64,
  pub expires_at: UnixMillis,
}
