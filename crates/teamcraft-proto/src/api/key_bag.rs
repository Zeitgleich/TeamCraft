use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Base64Bytes, UnixMillis};

/// `PUT /api/v1/me/key-bag`: stores the encrypted key bag and the backup key wrapped for recovery
/// (e2ee.md §6). Updates are compare-and-swap: `expected_version` is the version the client
/// last read (0 to create). A stale version fails with `version_conflict`; the client fetches,
/// merges and retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutKeyBagRequest {
  pub expected_version: u64,
  pub encrypted_key_bag: Base64Bytes,
  pub wrapped_backup_keys: Base64Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct KeyBagResponse {
  pub version: u64,
  pub encrypted_key_bag: Base64Bytes,
  pub wrapped_backup_keys: Base64Bytes,
  pub updated_at: UnixMillis,
}
