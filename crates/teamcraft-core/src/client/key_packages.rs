//! Deleting the private parts of this device's key packages once nobody can add it with them any
//! more (e2ee.md §4.3.1, issue #148): packages that expired, and last-resort packages the server
//! replaced, each after [`ClientSettings::key_package_retention`] for welcomes still on the way.
//! openmls itself only deletes a regular package when a welcome uses it.
//!
//! [`ClientSettings::key_package_retention`]: super::ClientSettings::key_package_retention

use std::collections::BTreeSet;

use teamcraft_crypto::mls::{StoredKeyPackage, delete_key_package, key_package_ref, stored_key_packages};
use teamcraft_proto::Base64Bytes;

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Where the device notes when its earlier last-resort packages were replaced on the server.
const REPLACED_LAST_RESORT_KEY: &str = "replaced_last_resort_key_packages";

/// A last-resort package of this device that a newer one replaced on the server: nobody claims it
/// after `replaced_at_unix_secs`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ReplacedPackage {
  hash_ref: Base64Bytes,
  replaced_at_unix_secs: u64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The key packages whose private parts this device holds.
  pub fn stored_key_packages(&self) -> Result<Vec<StoredKeyPackage>, CoreError> {
    Ok(stored_key_packages(&self.provider)?)
  }

  /// Housekeeping after a key package check: notes the replaced last-resort packages when the
  /// server took `maybe_new_last_resort`, then deletes spent private parts, once all welcomes were
  /// read since the last reset (one waiting for a package is opened first; issue #395): not in the
  /// middle of a welcome sync, nor on a connection that wasn't caught up yet. Failures are logged,
  /// never fatal: a sync must not stop over cleanup.
  pub(super) fn clean_up_key_packages(
    &mut self,
    maybe_new_last_resort: Option<&Base64Bytes>,
    now_unix_secs: u64,
  ) {
    if let Some(last_resort) = maybe_new_last_resort {
      let noted = key_package_ref(last_resort.as_slice())
        .map_err(CoreError::from)
        .and_then(|current| self.note_last_resort_replaced(&current, now_unix_secs));
      if let Err(error) = noted {
        tracing::warn!(%error, "could not note the replaced last-resort key package");
      }
    }
    if !self.are_welcomes_synced {
      return;
    }
    if let Err(error) = self.forget_spent_key_packages(now_unix_secs) {
      tracing::warn!(%error, "could not delete spent key packages");
    }
  }

  /// Notes, after the server took the new last-resort package `current`, that every other
  /// last-resort package this device holds is replaced from `now_unix_secs` on. Only after the
  /// server's yes: a lost answer leaves the old one counted as live, never the other way round.
  fn note_last_resort_replaced(&mut self, current: &[u8], now_unix_secs: u64) -> Result<(), CoreError> {
    let mut replaced = self.replaced_last_resort()?;
    let known: BTreeSet<Vec<u8>> = replaced
      .iter()
      .map(|entry| entry.hash_ref.as_slice().to_vec())
      .collect();
    for package in self.stored_key_packages()? {
      if package.is_last_resort && package.hash_ref != current && !known.contains(&package.hash_ref) {
        replaced.push(ReplacedPackage {
          hash_ref: Base64Bytes::new(package.hash_ref),
          replaced_at_unix_secs: now_unix_secs,
        });
      }
    }
    records::put_json(&self.store, Table::Account, REPLACED_LAST_RESORT_KEY, &replaced)
  }

  /// Deletes the private parts of key packages nobody can add this device with any more; returns
  /// how many. `now_unix_secs` is the clock the packages' lifetimes were made from.
  fn forget_spent_key_packages(&mut self, now_unix_secs: u64) -> Result<usize, CoreError> {
    let stored = self.stored_key_packages()?;
    let replaced = self.replaced_last_resort()?;
    let retention_secs = self.settings.key_package_retention.as_secs();
    let spent = spent_key_packages(&stored, &replaced, now_unix_secs, retention_secs);
    for hash_ref in &spent {
      delete_key_package(&self.provider, hash_ref)?;
    }
    // Notes about packages that are gone are of no further use.
    let remaining: BTreeSet<&[u8]> = stored
      .iter()
      .map(|package| package.hash_ref.as_slice())
      .filter(|hash_ref| !spent.iter().any(|spent| spent.as_slice() == *hash_ref))
      .collect();
    let kept: Vec<ReplacedPackage> = replaced
      .iter()
      .filter(|entry| remaining.contains(entry.hash_ref.as_slice()))
      .cloned()
      .collect();
    if kept != replaced {
      records::put_json(&self.store, Table::Account, REPLACED_LAST_RESORT_KEY, &kept)?;
    }
    if !spent.is_empty() {
      self.save_mls_storage()?;
      // The bundles' records are deleted; their old bytes go from the files too.
      self.store.purge_freed()?;
      tracing::info!(deleted = spent.len(), "deleted spent key packages");
    }
    Ok(spent.len())
  }

  fn replaced_last_resort(&self) -> Result<Vec<ReplacedPackage>, CoreError> {
    Ok(records::get_json(&self.store, Table::Account, REPLACED_LAST_RESORT_KEY)?.unwrap_or_default())
  }
}

/// The hash references of the stored packages whose private parts can go: expired, or replaced on
/// the server (last resort), for at least `retention_secs`.
fn spent_key_packages(
  stored: &[StoredKeyPackage],
  replaced: &[ReplacedPackage],
  now_unix_secs: u64,
  retention_secs: u64,
) -> Vec<Vec<u8>> {
  let is_past = |since: u64| now_unix_secs >= since.saturating_add(retention_secs);
  stored
    .iter()
    .filter(|package| {
      let is_expired = is_past(package.not_after_unix_secs);
      let is_replaced = package.is_last_resort
        && replaced
          .iter()
          .any(|entry| entry.hash_ref.as_slice() == package.hash_ref && is_past(entry.replaced_at_unix_secs));
      is_expired || is_replaced
    })
    .map(|package| package.hash_ref.clone())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  const DAY_SECS: u64 = 24 * 60 * 60;
  const RETENTION_SECS: u64 = 30 * DAY_SECS;
  const NOW: u64 = 1_800_000_000;

  #[test]
  fn regular_packages_go_once_expired_for_the_retention() {
    let fresh = package(1, NOW + DAY_SECS, false);
    let just_expired = package(2, NOW - DAY_SECS, false);
    let long_expired = package(3, NOW - RETENTION_SECS, false);
    let spent = spent_key_packages(&[fresh, just_expired, long_expired], &[], NOW, RETENTION_SECS);
    assert_eq!(spent, vec![vec![3; 32]]);
  }

  #[test]
  fn a_replaced_last_resort_package_goes_after_the_retention() {
    let current = package(1, NOW + 50 * DAY_SECS, true);
    let replaced_long_ago = package(2, NOW + 20 * DAY_SECS, true);
    let replaced_recently = package(3, NOW + 40 * DAY_SECS, true);
    let replaced = [
      replaced_entry(2, NOW - RETENTION_SECS),
      replaced_entry(3, NOW - DAY_SECS),
    ];
    let spent = spent_key_packages(
      &[current, replaced_long_ago, replaced_recently],
      &replaced,
      NOW,
      RETENTION_SECS,
    );
    assert_eq!(spent, vec![vec![2; 32]]);
  }

  #[test]
  fn a_last_resort_package_without_a_replacement_stays_until_it_expired() {
    // A rotation whose answer was lost: the server may hand out either package, both stay.
    let older = package(1, NOW + DAY_SECS, true);
    let newer = package(2, NOW + 60 * DAY_SECS, true);
    assert!(spent_key_packages(&[older, newer], &[], NOW, RETENTION_SECS).is_empty());
  }

  #[test]
  fn a_replacement_note_never_deletes_a_regular_package() {
    let regular = package(1, NOW + DAY_SECS, false);
    let replaced = [replaced_entry(1, NOW - RETENTION_SECS)];
    assert!(spent_key_packages(&[regular], &replaced, NOW, RETENTION_SECS).is_empty());
  }

  fn package(byte: u8, not_after_unix_secs: u64, is_last_resort: bool) -> StoredKeyPackage {
    StoredKeyPackage {
      hash_ref: vec![byte; 32],
      not_before_unix_secs: not_after_unix_secs.saturating_sub(84 * DAY_SECS),
      not_after_unix_secs,
      is_last_resort,
    }
  }

  fn replaced_entry(byte: u8, replaced_at_unix_secs: u64) -> ReplacedPackage {
    ReplacedPackage {
      hash_ref: Base64Bytes::new(vec![byte; 32]),
      replaced_at_unix_secs,
    }
  }
}
