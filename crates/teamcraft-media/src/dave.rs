//! DAVE (Discord Audio & Video End-to-End Encryption) on top of the `davey` crate.
//!
//! Two roles:
//!
//! - [`DaveParticipant`]: one call member. Holds the MLS group state and encrypts/decrypts frames.
//! - [`DaveExternalSender`]: the party that announces membership changes as MLS external
//!   proposals (in Discord: the voice gateway; in this spike: the SFU). It never learns media keys:
//!   members create the commits, and the external sender only signs Add/Remove proposals.
//!
//! The external sender can propose *any* key package, so members must check proposed participants
//! against a roster they trust (`expected_participants`), see SPIKE.md and e2ee.md.
//!
//! A processed commit or welcome changes nothing about encryption yet: [`DaveParticipant::prepare_transition`]
//! installs the new epoch's decryption keys, and the encryption key switches when the transition is
//! executed (at once without a transition id). The protocol version never drops to 0 here: calls
//! have no unencrypted mode (ARCHITECTURE.md §6.2).

use std::{borrow::Cow, num::NonZeroU16};

use davey::{
  CommitWelcome, DAVE_PROTOCOL_VERSION, DaveSession, ProposalsOperationType, SigningKeyPair,
  errors::{
    CreateKeyPackageError, DecryptError, DecryptorDecryptError, EncryptError, GetVerificationCodeError,
    InitError, ProcessCommitError, ProcessProposalsError, ProcessWelcomeError, SetExternalSenderError,
    TransitionError,
  },
};
use openmls::prelude::{
  BasicCredential, Credential, ExternalProposal, ExternalSender, GroupEpoch, GroupId, KeyPackage,
  KeyPackageIn, LeafNodeIndex, OpenMlsProvider, ProtocolVersion, SenderExtensionIndex, SignatureScheme,
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use teamcraft_proto::{CallId, DaveTransitionId, DeviceId, Id, IdError, Snowflake};
use thiserror::Error;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize, VLBytes};

/// Identity the external sender puts into its MLS credential.
const EXTERNAL_SENDER_IDENTITY: &[u8] = b"teamcraft-sfu";

/// One participant's DAVE session for one call.
pub struct DaveParticipant {
  session: DaveSession,
  /// Public half of this session's fresh P-256 signature key (SEC1), bound to the device by a
  /// call key binding (e2ee.md §8).
  signature_public_key: Vec<u8>,
  stats: DaveFrameStats,
}

/// Frames DAVE couldn't encrypt or decrypt, by cause, since the session started. Numbers only, for
/// call stats and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DaveFrameStats {
  /// Own frames that found no encryption key (the group not secured yet, or between epochs).
  pub encrypt_missing_keys: u64,
  /// Received frames whose key generation or sender had no key here (yet, or any more).
  pub decrypt_missing_keys: u64,
  /// Received frames with a nonce that was already used or too old (a replay, or reordering
  /// beyond what DAVE tracks).
  pub invalid_nonces: u64,
  /// Received frames that failed authentication or were malformed.
  pub decrypt_failures: u64,
}

/// Signs MLS external Add/Remove proposals for a call.
pub struct DaveExternalSender {
  provider: OpenMlsRustCrypto,
  signer: SignatureKeyPair,
  credential: Credential,
}

/// Participant ID as used inside DAVE (MLS basic credential = 8 bytes big endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DaveParticipantId(pub u64);

/// Call ID as used inside DAVE (MLS group ID = 8 bytes big endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DaveCallId(pub u64);

/// MLS epoch of a call's group. Proposals are only valid for the epoch they were created for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DaveEpoch(pub u64);

/// Position of a member in the MLS ratchet tree, needed for Remove proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DaveLeafIndex(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
  Audio,
  Video,
}

/// Codec of a frame; DAVE leaves codec-specific parts (e.g. H.264 NAL headers) unencrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCodec {
  Opus,
  Vp8,
  Vp9,
  H264,
  H265,
  Av1,
}

/// Messages a member produces after processing proposals: the commit goes to the external sender,
/// the welcome (present when members were added) to the new members.
#[derive(Debug)]
pub struct DaveCommit {
  pub commit: Vec<u8>,
  pub maybe_welcome: Option<Vec<u8>>,
}

#[derive(Debug, Error)]
pub enum DaveError {
  #[error("unsupported DAVE protocol version {0}")]
  UnsupportedProtocolVersion(u16),
  #[error("creating the DAVE session failed: {0}")]
  Init(#[from] InitError),
  #[error("invalid external sender package: {0}")]
  ExternalSender(#[from] SetExternalSenderError),
  #[error("creating a key package failed: {0}")]
  KeyPackage(#[from] CreateKeyPackageError),
  #[error("processing proposals failed: {0}")]
  Proposals(#[from] ProcessProposalsError),
  #[error("processing a welcome failed: {0}")]
  Welcome(#[from] ProcessWelcomeError),
  #[error("processing a commit failed: {0}")]
  Commit(#[from] ProcessCommitError),
  #[error("encrypting a frame failed: {0}")]
  Encrypt(#[from] EncryptError),
  #[error("decrypting a frame failed: {0}")]
  Decrypt(#[from] DecryptError),
  #[error("switching keys for a transition failed: {0}")]
  Transition(#[from] TransitionError),
  #[error("computing a verification code failed: {0}")]
  VerificationCode(#[from] GetVerificationCodeError),
  #[error("generating the external sender key failed")]
  SignerKeyGeneration,
  #[error("invalid key package: {0}")]
  InvalidKeyPackage(String),
  #[error("key package belongs to participant {actual:?}, expected {expected:?}")]
  KeyPackageParticipantMismatch {
    expected: DaveParticipantId,
    actual: DaveParticipantId,
  },
  #[error("creating an external proposal failed: {0}")]
  Proposal(String),
  #[error("TLS serialization failed: {0}")]
  Serialization(#[from] tls_codec::Error),
}

impl DaveParticipant {
  pub fn new(participant_id: DaveParticipantId, call_id: DaveCallId) -> Result<Self, DaveError> {
    let version = NonZeroU16::new(DAVE_PROTOCOL_VERSION)
      .ok_or(DaveError::UnsupportedProtocolVersion(DAVE_PROTOCOL_VERSION))?;
    // A fresh key for every call; never reused (e2ee.md §8).
    let key_pair = SigningKeyPair::generate();
    let session = DaveSession::new(version, participant_id.0, call_id.0, Some(&key_pair))?;
    Ok(Self {
      session,
      signature_public_key: key_pair.public.clone(),
      stats: DaveFrameStats::default(),
    })
  }

  /// Installs the external sender announced for this call and creates a pending (single member)
  /// group. Must precede [`Self::create_key_package`].
  pub fn set_external_sender(&mut self, package: &[u8]) -> Result<(), DaveError> {
    self.session.set_external_sender(package)?;
    Ok(())
  }

  /// A fresh key package to hand to the external sender.
  pub fn create_key_package(&mut self) -> Result<Vec<u8>, DaveError> {
    Ok(self.session.create_key_package()?)
  }

  /// Processes Add/Remove proposals from the external sender. Proposed participants outside
  /// `expected_participants` are rejected.
  pub fn process_proposals(
    &mut self,
    proposals: &[u8],
    expected_participants: &[DaveParticipantId],
  ) -> Result<Option<DaveCommit>, DaveError> {
    let expected: Vec<u64> = expected_participants.iter().map(|id| id.0).collect();
    let maybe_commit =
      self
        .session
        .process_proposals(ProposalsOperationType::APPEND, proposals, Some(&expected))?;
    Ok(maybe_commit.map(|CommitWelcome { commit, welcome }| DaveCommit {
      commit,
      maybe_welcome: welcome,
    }))
  }

  /// Joins the group of a welcome. Every member of the welcomed group must be in
  /// `recognized_participants` (the roster this device trusts, never the SFU's word alone), and
  /// the group must be this call's. Follow with [`Self::prepare_transition`].
  pub fn process_welcome(
    &mut self,
    welcome: &[u8],
    recognized_participants: &[DaveParticipantId],
  ) -> Result<(), DaveError> {
    let recognized: Vec<u64> = recognized_participants.iter().map(|id| id.0).collect();
    self.session.process_welcome(welcome, Some(&recognized))?;
    Ok(())
  }

  /// Applies the commit the external sender selected for this epoch (possibly our own). `true`
  /// when the group moved on and [`Self::prepare_transition`] follows; `false` when the commit was
  /// ignored (this device holds no group the commit could apply to yet).
  pub fn process_commit(&mut self, commit: &[u8]) -> Result<bool, DaveError> {
    Ok(self.session.process_commit(commit)?)
  }

  /// Installs the current epoch's decryption keys after a processed commit or welcome. Without a
  /// transition id the encryption key switches too; with one, only on
  /// [`Self::execute_transition`]. Always to [`DAVE_PROTOCOL_VERSION`]: never a downgrade.
  pub fn prepare_transition(
    &mut self,
    maybe_transition_id: Option<DaveTransitionId>,
  ) -> Result<(), DaveError> {
    let transition_id = maybe_transition_id.map_or(0, |id| id.0.get());
    self
      .session
      .prepare_transition(transition_id, DAVE_PROTOCOL_VERSION)?;
    Ok(())
  }

  /// Switches the encryption key to the epoch of a prepared transition; `false` if this
  /// transition wasn't prepared (or ran already).
  pub fn execute_transition(&mut self, transition_id: DaveTransitionId) -> Result<bool, DaveError> {
    Ok(self.session.execute_transition(transition_id.0.get())?)
  }

  /// Whether frames can be encrypted: the group is established and an encryption key is
  /// installed (for a joiner, once its first transition was executed).
  pub fn is_ready(&self) -> bool {
    self.session.can_encrypt()
  }

  pub fn frame_stats(&self) -> DaveFrameStats {
    self.stats
  }

  pub fn maybe_epoch(&self) -> Option<DaveEpoch> {
    self.session.epoch().map(|epoch| DaveEpoch(epoch.as_u64()))
  }

  pub fn maybe_leaf_index(&self) -> Option<DaveLeafIndex> {
    self
      .session
      .own_leaf_index()
      .map(|index| DaveLeafIndex(index.u32()))
  }

  /// Encrypts one encoded frame. Opus silence frames pass through unencrypted (DAVE spec).
  pub fn encrypt_frame<'a>(
    &mut self,
    kind: MediaKind,
    codec: FrameCodec,
    frame: &'a [u8],
  ) -> Result<Cow<'a, [u8]>, DaveError> {
    let result = self.session.encrypt(kind.into(), codec.into(), frame);
    if matches!(result, Err(EncryptError::NotReady)) {
      self.stats.encrypt_missing_keys += 1;
    }
    Ok(result?)
  }

  pub fn decrypt_frame(
    &mut self,
    sender: DaveParticipantId,
    kind: MediaKind,
    frame: &[u8],
  ) -> Result<Vec<u8>, DaveError> {
    let result = self.session.decrypt(sender.0, kind.into(), frame);
    if let Err(error) = &result {
      self.count_decrypt_error(error);
    }
    Ok(result?)
  }

  /// Code both sides compare out of band to verify each other (DAVE "displayable codes").
  pub fn verification_code(&self, other: DaveParticipantId) -> Result<String, DaveError> {
    Ok(self.session.get_verification_code(other.0)?)
  }

  /// This session's MLS signature public key (SEC1 P-256), to be bound to the device.
  pub fn signature_public_key(&self) -> &[u8] {
    &self.signature_public_key
  }

  /// Members the pending commit (from [`Self::process_proposals`]) would add, with their leaf
  /// signature keys; check them before sending the commit.
  pub fn pending_additions(&self) -> Vec<(DaveParticipantId, Vec<u8>)> {
    let Some(commit) = self.session.group().and_then(|group| group.pending_commit()) else {
      return Vec::new();
    };
    commit
      .add_proposals()
      .filter_map(|queued| {
        let leaf = queued.add_proposal().key_package().leaf_node();
        let id = participant_from_credential(leaf.credential()).ok()?;
        Some((id, leaf.signature_key().as_slice().to_vec()))
      })
      .collect()
  }

  /// Current group members with their leaf signature keys.
  pub fn member_signature_keys(&self) -> Vec<(DaveParticipantId, Vec<u8>)> {
    let Some(group) = self.session.group() else {
      return Vec::new();
    };
    group
      .members()
      .filter_map(|member| {
        Some((
          participant_from_credential(&member.credential).ok()?,
          member.signature_key,
        ))
      })
      .collect()
  }

  /// Participants in the current group, including ourselves; empty before joining.
  pub fn participant_ids(&self) -> Vec<DaveParticipantId> {
    self
      .session
      .get_user_ids()
      .unwrap_or_default()
      .into_iter()
      .map(DaveParticipantId)
      .collect()
  }

  /// Group-wide privacy code; changes with every epoch.
  pub fn maybe_privacy_code(&self) -> Option<&str> {
    self.session.voice_privacy_code()
  }

  fn count_decrypt_error(&mut self, error: &DecryptError) {
    let counter = match error {
      DecryptError::NoDecryptorForUser
      | DecryptError::DecryptionFailed(
        DecryptorDecryptError::MissingKeyRatchet | DecryptorDecryptError::MissingCryptor,
      ) => &mut self.stats.decrypt_missing_keys,
      DecryptError::DecryptionFailed(DecryptorDecryptError::InvalidNonce) => &mut self.stats.invalid_nonces,
      DecryptError::DecryptionFailed(
        DecryptorDecryptError::DecryptionFailure | DecryptorDecryptError::UnencryptedWhenPassthroughDisabled,
      ) => &mut self.stats.decrypt_failures,
    };
    *counter += 1;
  }
}

impl std::ops::AddAssign for DaveFrameStats {
  fn add_assign(&mut self, other: Self) {
    self.encrypt_missing_keys += other.encrypt_missing_keys;
    self.decrypt_missing_keys += other.decrypt_missing_keys;
    self.invalid_nonces += other.invalid_nonces;
    self.decrypt_failures += other.decrypt_failures;
  }
}

impl DaveExternalSender {
  pub fn new() -> Result<Self, DaveError> {
    // DAVE v1 ciphersuite: MLS_128_DHKEMP256_AES128GCM_SHA256_P256.
    let signer = SignatureKeyPair::new(SignatureScheme::ECDSA_SECP256R1_SHA256)
      .map_err(|_| DaveError::SignerKeyGeneration)?;
    let credential = BasicCredential::new(EXTERNAL_SENDER_IDENTITY.to_vec()).into();
    Ok(Self {
      provider: OpenMlsRustCrypto::default(),
      signer,
      credential,
    })
  }

  /// Serialized `ExternalSender` that every participant installs with
  /// [`DaveParticipant::set_external_sender`].
  pub fn package(&self) -> Result<Vec<u8>, DaveError> {
    let sender = ExternalSender::new(self.signer.public().into(), self.credential.clone());
    Ok(sender.tls_serialize_detached()?)
  }

  /// Verifies a key package and returns the participant it was created for.
  pub fn key_package_participant(&self, key_package: &[u8]) -> Result<DaveParticipantId, DaveError> {
    let key_package = self.validate_key_package(key_package)?;
    participant_from_credential(key_package.leaf_node().credential())
  }

  /// Builds one proposals message for an epoch: Remove proposals for `removed` members, then Add
  /// proposals for `joiners`. Each key package must belong to the participant it is listed with, so
  /// a client can't join under another identity.
  pub fn propose(
    &self,
    call_id: DaveCallId,
    epoch: DaveEpoch,
    joiners: &[(DaveParticipantId, &[u8])],
    removed: &[DaveLeafIndex],
  ) -> Result<Vec<u8>, DaveError> {
    let mut proposals = Vec::new();
    for leaf_index in removed {
      let message = ExternalProposal::new_remove::<OpenMlsRustCrypto>(
        LeafNodeIndex::new(leaf_index.0),
        group_id(call_id),
        GroupEpoch::from(epoch.0),
        &self.signer,
        SenderExtensionIndex::new(0),
      )
      .map_err(|error| DaveError::Proposal(error.to_string()))?;
      message.tls_serialize(&mut proposals)?;
    }
    for &(expected, key_package) in joiners {
      let key_package = self.validate_key_package(key_package)?;
      let actual = participant_from_credential(key_package.leaf_node().credential())?;
      if actual != expected {
        return Err(DaveError::KeyPackageParticipantMismatch { expected, actual });
      }
      let message = ExternalProposal::new_add::<OpenMlsRustCrypto>(
        key_package,
        group_id(call_id),
        GroupEpoch::from(epoch.0),
        &self.signer,
        SenderExtensionIndex::new(0),
      )
      .map_err(|error| DaveError::Proposal(error.to_string()))?;
      message.tls_serialize(&mut proposals)?;
    }
    Ok(VLBytes::new(proposals).tls_serialize_detached()?)
  }

  fn validate_key_package(&self, key_package: &[u8]) -> Result<KeyPackage, DaveError> {
    KeyPackageIn::tls_deserialize_exact(key_package)?
      .validate(self.provider.crypto(), ProtocolVersion::Mls10)
      .map_err(|error| DaveError::InvalidKeyPackage(error.to_string()))
  }
}

impl DaveParticipantId {
  /// DAVE members are devices; the 8-byte member id is the device's snowflake.
  pub fn from_device(device_id: DeviceId) -> Self {
    Self(snowflake_to_u64(device_id.snowflake()))
  }

  /// Fails for ids above the 63-bit snowflake range (never produced by [`Self::from_device`]).
  pub fn to_device(self) -> Result<DeviceId, IdError> {
    let raw = i64::try_from(self.0).map_err(|_| IdError::Malformed(self.0.to_string()))?;
    Ok(DeviceId::from_snowflake(Snowflake::new(raw)?))
  }
}

impl DaveCallId {
  /// The DAVE group id of a call is the call's snowflake.
  pub fn from_call(call_id: CallId) -> Self {
    Self(snowflake_to_u64(call_id.snowflake()))
  }
}

impl From<MediaKind> for davey::MediaType {
  fn from(kind: MediaKind) -> Self {
    match kind {
      MediaKind::Audio => Self::AUDIO,
      MediaKind::Video => Self::VIDEO,
    }
  }
}

impl From<FrameCodec> for davey::Codec {
  fn from(codec: FrameCodec) -> Self {
    match codec {
      FrameCodec::Opus => Self::OPUS,
      FrameCodec::Vp8 => Self::VP8,
      FrameCodec::Vp9 => Self::VP9,
      FrameCodec::H264 => Self::H264,
      FrameCodec::H265 => Self::H265,
      FrameCodec::Av1 => Self::AV1,
    }
  }
}

fn snowflake_to_u64(snowflake: Snowflake) -> u64 {
  // Invariant: snowflakes are non-negative, so the conversion never fails.
  u64::try_from(snowflake.get()).unwrap_or_default()
}

fn group_id(call_id: DaveCallId) -> GroupId {
  GroupId::from_slice(&call_id.0.to_be_bytes())
}

fn participant_from_credential(credential: &Credential) -> Result<DaveParticipantId, DaveError> {
  let bytes: [u8; 8] = credential
    .serialized_content()
    .try_into()
    .map_err(|_| DaveError::InvalidKeyPackage("credential is not an 8-byte participant id".to_owned()))?;
  Ok(DaveParticipantId(u64::from_be_bytes(bytes)))
}

#[cfg(test)]
mod tests {
  use super::*;

  const CALL: DaveCallId = DaveCallId(0x7E57);
  const ALICE: DaveParticipantId = DaveParticipantId(1);
  const BOB: DaveParticipantId = DaveParticipantId(2);
  const CAROL: DaveParticipantId = DaveParticipantId(3);
  const OPUS_FRAME: &[u8] = &[0x78, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09];

  mod group {
    use super::*;

    #[test]
    fn two_participants_exchange_frames() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);

      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );

      assert!(alice.is_ready() && bob.is_ready());
      assert_eq!(alice.maybe_epoch(), bob.maybe_epoch());
      assert_round_trip(&mut alice, ALICE, &mut bob);
      assert_round_trip(&mut bob, BOB, &mut alice);
      assert_eq!(
        alice.verification_code(BOB).unwrap(),
        bob.verification_code(ALICE).unwrap()
      );
      assert_eq!(alice.maybe_privacy_code(), bob.maybe_privacy_code());
    }

    #[test]
    fn member_keys_are_the_fresh_session_keys() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let key_package = bob.create_key_package().unwrap();
      let epoch = alice.maybe_epoch().unwrap();
      let proposals = gateway.propose(CALL, epoch, &[(BOB, &key_package)], &[]).unwrap();
      let commit = alice
        .process_proposals(&proposals, &[ALICE, BOB])
        .unwrap()
        .unwrap();
      assert_eq!(
        alice.pending_additions(),
        vec![(BOB, bob.signature_public_key().to_vec())]
      );
      apply_commit(&mut alice, &commit.commit);
      apply_welcome(&mut bob, &commit.maybe_welcome.unwrap(), &[ALICE, BOB]);
      let mut keys = bob.member_signature_keys();
      keys.sort();
      let mut expected = vec![
        (ALICE, alice.signature_public_key().to_vec()),
        (BOB, bob.signature_public_key().to_vec()),
      ];
      expected.sort();
      assert_eq!(keys, expected);
      // P-256 SEC1 uncompressed, and different per call.
      assert_eq!(alice.signature_public_key().len(), 65);
      assert_ne!(
        alice.signature_public_key(),
        joined_participant(&gateway, ALICE).signature_public_key()
      );
    }

    /// An AV1 temporal unit as WebCodecs' encoder gives it comes out of DAVE without its
    /// temporal delimiter and the last OBU's size, and whole again after `av1_temporal_unit`
    /// (#565).
    #[test]
    fn av1_frames_come_back_whole() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      // A temporal delimiter, a sequence header and a frame OBU of 300 bytes.
      let mut unit = vec![0x12, 0x00, 0x0a, 0x04, 0, 1, 2, 3, 0x32, 0xac, 0x02];
      unit.extend((0..300u32).map(|value| (value % 251) as u8));
      let encrypted = alice
        .encrypt_frame(MediaKind::Video, FrameCodec::Av1, &unit)
        .unwrap()
        .into_owned();
      let decrypted = bob.decrypt_frame(ALICE, MediaKind::Video, &encrypted).unwrap();
      assert_ne!(decrypted, unit, "DAVE sends the unit as AV1's RTP format does");
      assert_eq!(crate::video::av1_temporal_unit(&decrypted), unit);
    }

    #[test]
    fn third_participant_joins_existing_group() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let mut carol = joined_participant(&gateway, CAROL);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );

      add_members(
        &gateway,
        &mut alice,
        &mut [&mut bob],
        &mut [(CAROL, &mut carol)],
        &[ALICE, BOB, CAROL],
      );

      assert_eq!(alice.maybe_epoch(), Some(DaveEpoch(2)));
      assert_eq!(carol.participant_ids().len(), 3);
      assert_eq!(carol.maybe_epoch(), bob.maybe_epoch());
      assert_round_trip(&mut carol, CAROL, &mut alice);
      assert_round_trip(&mut alice, ALICE, &mut carol);
      assert_round_trip(&mut bob, BOB, &mut carol);
    }

    #[test]
    fn a_joiner_cannot_decrypt_frames_from_before_it_joined() {
      // A meeting guest's device is added like Carol: its commit starts a new epoch, and the
      // frames sent before were encrypted under keys the welcome never gives it (e2ee.md §4.6).
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let mut carol = joined_participant(&gateway, CAROL);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let before = alice
        .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
        .unwrap()
        .into_owned();

      add_members(
        &gateway,
        &mut alice,
        &mut [&mut bob],
        &mut [(CAROL, &mut carol)],
        &[ALICE, BOB, CAROL],
      );

      assert!(carol.decrypt_frame(ALICE, MediaKind::Audio, &before).is_err());
      assert_eq!(
        bob.decrypt_frame(ALICE, MediaKind::Audio, &before).unwrap(),
        OPUS_FRAME
      );
      assert_round_trip(&mut alice, ALICE, &mut carol);
    }

    #[test]
    fn removed_participant_cannot_decrypt_new_epoch() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let mut carol = joined_participant(&gateway, CAROL);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob), (CAROL, &mut carol)],
        &[ALICE, BOB, CAROL],
      );
      let carol_leaf = carol.maybe_leaf_index().unwrap();

      let epoch = alice.maybe_epoch().unwrap();
      let proposals = gateway.propose(CALL, epoch, &[], &[carol_leaf]).unwrap();
      let commit = alice
        .process_proposals(&proposals, &[ALICE, BOB])
        .unwrap()
        .unwrap();
      assert!(commit.maybe_welcome.is_none());
      bob.process_proposals(&proposals, &[ALICE, BOB]).unwrap();
      apply_commit(&mut alice, &commit.commit);
      apply_commit(&mut bob, &commit.commit);

      assert_round_trip(&mut alice, ALICE, &mut bob);
      let encrypted = alice
        .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
        .unwrap()
        .into_owned();
      assert!(carol.decrypt_frame(ALICE, MediaKind::Audio, &encrypted).is_err());
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn unexpected_participant_is_rejected() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let key_package = bob.create_key_package().unwrap();
      let proposals = gateway
        .propose(CALL, DaveEpoch(0), &[(BOB, &key_package)], &[])
        .unwrap();

      let result = alice.process_proposals(&proposals, &[ALICE]);

      assert!(matches!(
        result,
        Err(DaveError::Proposals(ProcessProposalsError::UnexpectedUser(2)))
      ));
    }

    #[test]
    fn key_package_of_other_participant_is_rejected() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut bob = joined_participant(&gateway, BOB);
      let key_package = bob.create_key_package().unwrap();

      let result = gateway.propose(CALL, DaveEpoch(0), &[(CAROL, &key_package)], &[]);

      assert!(matches!(
        result,
        Err(DaveError::KeyPackageParticipantMismatch {
          expected: CAROL,
          actual: BOB
        })
      ));
    }

    #[test]
    fn garbage_key_package() {
      let gateway = DaveExternalSender::new().unwrap();
      assert!(gateway.key_package_participant(&[]).is_err());
      assert!(gateway.key_package_participant(&[0xFF; 64]).is_err());
    }

    #[test]
    fn tampered_frame_fails_to_decrypt() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let mut encrypted = alice
        .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
        .unwrap()
        .into_owned();
      encrypted[0] ^= 0x01;

      assert!(bob.decrypt_frame(ALICE, MediaKind::Audio, &encrypted).is_err());
    }

    #[test]
    fn encrypt_before_group_is_established() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      assert!(matches!(
        alice.encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME),
        Err(DaveError::Encrypt(EncryptError::NotReady))
      ));
    }
  }

  mod welcome_checks {
    use super::*;

    /// A welcome naming a member outside the roster this device trusts is refused, whatever the
    /// SFU says (e2ee.md §8).
    #[test]
    fn welcome_with_an_unknown_member_is_rejected() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let mut carol = joined_participant(&gateway, CAROL);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let welcome = propose_carol(&gateway, &mut alice, &mut bob, &mut carol);

      let result = carol.process_welcome(&welcome, &[ALICE, CAROL]);

      assert!(matches!(
        result,
        Err(DaveError::Welcome(ProcessWelcomeError::UnrecognizedUser(2)))
      ));
      assert!(!carol.is_ready());
    }

    /// A welcome into another call's group is refused, though it is for this device's key package
    /// signed by the same external sender.
    #[test]
    fn welcome_into_another_call_is_rejected() {
      let gateway = DaveExternalSender::new().unwrap();
      let other_call = DaveCallId(0xBAD);
      let mut alice = DaveParticipant::new(ALICE, other_call).unwrap();
      alice.set_external_sender(&gateway.package().unwrap()).unwrap();
      let mut bob = joined_participant(&gateway, BOB);
      let key_package = bob.create_key_package().unwrap();
      let proposals = gateway
        .propose(other_call, DaveEpoch(0), &[(BOB, &key_package)], &[])
        .unwrap();
      let commit = alice
        .process_proposals(&proposals, &[ALICE, BOB])
        .unwrap()
        .unwrap();

      let result = bob.process_welcome(&commit.maybe_welcome.unwrap(), &[ALICE, BOB]);

      assert!(matches!(
        result,
        Err(DaveError::Welcome(ProcessWelcomeError::UnexpectedGroupId))
      ));
    }

    /// A second leaf for a participant already in the group (another key package of the same id)
    /// is refused before anything is committed.
    #[test]
    fn duplicate_participant_is_rejected() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let mut second_bob = joined_participant(&gateway, BOB);
      let key_package = second_bob.create_key_package().unwrap();
      let proposals = gateway
        .propose(CALL, alice.maybe_epoch().unwrap(), &[(BOB, &key_package)], &[])
        .unwrap();

      let result = alice.process_proposals(&proposals, &[ALICE, BOB]);

      assert!(matches!(
        result,
        Err(DaveError::Proposals(ProcessProposalsError::InvalidRoster(_)))
      ));
    }
  }

  mod transitions {
    use super::*;

    /// Members switch to the new epoch's key only when the transition is executed; a joiner can
    /// decrypt everyone from then on, and can't encrypt before (#1541).
    #[test]
    fn encryption_switches_on_execute() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      let mut carol = joined_participant(&gateway, CAROL);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let welcome = propose_carol(&gateway, &mut alice, &mut bob, &mut carol);
      carol.process_welcome(&welcome, &[ALICE, BOB, CAROL]).unwrap();
      for participant in [&mut alice, &mut bob, &mut carol] {
        participant.prepare_transition(Some(transition(4))).unwrap();
      }
      assert!(
        alice.is_ready() && !carol.is_ready(),
        "a joiner waits for the execution"
      );

      let before = alice
        .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
        .unwrap()
        .into_owned();
      assert!(carol.decrypt_frame(ALICE, MediaKind::Audio, &before).is_err());
      assert_eq!(
        bob.decrypt_frame(ALICE, MediaKind::Audio, &before).unwrap(),
        OPUS_FRAME
      );

      for participant in [&mut alice, &mut bob, &mut carol] {
        assert!(participant.execute_transition(transition(4)).unwrap());
      }
      assert!(
        !alice.execute_transition(transition(4)).unwrap(),
        "a transition runs once"
      );
      assert_round_trip(&mut alice, ALICE, &mut carol);
      assert_round_trip(&mut carol, CAROL, &mut bob);
      assert_round_trip(&mut bob, BOB, &mut alice);
      assert_eq!(alice.maybe_privacy_code(), carol.maybe_privacy_code());
    }

    /// Re-installing the current epoch's key (a repeated prepare) must not restart the nonces:
    /// the same frame under the same key and nonce would encrypt to the same bytes, and AES-GCM
    /// with a repeated nonce leaks the key stream (davey 0.2.0 fix, #1541).
    #[test]
    fn preparing_the_current_epoch_again_never_repeats_a_nonce() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let mut sent = Vec::new();
      for _ in 0..3 {
        sent.push(
          alice
            .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
            .unwrap()
            .into_owned(),
        );
        alice.prepare_transition(None).unwrap();
        alice.prepare_transition(Some(transition(9))).unwrap();
        alice.execute_transition(transition(9)).unwrap();
      }

      assert_ne!(sent[0], sent[1]);
      assert_ne!(sent[1], sent[2]);
      assert_ne!(sent[0], sent[2]);
      for frame in &sent {
        assert_eq!(
          bob.decrypt_frame(ALICE, MediaKind::Audio, frame).unwrap(),
          OPUS_FRAME
        );
      }
      assert_eq!(bob.frame_stats(), DaveFrameStats::default());
    }

    /// Replayed frames and frames without a key are counted by cause.
    #[test]
    fn failed_frames_are_counted() {
      let gateway = DaveExternalSender::new().unwrap();
      let mut alice = joined_participant(&gateway, ALICE);
      let mut bob = joined_participant(&gateway, BOB);
      assert!(
        alice
          .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
          .is_err()
      );
      add_members(
        &gateway,
        &mut alice,
        &mut [],
        &mut [(BOB, &mut bob)],
        &[ALICE, BOB],
      );
      let frame = alice
        .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
        .unwrap()
        .into_owned();
      bob.decrypt_frame(ALICE, MediaKind::Audio, &frame).unwrap();

      assert!(bob.decrypt_frame(ALICE, MediaKind::Audio, &frame).is_err());
      assert!(bob.decrypt_frame(CAROL, MediaKind::Audio, &frame).is_err());

      assert_eq!(alice.frame_stats().encrypt_missing_keys, 1);
      assert_eq!(
        bob.frame_stats(),
        DaveFrameStats {
          invalid_nonces: 1,
          decrypt_missing_keys: 1,
          ..DaveFrameStats::default()
        }
      );
    }
  }

  fn joined_participant(gateway: &DaveExternalSender, id: DaveParticipantId) -> DaveParticipant {
    let mut participant = DaveParticipant::new(id, CALL).unwrap();
    participant
      .set_external_sender(&gateway.package().unwrap())
      .unwrap();
    participant
  }

  /// Runs one membership change: proposals to the committer and existing members, the
  /// committer's commit to everyone, the welcome to joiners.
  fn add_members(
    gateway: &DaveExternalSender,
    committer: &mut DaveParticipant,
    members: &mut [&mut DaveParticipant],
    joiners: &mut [(DaveParticipantId, &mut DaveParticipant)],
    roster: &[DaveParticipantId],
  ) {
    let key_packages: Vec<(DaveParticipantId, Vec<u8>)> = joiners
      .iter_mut()
      .map(|(id, joiner)| (*id, joiner.create_key_package().unwrap()))
      .collect();
    let borrowed: Vec<(DaveParticipantId, &[u8])> = key_packages
      .iter()
      .map(|(id, key_package)| (*id, key_package.as_slice()))
      .collect();
    let epoch = committer.maybe_epoch().unwrap();
    let proposals = gateway.propose(CALL, epoch, &borrowed, &[]).unwrap();

    let commit = committer.process_proposals(&proposals, roster).unwrap().unwrap();
    for member in members.iter_mut() {
      member.process_proposals(&proposals, roster).unwrap();
    }
    apply_commit(committer, &commit.commit);
    for member in members.iter_mut() {
      apply_commit(member, &commit.commit);
    }
    let welcome = commit.maybe_welcome.unwrap();
    for (_, joiner) in joiners.iter_mut() {
      apply_welcome(joiner, &welcome, roster);
    }
  }

  /// A commit switched to at once (no transition id), as from an SFU without transitions.
  fn apply_commit(participant: &mut DaveParticipant, commit: &[u8]) {
    assert!(participant.process_commit(commit).unwrap());
    participant.prepare_transition(None).unwrap();
  }

  fn apply_welcome(participant: &mut DaveParticipant, welcome: &[u8], roster: &[DaveParticipantId]) {
    participant.process_welcome(welcome, roster).unwrap();
    participant.prepare_transition(None).unwrap();
  }

  fn transition(id: u16) -> DaveTransitionId {
    DaveTransitionId(std::num::NonZeroU16::new(id).unwrap())
  }

  /// Alice and Bob in a group, Carol proposed: returns Carol's welcome after Alice and Bob
  /// processed the commit (nothing prepared yet).
  fn propose_carol(
    gateway: &DaveExternalSender,
    alice: &mut DaveParticipant,
    bob: &mut DaveParticipant,
    carol: &mut DaveParticipant,
  ) -> Vec<u8> {
    let key_package = carol.create_key_package().unwrap();
    let epoch = alice.maybe_epoch().unwrap();
    let proposals = gateway
      .propose(CALL, epoch, &[(CAROL, &key_package)], &[])
      .unwrap();
    let roster = [ALICE, BOB, CAROL];
    let commit = alice.process_proposals(&proposals, &roster).unwrap().unwrap();
    bob.process_proposals(&proposals, &roster).unwrap();
    assert!(alice.process_commit(&commit.commit).unwrap());
    assert!(bob.process_commit(&commit.commit).unwrap());
    commit.maybe_welcome.unwrap()
  }

  fn assert_round_trip(
    sender: &mut DaveParticipant,
    sender_id: DaveParticipantId,
    receiver: &mut DaveParticipant,
  ) {
    let encrypted = sender
      .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, OPUS_FRAME)
      .unwrap()
      .into_owned();
    assert_ne!(encrypted.as_slice(), OPUS_FRAME);
    let decrypted = receiver
      .decrypt_frame(sender_id, MediaKind::Audio, &encrypted)
      .unwrap();
    assert_eq!(decrypted, OPUS_FRAME);
  }
}
