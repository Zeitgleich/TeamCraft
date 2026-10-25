//! The private parts of this device's own key packages in its MLS storage (e2ee.md §4.3.1): listed
//! so the device can delete those nobody can add it with any more (issue #148). openmls deletes a
//! regular package's private part when a welcome uses it, but never one that expired unused or a
//! replaced last-resort package.

use openmls::ciphersuite::hash_ref::make_key_package_ref;
use openmls::prelude::{KeyPackageBundle, KeyPackageRef, OpenMlsProvider};
use openmls_traits::storage::StorageProvider;
use tls_codec::{Deserialize as _, Serialize as _, VLBytes};

use super::{CIPHERSUITE, DefaultMlsProvider, MlsError};

/// The storage label of key package bundles in `openmls_memory_storage` (its keys are the label,
/// the JSON of the hash reference and the storage version). Entries under it that don't decode as
/// a bundle are skipped.
const KEY_PACKAGE_LABEL: &[u8] = b"KeyPackage";

/// One of this device's key packages whose private part is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredKeyPackage {
  /// The package's hash reference (RFC 9420 §5.2), the key it is stored and deleted under.
  pub hash_ref: Vec<u8>,
  /// Seconds since the Unix epoch from which the package is valid (its creation, backdated).
  pub not_before_unix_secs: u64,
  /// Seconds since the Unix epoch after which nobody accepts the package any more.
  pub not_after_unix_secs: u64,
  pub is_last_resort: bool,
}

/// Lists the key packages whose private parts the provider's storage holds.
pub fn stored_key_packages(provider: &DefaultMlsProvider) -> Result<Vec<StoredKeyPackage>, MlsError> {
  let values = provider
    .storage()
    .values
    .read()
    .map_err(|_| MlsError::UnexpectedMessage("MLS storage lock poisoned"))?;
  let mut packages = Vec::new();
  for (key, value) in values.iter() {
    if !key.starts_with(KEY_PACKAGE_LABEL) {
      continue;
    }
    // Other labels that start alike (none today) hold other types and don't decode.
    let Ok(bundle) = serde_json::from_slice::<KeyPackageBundle>(value) else {
      continue;
    };
    let key_package = bundle.key_package();
    let hash_ref = key_package
      .hash_ref(provider.crypto())
      .map_err(|error| MlsError::openmls("key package reference", error))?;
    packages.push(StoredKeyPackage {
      hash_ref: hash_ref.as_slice().to_vec(),
      not_before_unix_secs: key_package.life_time().not_before(),
      not_after_unix_secs: key_package.life_time().not_after(),
      is_last_resort: key_package.last_resort(),
    });
  }
  Ok(packages)
}

/// Deletes the private part of one of this device's key packages: nobody can add the device with
/// that package afterwards (a welcome for it can't be opened).
pub fn delete_key_package(provider: &DefaultMlsProvider, hash_ref: &[u8]) -> Result<(), MlsError> {
  // openmls builds a reference from bytes only through its TLS encoding (a length-prefixed value).
  let encoded = VLBytes::new(hash_ref.to_vec()).tls_serialize_detached()?;
  let hash_ref = KeyPackageRef::tls_deserialize_exact(encoded)?;
  provider
    .storage()
    .delete_key_package(&hash_ref)
    .map_err(|error| MlsError::openmls("key package deletion", error))
}

/// The hash reference of an encoded key package (as [`super::create_key_package`] returns it), to
/// recognise it among the [`stored_key_packages`] later. RFC 9420 §5.2: a hash of the encoding, so
/// neither the package nor its lifetime is checked here.
pub fn key_package_ref(bytes: &[u8]) -> Result<Vec<u8>, MlsError> {
  let crypto = openmls_rust_crypto::RustCrypto::default();
  let hash_ref = make_key_package_ref(bytes, CIPHERSUITE, &crypto)
    .map_err(|error| MlsError::openmls("key package reference", error))?;
  Ok(hash_ref.as_slice().to_vec())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::membership::tests::TestUser;
  use crate::mls::{
    KEY_PACKAGE_BACKDATE_SECS, KEY_PACKAGE_LIFETIME_SECS, create_key_package, create_last_resort_key_package,
  };

  const NOW_UNIX_SECS: u64 = 1_800_000_000;

  #[test]
  fn lists_and_deletes_own_key_packages() {
    let provider = DefaultMlsProvider::default();
    let user = TestUser::new(1);
    let now = NOW_UNIX_SECS;
    let first = create_key_package(&provider, &user.device, &user.certificate, now).unwrap();
    let last_resort =
      create_last_resort_key_package(&provider, &user.device, &user.certificate, now).unwrap();
    let first_ref = key_package_ref(&first).unwrap();
    let last_resort_ref = key_package_ref(&last_resort).unwrap();

    let mut stored = stored_key_packages(&provider).unwrap();
    stored.sort_by_key(|package| package.is_last_resort);
    assert_eq!(
      stored,
      vec![
        StoredKeyPackage {
          hash_ref: first_ref.clone(),
          not_before_unix_secs: now - KEY_PACKAGE_BACKDATE_SECS,
          not_after_unix_secs: now + KEY_PACKAGE_LIFETIME_SECS,
          is_last_resort: false,
        },
        StoredKeyPackage {
          hash_ref: last_resort_ref.clone(),
          not_before_unix_secs: now - KEY_PACKAGE_BACKDATE_SECS,
          not_after_unix_secs: now + KEY_PACKAGE_LIFETIME_SECS,
          is_last_resort: true,
        },
      ]
    );

    delete_key_package(&provider, &first_ref).unwrap();
    let stored = stored_key_packages(&provider).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].hash_ref, last_resort_ref);
  }

  #[test]
  fn deleting_an_unknown_reference_is_harmless() {
    let provider = DefaultMlsProvider::default();
    delete_key_package(&provider, &[7; 32]).unwrap();
    assert!(stored_key_packages(&provider).unwrap().is_empty());
  }
}
