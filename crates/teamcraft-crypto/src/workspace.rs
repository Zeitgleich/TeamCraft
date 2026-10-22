//! Signatures on the workspace's append-only logs: the admin roster and the policy (admin.md §2.3,
//! e2ee.md §11).
//!
//! An entry is signed by an owner's device key over its log, version and payload; the signature
//! travels together with the device certificate, so a client can check it against the owner's
//! identity key without asking the server. Which identity keys count as owners is the caller's
//! decision (the roster chain, rooted in the first owner pinned on first use).

use sha2::{Digest, Sha256};
use tls_codec::{
  DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize, VLByteSlice,
};

use crate::domain;
use crate::identity::{
  DeviceKeyPair, Ed25519Signature, IdentityError, SignedDeviceCertificate, VerifiedDevice,
};
use crate::membership::IdentityDirectory;

/// Which log an entry belongs to; part of the signed data, so an entry can't move between logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum WorkspaceLog {
  Roster = 1,
  Policy = 2,
}

/// The signature field of a log entry: the signer's device certificate and its signature.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct SignedLogEntry {
  signer: SignedDeviceCertificate,
  signature: Ed25519Signature,
}

#[derive(TlsSerialize, TlsSize)]
struct SigningInput<'a> {
  log: WorkspaceLog,
  version: u64,
  payload: VLByteSlice<'a>,
}

/// Input of [`log_entry_hash`].
#[derive(TlsSerialize, TlsSize)]
struct HashInput<'a> {
  log: WorkspaceLog,
  version: u64,
  payload: VLByteSlice<'a>,
  signature: VLByteSlice<'a>,
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceLogError {
  #[error("signer {0} is not a trusted owner")]
  UntrustedSigner(crate::ids::UserId),
  #[error("signer's device certificate is invalid: {0}")]
  InvalidCertificate(IdentityError),
  #[error("log entry signature is invalid")]
  InvalidSignature,
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

/// Signs an entry; the result goes into the entry's `signature` field.
pub fn sign_log_entry(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  log: WorkspaceLog,
  version: u64,
  payload: &[u8],
) -> Result<Vec<u8>, WorkspaceLogError> {
  let input = SigningInput {
    log,
    version,
    payload: VLByteSlice(payload),
  }
  .tls_serialize_detached()?;
  let signature = device.sign_labeled(domain::WORKSPACE_LOG, &input);
  Ok(
    SignedLogEntry {
      signer: certificate.clone(),
      signature,
    }
    .tls_serialize_detached()?,
  )
}

/// Verifies an entry against the identity keys in `owners` (only owners may sign). Returns the
/// signing device.
pub fn verify_log_entry(
  signature: &[u8],
  log: WorkspaceLog,
  version: u64,
  payload: &[u8],
  owners: &impl IdentityDirectory,
) -> Result<VerifiedDevice, WorkspaceLogError> {
  let entry = SignedLogEntry::tls_deserialize_exact_bytes(signature)?;
  let signer = entry.signer.unverified().user_id;
  let identity = owners
    .pinned_identity(signer)
    .ok_or(WorkspaceLogError::UntrustedSigner(signer))?;
  let device = entry
    .signer
    .verify(&identity)
    .map_err(WorkspaceLogError::InvalidCertificate)?;
  let input = SigningInput {
    log,
    version,
    payload: VLByteSlice(payload),
  }
  .tls_serialize_detached()?;
  device
    .signing_key()
    .verify_labeled(domain::WORKSPACE_LOG, &input, &entry.signature)
    .map_err(|_| WorkspaceLogError::InvalidSignature)?;
  Ok(device)
}

/// The hash of a whole log entry (signature included). The next entry's signed document names it,
/// so the entries form a chain: a server that shows two clients different entries for one version
/// can be caught by comparing heads (security review A6).
pub fn log_entry_hash(
  log: WorkspaceLog,
  version: u64,
  payload: &[u8],
  signature: &[u8],
) -> Result<[u8; 32], WorkspaceLogError> {
  let input = HashInput {
    log,
    version,
    payload: VLByteSlice(payload),
    signature: VLByteSlice(signature),
  }
  .tls_serialize_detached()?;
  Ok(Sha256::digest(domain::labeled(domain::WORKSPACE_LOG_HASH, &input)).into())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::membership::tests::{TestDirectory, TestUser};

  const PAYLOAD: &[u8] = br#"{"version":1}"#;

  #[test]
  fn owner_signature_verifies() {
    let owner = TestUser::new(1);
    let signature = sign_log_entry(
      &owner.device,
      &owner.certificate,
      WorkspaceLog::Policy,
      1,
      PAYLOAD,
    )
    .unwrap();
    let device = verify_log_entry(
      &signature,
      WorkspaceLog::Policy,
      1,
      PAYLOAD,
      &TestDirectory::with(&[&owner]),
    )
    .unwrap();
    assert_eq!(device.user_id(), owner.user_id);
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_non_owner_and_forged_identity() {
      let owner = TestUser::new(1);
      let admin = TestUser::new(2);
      let impostor = TestUser::new(1);
      let owners = TestDirectory::with(&[&owner]);
      let by_admin = sign_log_entry(
        &admin.device,
        &admin.certificate,
        WorkspaceLog::Roster,
        1,
        PAYLOAD,
      )
      .unwrap();
      let by_impostor = sign_log_entry(
        &impostor.device,
        &impostor.certificate,
        WorkspaceLog::Roster,
        1,
        PAYLOAD,
      )
      .unwrap();

      assert!(matches!(
        verify_log_entry(&by_admin, WorkspaceLog::Roster, 1, PAYLOAD, &owners),
        Err(WorkspaceLogError::UntrustedSigner(_))
      ));
      assert!(matches!(
        verify_log_entry(&by_impostor, WorkspaceLog::Roster, 1, PAYLOAD, &owners),
        Err(WorkspaceLogError::InvalidCertificate(_))
      ));
    }

    #[test]
    fn rejects_entry_moved_to_other_version_log_or_payload() {
      let owner = TestUser::new(1);
      let owners = TestDirectory::with(&[&owner]);
      let signature = sign_log_entry(
        &owner.device,
        &owner.certificate,
        WorkspaceLog::Policy,
        3,
        PAYLOAD,
      )
      .unwrap();
      for (log, version, payload) in [
        (WorkspaceLog::Roster, 3, PAYLOAD),
        (WorkspaceLog::Policy, 4, PAYLOAD),
        (WorkspaceLog::Policy, 3, br#"{"version":3}"#.as_slice()),
      ] {
        assert!(matches!(
          verify_log_entry(&signature, log, version, payload, &owners),
          Err(WorkspaceLogError::InvalidSignature)
        ));
      }
      assert!(verify_log_entry(&[1, 2, 3], WorkspaceLog::Policy, 3, PAYLOAD, &owners).is_err());
    }
  }
}
