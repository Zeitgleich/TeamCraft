//! Workspace log entries (roster and policy, admin.md §2.3) as clients receive them from the
//! untrusted server: arbitrary payloads and signatures must be rejected cleanly, never panic, and
//! never be accepted as owner-signed.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use teamcraft_core::workspace::WorkspaceState;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids::UserId as CryptoUserId;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, verify_log_entry};
use teamcraft_proto::{Base64Bytes, UnixMillis, WorkspaceLogEntryResponse, WorkspaceLogKind};

/// Every user has the same pinned key, so fuzzed certificates reach the signature checks.
struct EveryoneHasKey([u8; 32]);

impl IdentityDirectory for EveryoneHasKey {
  fn pinned_identity(&self, _: CryptoUserId) -> Option<IdentityPublicKey> {
    Some(IdentityPublicKey::from_bytes(self.0))
  }
}

#[derive(Debug, Arbitrary)]
struct Entry {
  is_roster: bool,
  version: u8,
  created_at: i32,
  payload: Vec<u8>,
  signature: Vec<u8>,
}

fuzz_target!(|input: (Vec<Entry>, [u8; 32])| {
  let (entries, key) = input;
  let directory = EveryoneHasKey(key);
  for entry in &entries {
    let log = if entry.is_roster { WorkspaceLog::Roster } else { WorkspaceLog::Policy };
    assert!(
      verify_log_entry(&entry.signature, log, u64::from(entry.version), &entry.payload, &directory).is_err(),
      "random bytes verified as a signed log entry"
    );
  }
  let mut state = WorkspaceState::default();
  let response = entries
    .into_iter()
    .map(|entry| WorkspaceLogEntryResponse {
      kind: if entry.is_roster { WorkspaceLogKind::Roster } else { WorkspaceLogKind::Policy },
      version: u64::from(entry.version),
      payload: Base64Bytes::new(entry.payload),
      signature: Base64Bytes::new(entry.signature),
      maybe_signer_device_id: None,
      created_at: UnixMillis(i64::from(entry.created_at)),
    })
    .collect();
  let _ = state.apply_entries(response);
});
