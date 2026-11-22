//! What this device itself accepted of the workspace's signed assets (custom emoji, sticker packs;
//! e2ee.md §4.13, "Former admins"), to judge documents of somebody who is no longer an admin.
//!
//! A document's signing time and roster version are the signer's word: a demoted admin who is still
//! a member can sign a new document that names a time or roster version inside their old tenure.
//! What they can't choose is what this device saw, and when. So a document that passes only through
//! a past tenure counts here only if this device accepted it before it verified the signer's
//! demotion. The ledger is local: never signed, never sent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Documents a ledger keeps. Far beyond the emoji and pack limits times their renames; past it the
/// oldest go, and a former admin's document among them stops counting on this device (an admin can
/// sign it again).
const MAX_HELD_DOCUMENTS: usize = 4096;

/// The documents of one kind of asset this device accepted, by the SHA-256 of their signed payload.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetLedger {
  /// The roster version this device had verified when it began keeping the ledger. A demotion in a
  /// roster up to this one happened before the device watched: its documents are judged without the
  /// ledger, as before (e2ee.md §4.13, "What it does not stop").
  #[serde(
    rename = "since_roster_version",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_since_roster_version: Option<u64>,
  /// Hex SHA-256 of each accepted payload, and when it was first held (a counter, oldest lowest).
  #[serde(default)]
  held: BTreeMap<String, u64>,
  #[serde(default)]
  next_seq: u64,
}

/// Who signed a document whose signature verified, as far as this device's verified roster says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssetSigner {
  /// An owner or admin of the roster now.
  Admin,
  /// Somebody who is not, verified through a past tenure of the admin history; with the roster
  /// version that ended it, if the history knows one.
  FormerAdmin { maybe_demoted_at: Option<u64> },
}

impl AssetLedger {
  /// The ledger kept under `key`; empty before the first read.
  pub(crate) fn load(store: &impl LocalStore, key: &str) -> Result<Self, CoreError> {
    Ok(records::get_json(store, Table::Workspace, key)?.unwrap_or_default())
  }

  pub(crate) fn save(&self, store: &impl LocalStore, key: &str) -> Result<(), CoreError> {
    records::put_json(store, Table::Workspace, key, self)
  }

  /// Starts keeping the ledger at the roster this device verified, once there is one.
  pub(crate) fn follow(&mut self, roster_version: u64) {
    if self.maybe_since_roster_version.is_none() && roster_version > 0 {
      self.maybe_since_roster_version = Some(roster_version);
    }
  }

  /// Whether a document with `payload`, signed by `signer`, counts on this device. An admin's
  /// always does. A former admin's does if this device held it already, or if their demotion came
  /// before this device kept the ledger (it has nothing to compare with). A document first seen
  /// after a demotion this device followed is refused.
  pub(crate) fn admits(&self, payload: &[u8], signer: AssetSigner) -> bool {
    match signer {
      AssetSigner::Admin => true,
      AssetSigner::FormerAdmin { maybe_demoted_at } => {
        let has_followed_the_demotion = match (maybe_demoted_at, self.maybe_since_roster_version) {
          (Some(demoted_at), Some(since)) => demoted_at > since,
          // An open tenure of somebody not on the roster (a history this device couldn't complete),
          // or a ledger not begun: nothing to judge by.
          _ => false,
        };
        !has_followed_the_demotion || self.held.contains_key(&digest(payload))
      },
    }
  }

  /// Notes a document this device accepted.
  pub(crate) fn hold(&mut self, payload: &[u8]) {
    let seq = self.next_seq;
    self.next_seq = self.next_seq.wrapping_add(1);
    self.held.entry(digest(payload)).or_insert(seq);
    while self.held.len() > MAX_HELD_DOCUMENTS {
      let Some(oldest) = self
        .held
        .iter()
        .min_by_key(|(_, seq)| **seq)
        .map(|(key, _)| key.clone())
      else {
        break;
      };
      self.held.remove(&oldest);
    }
  }
}

fn digest(payload: &[u8]) -> String {
  teamcraft_crypto::attachment::content_sha256(payload)
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  const DEMOTED: AssetSigner = AssetSigner::FormerAdmin {
    maybe_demoted_at: Some(4),
  };

  #[test]
  fn an_admins_document_always_counts() {
    assert!(AssetLedger::default().admits(b"doc", AssetSigner::Admin));
  }

  #[test]
  fn a_former_admins_document_counts_only_if_held_before_a_followed_demotion() {
    let mut ledger = AssetLedger::default();
    ledger.follow(2);
    ledger.hold(b"early");
    assert!(ledger.admits(b"early", DEMOTED));
    assert!(!ledger.admits(b"new", DEMOTED));
  }

  #[test]
  fn a_demotion_before_the_ledger_began_is_judged_without_it() {
    let mut ledger = AssetLedger::default();
    ledger.follow(4);
    assert!(ledger.admits(b"new", DEMOTED));
    // Without a known end, or without a ledger, there is nothing to compare with.
    let open = AssetSigner::FormerAdmin {
      maybe_demoted_at: None,
    };
    assert!(ledger.admits(b"new", open));
    assert!(AssetLedger::default().admits(b"new", DEMOTED));
  }

  #[test]
  fn the_ledger_begins_once_and_at_a_real_roster() {
    let mut ledger = AssetLedger::default();
    ledger.follow(0);
    assert_eq!(ledger.maybe_since_roster_version, None);
    ledger.follow(3);
    ledger.follow(7);
    assert_eq!(ledger.maybe_since_roster_version, Some(3));
  }

  #[test]
  fn the_ledger_is_bounded_and_drops_the_oldest() {
    let mut ledger = AssetLedger::default();
    ledger.follow(1);
    for index in 0..=MAX_HELD_DOCUMENTS {
      ledger.hold(format!("doc {index}").as_bytes());
    }
    assert_eq!(ledger.held.len(), MAX_HELD_DOCUMENTS);
    assert!(!ledger.admits(b"doc 0", DEMOTED));
    assert!(ledger.admits(format!("doc {MAX_HELD_DOCUMENTS}").as_bytes(), DEMOTED));
  }
}
