//! Signed membership changes (e2ee.md §4.6, admin.md §2.2–§2.3).
//!
//! The server is untrusted, so "the server says Paul may add Anna" is never enough. Every add or
//! remove is a [`MembershipChange`] signed by the acting user's device key; the device key is
//! vouched for by the actor's pinned identity key, and the actor's right to make the change is
//! decided by a [`MembershipAuthority`] (backed by the signed admin roster and role assignments).
//! Member clients only commit, and only accept commits for, changes that pass
//! [`SignedMembershipChange::verify`].

use std::fmt;

use rand_core::CryptoRngCore;
use tls_codec::{DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize};

use crate::domain;
use crate::identity::{
  DeviceKeyPair, Ed25519Signature, IdentityError, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice,
};
use crate::ids::{ConversationId, DeviceId, UserId};

const MEMBERSHIP_CHANGE_VERSION: u8 = 1;

/// Pinned identity keys of other users (trust on first use or OIDC-attested, e2ee.md §3).
/// Implemented by the client's contact store.
pub trait IdentityDirectory {
  fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey>;
}

/// Decides whether a user may make a membership change in a conversation (admin.md §2.3).
///
/// The implementation must base its answer on signed data only – the admin roster rooted in the
/// server owner's identity key, signed space/channel role assignments and the signed workspace
/// policy – never on the server's word. That roster is not built yet; this trait is the seam.
pub trait MembershipAuthority {
  fn is_authorized(&self, actor: UserId, conversation_id: ConversationId, action: &MembershipAction) -> bool;
}

/// Random identifier of a change, so a member client can refuse to apply the same signed change
/// twice (e.g. the server replaying an old "add Anna" after Anna was removed again).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChangeId([u8; 16]);

/// What should happen to the conversation's membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum MembershipAction {
  /// Add all devices of a user; `history` tells the committing client which past content keys to
  /// share (e2ee.md §4.3).
  #[tls_codec(discriminant = 1)]
  AddUser { user_id: UserId, history: HistoryShare },
  /// Remove all devices of a user.
  #[tls_codec(discriminant = 2)]
  RemoveUser { user_id: UserId },
  /// Remove one device, e.g. a lost laptop.
  #[tls_codec(discriminant = 3)]
  RemoveDevice { user_id: UserId, device_id: DeviceId },
}

/// The "Share history" choice of the add dialog (e2ee.md §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
pub enum HistoryShare {
  Full = 1,
  FromJoin = 2,
}

/// An unsigned membership change.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct MembershipChange {
  version: u8,
  change_id: ChangeId,
  conversation_id: ConversationId,
  action: MembershipAction,
}

/// A membership change with the actor's device certificate and device signature. This is the
/// "signed admin action" of admin.md §2.2 and travels in the authenticated data of the MLS commit.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedMembershipChange {
  change: MembershipChange,
  signer: SignedDeviceCertificate,
  signature: Ed25519Signature,
}

/// A change whose signature, signer identity and authorization were checked. Only
/// [`SignedMembershipChange::verify`] creates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMembershipChange {
  change: MembershipChange,
  actor: VerifiedDevice,
}

#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
  #[error("unsupported membership change version {0}")]
  UnsupportedVersion(u8),
  #[error("no pinned identity key for signer {0}")]
  UnknownSigner(UserId),
  #[error("signer's device certificate is invalid: {0}")]
  InvalidSignerCertificate(IdentityError),
  #[error("membership change signature is invalid")]
  InvalidSignature,
  #[error("signing device does not match the certificate passed along")]
  CertificateMismatch,
  #[error("user {actor} may not perform {action:?} in conversation {conversation_id}")]
  Unauthorized {
    actor: UserId,
    conversation_id: ConversationId,
    action: MembershipAction,
  },
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl ChangeId {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    let mut bytes = [0u8; 16];
    rng.fill_bytes(&mut bytes);
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 16] {
    &self.0
  }
}

impl fmt::Debug for ChangeId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("ChangeId(")?;
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    f.write_str(")")
  }
}

impl MembershipAction {
  /// The user whose membership changes.
  pub fn target_user(&self) -> UserId {
    match self {
      Self::AddUser { user_id, .. } | Self::RemoveUser { user_id } | Self::RemoveDevice { user_id, .. } => {
        *user_id
      },
    }
  }
}

impl MembershipChange {
  pub fn new(
    conversation_id: ConversationId,
    action: MembershipAction,
    rng: &mut impl CryptoRngCore,
  ) -> Self {
    Self {
      version: MEMBERSHIP_CHANGE_VERSION,
      change_id: ChangeId::generate(rng),
      conversation_id,
      action,
    }
  }

  pub fn change_id(&self) -> ChangeId {
    self.change_id
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  pub fn action(&self) -> &MembershipAction {
    &self.action
  }

  /// Signs the change with the actor's device key. `certificate` must be that device's certificate.
  pub fn sign(
    self,
    device: &DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  ) -> Result<SignedMembershipChange, MembershipError> {
    let claimed = certificate.unverified();
    let own = device.public_keys();
    if claimed.user_id != device.user_id()
      || claimed.device_id != own.device_id
      || claimed.signing_key != own.signing_key
    {
      return Err(MembershipError::CertificateMismatch);
    }
    let signature = device.sign_labeled(domain::MEMBERSHIP_CHANGE, &self.tls_serialize_detached()?);
    Ok(SignedMembershipChange {
      change: self,
      signer: certificate,
      signature,
    })
  }
}

impl SignedMembershipChange {
  pub fn to_bytes(&self) -> Result<Vec<u8>, MembershipError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MembershipError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  /// The claimed, unverified change. Don't act on it before [`Self::verify`].
  pub fn unverified(&self) -> &MembershipChange {
    &self.change
  }

  /// Checks the signer's certificate against its pinned identity key, the device signature over
  /// the change and the signer's authorization for the action.
  pub fn verify(
    &self,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<VerifiedMembershipChange, MembershipError> {
    if self.change.version != MEMBERSHIP_CHANGE_VERSION {
      return Err(MembershipError::UnsupportedVersion(self.change.version));
    }
    let signer_user = self.signer.unverified().user_id;
    let pinned = directory
      .pinned_identity(signer_user)
      .ok_or(MembershipError::UnknownSigner(signer_user))?;
    let actor = self
      .signer
      .verify(&pinned)
      .map_err(MembershipError::InvalidSignerCertificate)?;
    actor
      .signing_key()
      .verify_labeled(
        domain::MEMBERSHIP_CHANGE,
        &self.change.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(|_| MembershipError::InvalidSignature)?;
    if !authority.is_authorized(actor.user_id(), self.change.conversation_id, &self.change.action) {
      return Err(MembershipError::Unauthorized {
        actor: actor.user_id(),
        conversation_id: self.change.conversation_id,
        action: self.change.action,
      });
    }
    Ok(VerifiedMembershipChange {
      change: self.change.clone(),
      actor,
    })
  }
}

impl VerifiedMembershipChange {
  pub fn change(&self) -> &MembershipChange {
    &self.change
  }

  pub fn actor(&self) -> &VerifiedDevice {
    &self.actor
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use std::collections::{HashMap, HashSet};

  use rand_core::OsRng;

  use super::*;
  use crate::identity::IdentityKeyPair;

  const CONVERSATION: ConversationId = ConversationId::new(500);
  const ANNA: UserId = UserId::new(2);

  #[test]
  fn signed_change_round_trips_and_verifies() {
    let admin = TestUser::new(1);
    let directory = TestDirectory::with(&[&admin]);
    let authority = TestAuthority::allowing(&[admin.user_id]);
    let change = add_anna();
    let signed = change
      .clone()
      .sign(&admin.device, admin.certificate.clone())
      .unwrap();

    let decoded = SignedMembershipChange::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    let verified = decoded.verify(&directory, &authority).unwrap();

    assert_eq!(verified.change(), &change);
    assert_eq!(verified.actor().user_id(), admin.user_id);
    assert_eq!(verified.change().action().target_user(), ANNA);
  }

  #[test]
  fn any_cross_signed_device_of_the_actor_may_sign() {
    let admin = TestUser::new(1);
    let admin_phone = admin.new_device(101);
    let directory = TestDirectory::with(&[&admin]);
    let signed = add_anna()
      .sign(&admin_phone.device, admin_phone.certificate.clone())
      .unwrap();
    let verified = signed
      .verify(&directory, &TestAuthority::allowing(&[admin.user_id]))
      .unwrap();
    assert_eq!(verified.actor().device_id(), DeviceId::new(101));
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_unauthorized_actor() {
      let member = TestUser::new(3);
      let directory = TestDirectory::with(&[&member]);
      let signed = add_anna()
        .sign(&member.device, member.certificate.clone())
        .unwrap();

      assert!(matches!(
        signed.verify(&directory, &TestAuthority::allowing(&[])),
        Err(MembershipError::Unauthorized { .. })
      ));
    }

    #[test]
    fn rejects_unsigned_change() {
      // A change with an all-zero "signature", as a server would have to forge it.
      let admin = TestUser::new(1);
      let mut signed = add_anna().sign(&admin.device, admin.certificate.clone()).unwrap();
      signed.signature = Ed25519Signature::from_bytes([0; 64]);

      assert!(matches!(
        signed.verify(
          &TestDirectory::with(&[&admin]),
          &TestAuthority::allowing(&[admin.user_id])
        ),
        Err(MembershipError::InvalidSignature)
      ));
    }

    #[test]
    fn rejects_change_signed_by_other_device() {
      // The server swaps in the admin's certificate but signs with a key of its own.
      let admin = TestUser::new(1);
      let server = TestUser::new(1);
      let forged = add_anna()
        .sign(&server.device, server.certificate.clone())
        .unwrap();
      let swapped = SignedMembershipChange {
        signer: admin.certificate.clone(),
        ..forged
      };

      assert!(matches!(
        swapped.verify(
          &TestDirectory::with(&[&admin]),
          &TestAuthority::allowing(&[admin.user_id])
        ),
        Err(MembershipError::InvalidSignature)
      ));
    }

    #[test]
    fn rejects_signer_certified_by_unpinned_identity() {
      // The server invents a device for the admin, certified by an identity key it controls.
      let admin = TestUser::new(1);
      let impostor = TestUser::new(1);
      let signed = add_anna()
        .sign(&impostor.device, impostor.certificate.clone())
        .unwrap();

      assert!(matches!(
        signed.verify(
          &TestDirectory::with(&[&admin]),
          &TestAuthority::allowing(&[admin.user_id])
        ),
        Err(MembershipError::InvalidSignerCertificate(
          IdentityError::IdentityKeyMismatch
        ))
      ));
      assert!(matches!(
        signed.verify(
          &TestDirectory::with(&[]),
          &TestAuthority::allowing(&[admin.user_id])
        ),
        Err(MembershipError::UnknownSigner(_))
      ));
    }

    #[test]
    fn rejects_tampered_action_or_conversation() {
      let admin = TestUser::new(1);
      let directory = TestDirectory::with(&[&admin]);
      let authority = TestAuthority::allowing(&[admin.user_id]);
      let signed = add_anna().sign(&admin.device, admin.certificate.clone()).unwrap();

      let mut other_user = signed.clone();
      other_user.change.action = MembershipAction::AddUser {
        user_id: UserId::new(66),
        history: HistoryShare::Full,
      };
      let mut other_history = signed.clone();
      other_history.change.action = MembershipAction::AddUser {
        user_id: ANNA,
        history: HistoryShare::Full,
      };
      let mut other_conversation = signed.clone();
      other_conversation.change.conversation_id = ConversationId::new(501);

      for tampered in [other_user, other_history, other_conversation] {
        assert!(matches!(
          tampered.verify(&directory, &authority),
          Err(MembershipError::InvalidSignature)
        ));
      }
    }

    #[test]
    fn rejects_signing_with_foreign_certificate() {
      let admin = TestUser::new(1);
      let other = TestUser::new(4);
      assert!(matches!(
        add_anna().sign(&admin.device, other.certificate.clone()),
        Err(MembershipError::CertificateMismatch)
      ));
    }

    #[test]
    fn rejects_unknown_version_and_malformed_bytes() {
      let admin = TestUser::new(1);
      let mut signed = add_anna().sign(&admin.device, admin.certificate.clone()).unwrap();
      signed.change.version = 7;
      assert!(matches!(
        signed.verify(
          &TestDirectory::with(&[&admin]),
          &TestAuthority::allowing(&[admin.user_id])
        ),
        Err(MembershipError::UnsupportedVersion(7))
      ));
      assert!(SignedMembershipChange::from_bytes(&[]).is_err());
      let mut bytes = add_anna()
        .sign(&admin.device, admin.certificate.clone())
        .unwrap()
        .to_bytes()
        .unwrap();
      bytes[1 + 16 + 8] = 9; // unknown action discriminant
      assert!(SignedMembershipChange::from_bytes(&bytes).is_err());
    }
  }

  fn add_anna() -> MembershipChange {
    MembershipChange::new(
      CONVERSATION,
      MembershipAction::AddUser {
        user_id: ANNA,
        history: HistoryShare::FromJoin,
      },
      &mut OsRng,
    )
  }

  /// A user with one device, for tests across modules.
  pub(crate) struct TestUser {
    pub(crate) user_id: UserId,
    pub(crate) identity: IdentityKeyPair,
    pub(crate) device: DeviceKeyPair,
    pub(crate) certificate: SignedDeviceCertificate,
  }

  impl TestUser {
    pub(crate) fn new(user_id: u64) -> Self {
      Self::with_identity(user_id, IdentityKeyPair::generate(&mut OsRng), user_id * 100)
    }

    /// Another device of the same user, certified by the same identity key.
    pub(crate) fn new_device(&self, device_id: u64) -> Self {
      Self::with_identity(
        self.user_id.get(),
        IdentityKeyPair::from_secret(&self.identity.to_secret()),
        device_id,
      )
    }

    fn with_identity(user_id: u64, identity: IdentityKeyPair, device_id: u64) -> Self {
      let user_id = UserId::new(user_id);
      let device = DeviceKeyPair::generate(user_id, DeviceId::new(device_id), &mut OsRng);
      let certificate = identity.certify_device(user_id, &device.public_keys()).unwrap();
      Self {
        user_id,
        identity,
        device,
        certificate,
      }
    }
  }

  #[derive(Default)]
  pub(crate) struct TestDirectory(HashMap<UserId, IdentityPublicKey>);

  impl TestDirectory {
    pub(crate) fn with(users: &[&TestUser]) -> Self {
      Self(
        users
          .iter()
          .map(|user| (user.user_id, user.identity.public_key()))
          .collect(),
      )
    }
  }

  impl IdentityDirectory for TestDirectory {
    fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey> {
      self.0.get(&user_id).copied()
    }
  }

  /// Lets the listed users make any change, plus everyone remove themselves.
  pub(crate) struct TestAuthority(HashSet<UserId>);

  impl TestAuthority {
    pub(crate) fn allowing(users: &[UserId]) -> Self {
      Self(users.iter().copied().collect())
    }
  }

  impl MembershipAuthority for TestAuthority {
    fn is_authorized(
      &self,
      actor: UserId,
      _conversation_id: ConversationId,
      action: &MembershipAction,
    ) -> bool {
      self.0.contains(&actor) || action.target_user() == actor
    }
  }
}
