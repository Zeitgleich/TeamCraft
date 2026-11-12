//! Who may let guests into a meeting, as its organizer signs it (#1103, e2ee.md §4.6, "Meeting
//! guests").
//!
//! A guest's page knows only `v`, the fingerprint of the organizer's identity key, from the meeting
//! link. Without more, it accepts the meeting chat's group only from a welcome sent by one of the
//! organizer's devices. This statement lets other people of the meeting admit too: the organizer's
//! identity key signs the identity keys of the admitters for one meeting and its chat, with a
//! sequence number that only grows and an end of validity. The server stores and serves it; a guest
//! checks it against `v`, a member against the organizer's pinned key, so a server can't add an
//! admitter, move a list to another meeting, or bring back an older list to whoever saw a newer one.
//!
//! From version 2 the statement also names the meeting's invitees (#1726, e2ee.md §4.6.5,
//! "Meetings (D10)"): a host's Join adds only users the organizer signed, so a server can't slip
//! an uninvited user past "only invited". A version 1 statement still verifies; it names no
//! invitees ([`VerifiedMeetingAdmitters::maybe_invitees`] is `None`).

use std::io::Write;

use tls_codec::{
  DeserializeBytes, Serialize as _, Size as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};

use crate::domain;
use crate::identity::{
  Ed25519Signature, IdentityError, IdentityFingerprint, IdentityKeyPair, IdentityPublicKey, VerifiedDevice,
};
use crate::ids::{ConversationId, MeetingId, UserId};

/// The first version: admitters, no invitees.
const MEETING_ADMITTERS_VERSION: u8 = 1;
/// Admitters and the invitees (#1726).
const MEETING_INVITEES_VERSION: u8 = 2;
/// At most this many admitters besides the organizer (a meeting chat's people).
pub const MAX_ADMITTERS: usize = 1_000;
/// At most this many invited users (the server's `max_invitees` is lower by default).
pub const MAX_INVITEES: usize = 1_000;
/// An encoded statement is never larger: the server refuses larger ones before reading them.
pub const MAX_ENCODED_BYTES: usize = 64 * 1024;

/// The admitters of one meeting, as the organizer states them. Encoded as [`AdmittersBody`], then,
/// from version 2, the invitees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingAdmitters {
  body: AdmittersBody,
  /// Sorted, one entry per user; `Some` exactly from version 2.
  maybe_invitees: Option<Vec<UserId>>,
}

/// The fields of every version, in their version 1 encoding.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct AdmittersBody {
  version: u8,
  meeting_id: MeetingId,
  conversation_id: ConversationId,
  /// The signer, so a guest who knows only its fingerprint can check the signature.
  organizer: IdentityPublicKey,
  sequence: u64,
  /// After this (milliseconds since the epoch, by the checker's clock) the list counts for nothing.
  expires_at_ms: u64,
  /// [`LinkAdmission`] as a byte.
  link_admission: u8,
  /// Sorted by user, one entry per user.
  admitters: Vec<Admitter>,
}

/// One person whose devices may let guests in: the identity key that certifies those devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct Admitter {
  pub user_id: UserId,
  pub identity_key: IdentityPublicKey,
}

/// How the admitters let in people who come with the meeting link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAdmission {
  /// After a host's "admit" in the lobby.
  Lobby,
  /// By themselves: the organizer chose "no lobby".
  ByThemselves,
}

/// What the organizer's device signs: everything but the signer's key, which the signature adds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittersDraft {
  pub meeting_id: MeetingId,
  pub conversation_id: ConversationId,
  pub sequence: u64,
  pub expires_at_ms: u64,
  pub link_admission: LinkAdmission,
  /// In any order; the statement sorts them. One entry per user.
  pub admitters: Vec<Admitter>,
  /// The users the organizer invited, in any order (the statement sorts them), or `None` for a
  /// version 1 statement without them (signed while older apps that can't read version 2 may still
  /// check it).
  pub maybe_invitees: Option<Vec<UserId>>,
}

/// A statement with the organizer's signature, as the server stores it.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedMeetingAdmitters {
  statement: MeetingAdmitters,
  signature: Ed25519Signature,
}

/// What a statement must match to count: the organizer's key, the meeting chat (whose group the
/// admitters let guests into) and the meeting where the checker knows it, the checker's clock and
/// the newest sequence it saw for the chat.
#[derive(Clone, Copy, Debug)]
pub struct AdmittersExpectation<'a> {
  /// `v` of the meeting link (a guest), or the fingerprint of the organizer's pinned key (a member).
  pub organizer: &'a IdentityFingerprint,
  /// The meeting the guest joined. A member's device knows the chat only: a chat belongs to one
  /// meeting, and the chat is what the statement binds the admitters to.
  pub maybe_meeting_id: Option<MeetingId>,
  pub conversation_id: ConversationId,
  pub now_ms: u64,
  pub maybe_seen_sequence: Option<u64>,
}

/// A statement whose signature and binding were checked. Only [`SignedMeetingAdmitters::verify`]
/// makes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMeetingAdmitters {
  statement: MeetingAdmitters,
}

#[derive(Debug, thiserror::Error)]
pub enum MeetingAdmittersError {
  #[error("unsupported admitters version {0}")]
  UnsupportedVersion(u8),
  #[error("unknown link admission {0}")]
  UnknownLinkAdmission(u8),
  #[error("the admitters are signed by another key than the organizer's")]
  WrongOrganizer,
  #[error("the admitters' signature is invalid: {0}")]
  InvalidSignature(IdentityError),
  #[error("the admitters are for meeting {actual}, expected {expected}")]
  WrongMeeting { expected: MeetingId, actual: MeetingId },
  #[error("the admitters are for conversation {actual}, expected {expected}")]
  WrongConversation {
    expected: ConversationId,
    actual: ConversationId,
  },
  #[error("the admitters expired at {expires_at_ms}")]
  Expired { expires_at_ms: u64 },
  #[error("admitters {offered} are older than {seen}, seen before")]
  Rollback { seen: u64, offered: u64 },
  #[error("{0} admitters are more than allowed")]
  TooManyAdmitters(usize),
  #[error("{0} invitees are more than allowed")]
  TooManyInvitees(usize),
  #[error("the admitters or invitees are not sorted by user, or name a user twice")]
  NotCanonical,
  #[error("the admitters take {0} bytes, more than allowed")]
  TooLarge(usize),
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl AdmittersDraft {
  /// Signs the statement with the organizer's identity key (every device of the organizer holds it).
  pub fn sign(self, organizer: &IdentityKeyPair) -> Result<SignedMeetingAdmitters, MeetingAdmittersError> {
    let mut admitters = self.admitters;
    admitters.sort_by_key(|admitter| admitter.user_id);
    admitters.dedup_by_key(|admitter| admitter.user_id);
    if admitters.len() > MAX_ADMITTERS {
      return Err(MeetingAdmittersError::TooManyAdmitters(admitters.len()));
    }
    let maybe_invitees = self.maybe_invitees.map(|mut invitees| {
      invitees.sort_unstable();
      invitees.dedup();
      invitees
    });
    if let Some(invitees) = &maybe_invitees
      && invitees.len() > MAX_INVITEES
    {
      return Err(MeetingAdmittersError::TooManyInvitees(invitees.len()));
    }
    let version = if maybe_invitees.is_some() {
      MEETING_INVITEES_VERSION
    } else {
      MEETING_ADMITTERS_VERSION
    };
    let statement = MeetingAdmitters {
      body: AdmittersBody {
        version,
        meeting_id: self.meeting_id,
        conversation_id: self.conversation_id,
        organizer: organizer.public_key(),
        sequence: self.sequence,
        expires_at_ms: self.expires_at_ms,
        link_admission: self.link_admission.to_byte(),
        admitters,
      },
      maybe_invitees,
    };
    let signature = organizer.sign_labeled(domain::MEETING_ADMITTERS, &statement.tls_serialize_detached()?);
    let signed = SignedMeetingAdmitters { statement, signature };
    signed.check_size()?;
    Ok(signed)
  }
}

impl SignedMeetingAdmitters {
  pub fn to_bytes(&self) -> Result<Vec<u8>, MeetingAdmittersError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MeetingAdmittersError> {
    if bytes.len() > MAX_ENCODED_BYTES {
      return Err(MeetingAdmittersError::TooLarge(bytes.len()));
    }
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  /// The claimed meeting, before verification (the server files the statement under it).
  pub fn unverified_meeting_id(&self) -> MeetingId {
    self.statement.body.meeting_id
  }

  /// The claimed meeting chat, before verification.
  pub fn unverified_conversation_id(&self) -> ConversationId {
    self.statement.body.conversation_id
  }

  /// The claimed sequence number, before verification.
  pub fn unverified_sequence(&self) -> u64 {
    self.statement.body.sequence
  }

  /// Whether the statement is well formed and signed by the key it names. The server's sanity
  /// check: it says nothing about whether that key is the organizer's.
  pub fn check_shape(&self) -> Result<(), MeetingAdmittersError> {
    self.check_size()?;
    self.check_signed_by_named_key()
  }

  /// Checks the statement against what the checker knows: signed by the organizer's key, for this
  /// meeting and its chat, not expired, and not older than one it saw.
  pub fn verify(
    &self,
    expected: &AdmittersExpectation<'_>,
  ) -> Result<VerifiedMeetingAdmitters, MeetingAdmittersError> {
    let statement = &self.statement.body;
    if statement.organizer.fingerprint() != *expected.organizer {
      return Err(MeetingAdmittersError::WrongOrganizer);
    }
    self.check_signed_by_named_key()?;
    if let Some(meeting_id) = expected.maybe_meeting_id
      && statement.meeting_id != meeting_id
    {
      return Err(MeetingAdmittersError::WrongMeeting {
        expected: meeting_id,
        actual: statement.meeting_id,
      });
    }
    if statement.conversation_id != expected.conversation_id {
      return Err(MeetingAdmittersError::WrongConversation {
        expected: expected.conversation_id,
        actual: statement.conversation_id,
      });
    }
    if let Some(seen) = expected.maybe_seen_sequence
      && statement.sequence < seen
    {
      return Err(MeetingAdmittersError::Rollback {
        seen,
        offered: statement.sequence,
      });
    }
    if expected.now_ms > statement.expires_at_ms {
      return Err(MeetingAdmittersError::Expired {
        expires_at_ms: statement.expires_at_ms,
      });
    }
    Ok(VerifiedMeetingAdmitters {
      statement: self.statement.clone(),
    })
  }

  fn check_size(&self) -> Result<(), MeetingAdmittersError> {
    let size = self.tls_serialized_len();
    if size > MAX_ENCODED_BYTES {
      return Err(MeetingAdmittersError::TooLarge(size));
    }
    Ok(())
  }

  fn check_signed_by_named_key(&self) -> Result<(), MeetingAdmittersError> {
    let statement = &self.statement.body;
    if !matches!(
      statement.version,
      MEETING_ADMITTERS_VERSION | MEETING_INVITEES_VERSION
    ) {
      return Err(MeetingAdmittersError::UnsupportedVersion(statement.version));
    }
    statement
      .organizer
      .verify_labeled(
        domain::MEETING_ADMITTERS,
        &self.statement.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(MeetingAdmittersError::InvalidSignature)?;
    LinkAdmission::from_byte(statement.link_admission)?;
    if statement.admitters.len() > MAX_ADMITTERS {
      return Err(MeetingAdmittersError::TooManyAdmitters(statement.admitters.len()));
    }
    let is_canonical = statement
      .admitters
      .windows(2)
      .all(|pair| pair[0].user_id < pair[1].user_id);
    if !is_canonical {
      return Err(MeetingAdmittersError::NotCanonical);
    }
    if let Some(invitees) = &self.statement.maybe_invitees {
      if invitees.len() > MAX_INVITEES {
        return Err(MeetingAdmittersError::TooManyInvitees(invitees.len()));
      }
      if !invitees.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(MeetingAdmittersError::NotCanonical);
      }
    }
    Ok(())
  }
}

impl VerifiedMeetingAdmitters {
  pub fn sequence(&self) -> u64 {
    self.statement.body.sequence
  }

  pub fn expires_at_ms(&self) -> u64 {
    self.statement.body.expires_at_ms
  }

  pub fn link_admission(&self) -> LinkAdmission {
    // Checked in `verify`.
    LinkAdmission::from_byte(self.statement.body.link_admission).unwrap_or(LinkAdmission::Lobby)
  }

  pub fn admitters(&self) -> &[Admitter] {
    &self.statement.body.admitters
  }

  /// Whether `user` with exactly `identity_key` may let guests in: a listed admitter, or the
  /// organizer (whose key signed the list).
  pub fn is_admitter(&self, user: UserId, identity_key: &IdentityPublicKey) -> bool {
    *identity_key == self.statement.body.organizer
      || self
        .statement
        .body
        .admitters
        .iter()
        .any(|admitter| admitter.user_id == user && admitter.identity_key == *identity_key)
  }

  /// Whether a device's certificate (checked against its user's pinned key) makes it an admitter's.
  pub fn admits_device(&self, device: &VerifiedDevice) -> bool {
    self.is_admitter(device.user_id(), device.identity_key())
  }

  /// The users the organizer invited, sorted; `None` for a version 1 statement, which doesn't say.
  pub fn maybe_invitees(&self) -> Option<&[UserId]> {
    self.statement.maybe_invitees.as_deref()
  }

  /// Whether the statement names the same admitters, link admission and invitees as `draft`
  /// (sequence and end of validity aside): the organizer's device signs anew only when this
  /// changes or the end nears. A version 1 statement differs from a draft with invitees.
  pub fn has_same_content(
    &self,
    admitters: &[Admitter],
    link_admission: LinkAdmission,
    maybe_invitees: Option<&[UserId]>,
  ) -> bool {
    let mut sorted = admitters.to_vec();
    sorted.sort_by_key(|admitter| admitter.user_id);
    sorted.dedup_by_key(|admitter| admitter.user_id);
    let maybe_sorted_invitees = maybe_invitees.map(|invitees| {
      let mut invitees = invitees.to_vec();
      invitees.sort_unstable();
      invitees.dedup();
      invitees
    });
    self.statement.body.admitters == sorted
      && self.link_admission() == link_admission
      && self.statement.maybe_invitees == maybe_sorted_invitees
  }
}

impl tls_codec::Size for MeetingAdmitters {
  fn tls_serialized_len(&self) -> usize {
    self.body.tls_serialized_len()
      + self
        .maybe_invitees
        .as_ref()
        .map_or(0, tls_codec::Size::tls_serialized_len)
  }
}

impl tls_codec::Serialize for MeetingAdmitters {
  fn tls_serialize<W: Write>(&self, writer: &mut W) -> Result<usize, tls_codec::Error> {
    let mut written = self.body.tls_serialize(writer)?;
    if let Some(invitees) = &self.maybe_invitees {
      written += invitees.tls_serialize(writer)?;
    }
    Ok(written)
  }
}

impl DeserializeBytes for MeetingAdmitters {
  /// Version 1 ends after the body; version 2 has the invitees after it. Any other version is
  /// read as version 1, so its remaining bytes fail the exact decoding.
  fn tls_deserialize_bytes(bytes: &[u8]) -> Result<(Self, &[u8]), tls_codec::Error> {
    let (body, rest) = AdmittersBody::tls_deserialize_bytes(bytes)?;
    if body.version != MEETING_INVITEES_VERSION {
      return Ok((
        Self {
          body,
          maybe_invitees: None,
        },
        rest,
      ));
    }
    let (invitees, rest) = Vec::<UserId>::tls_deserialize_bytes(rest)?;
    Ok((
      Self {
        body,
        maybe_invitees: Some(invitees),
      },
      rest,
    ))
  }
}

impl LinkAdmission {
  fn to_byte(self) -> u8 {
    match self {
      Self::Lobby => 0,
      Self::ByThemselves => 1,
    }
  }

  fn from_byte(byte: u8) -> Result<Self, MeetingAdmittersError> {
    match byte {
      0 => Ok(Self::Lobby),
      1 => Ok(Self::ByThemselves),
      other => Err(MeetingAdmittersError::UnknownLinkAdmission(other)),
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::membership::tests::TestUser;

  use super::*;

  const MEETING: MeetingId = MeetingId::new(7);
  const CHAT: ConversationId = ConversationId::new(70);
  const NOW_MS: u64 = 1_800_000_000_000;
  const ENDS_MS: u64 = NOW_MS + 3_600_000;

  #[test]
  fn a_guest_accepts_the_organizers_admitters_for_its_meeting() {
    let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
    let signed = draft(&[&bob], 5).sign(&organizer.identity).unwrap();
    let decoded = SignedMeetingAdmitters::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    let anchor = organizer.identity.public_key().fingerprint();

    let verified = decoded.verify(&expect(&anchor, None)).unwrap();

    assert_eq!(verified.sequence(), 5);
    assert_eq!(verified.link_admission(), LinkAdmission::ByThemselves);
    assert!(verified.admits_device(&device_of(&bob)));
    assert!(
      verified.admits_device(&device_of(&organizer)),
      "the organizer always admits"
    );
    assert!(decoded.check_shape().is_ok());
  }

  #[test]
  fn admitters_are_sorted_and_compared_without_order() {
    let organizer = TestUser::new(1);
    let (bob, carol) = (TestUser::new(3), TestUser::new(2));
    let anchor = organizer.identity.public_key().fingerprint();
    let verified = draft(&[&bob, &carol], 1)
      .sign(&organizer.identity)
      .unwrap()
      .verify(&expect(&anchor, None))
      .unwrap();
    let ids: Vec<u64> = verified.admitters().iter().map(|a| a.user_id.get()).collect();
    assert_eq!(ids, vec![2, 3]);
    let listed = [admitter(&bob), admitter(&carol)];
    assert!(verified.has_same_content(&listed, LinkAdmission::ByThemselves, None));
    assert!(!verified.has_same_content(&listed, LinkAdmission::Lobby, None));
    assert!(!verified.has_same_content(&listed[..1], LinkAdmission::ByThemselves, None));
  }

  mod invitees {
    use super::*;

    #[test]
    fn the_invitees_are_signed_sorted_and_round_trip() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      let mut with_invitees = draft(&[&bob], 3);
      with_invitees.maybe_invitees = Some(vec![UserId::new(9), UserId::new(4), UserId::new(9)]);
      let signed = with_invitees.sign(&organizer.identity).unwrap();
      let decoded = SignedMeetingAdmitters::from_bytes(&signed.to_bytes().unwrap()).unwrap();

      let verified = decoded.verify(&expect(&anchor, None)).unwrap();

      assert_eq!(
        verified.maybe_invitees(),
        Some(&[UserId::new(4), UserId::new(9)][..])
      );
      assert!(decoded.check_shape().is_ok());
      let listed = [admitter(&bob)];
      let invitees = [UserId::new(9), UserId::new(4)];
      assert!(verified.has_same_content(&listed, LinkAdmission::ByThemselves, Some(&invitees)));
      assert!(!verified.has_same_content(&listed, LinkAdmission::ByThemselves, None));
      assert!(!verified.has_same_content(
        &listed,
        LinkAdmission::ByThemselves,
        Some(&invitees[..1])
      ));
    }

    #[test]
    fn a_first_version_statement_still_verifies_and_names_no_invitees() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      let signed = draft(&[&bob], 3).sign(&organizer.identity).unwrap();
      let bytes = signed.to_bytes().unwrap();
      assert_eq!(bytes[0], MEETING_ADMITTERS_VERSION, "signed as version 1");

      let verified = SignedMeetingAdmitters::from_bytes(&bytes)
        .unwrap()
        .verify(&expect(&anchor, None))
        .unwrap();

      assert_eq!(verified.maybe_invitees(), None);
      assert!(verified.admits_device(&device_of(&bob)));
    }

    #[test]
    fn an_invitee_the_server_adds_breaks_the_signature() {
      let organizer = TestUser::new(1);
      let anchor = organizer.identity.public_key().fingerprint();
      let mut with_invitees = draft(&[], 3);
      with_invitees.maybe_invitees = Some(vec![UserId::new(4)]);
      let mut signed = with_invitees.sign(&organizer.identity).unwrap();
      signed
        .statement
        .maybe_invitees
        .as_mut()
        .unwrap()
        .push(UserId::new(66));
      assert!(matches!(
        signed.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::InvalidSignature(_))
      ));
    }

    #[test]
    fn stripping_the_invitees_breaks_the_signature() {
      let organizer = TestUser::new(1);
      let anchor = organizer.identity.public_key().fingerprint();
      let mut with_invitees = draft(&[], 3);
      with_invitees.maybe_invitees = Some(vec![UserId::new(4)]);
      let mut signed = with_invitees.sign(&organizer.identity).unwrap();
      // The server drops the list and claims version 1, so the Join would take its own list.
      signed.statement.maybe_invitees = None;
      signed.statement.body.version = MEETING_ADMITTERS_VERSION;
      assert!(matches!(
        signed.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::InvalidSignature(_))
      ));
    }

    #[test]
    fn unsorted_or_too_many_invitees_are_refused() {
      let organizer = TestUser::new(1);
      let anchor = organizer.identity.public_key().fingerprint();
      let mut with_invitees = draft(&[], 1);
      with_invitees.maybe_invitees = Some(vec![UserId::new(4), UserId::new(5)]);
      let mut statement = with_invitees.sign(&organizer.identity).unwrap().statement;
      if let Some(invitees) = statement.maybe_invitees.as_mut() {
        invitees.reverse();
      }
      assert!(matches!(
        resign(statement, &organizer).verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::NotCanonical)
      ));

      let mut too_many = draft(&[], 1);
      too_many.maybe_invitees = Some((0..=MAX_INVITEES as u64).map(UserId::new).collect());
      assert!(matches!(
        too_many.sign(&organizer.identity),
        Err(MeetingAdmittersError::TooManyInvitees(_))
      ));
    }

    #[test]
    fn an_unknown_version_is_refused() {
      let organizer = TestUser::new(1);
      let anchor = organizer.identity.public_key().fingerprint();
      let mut statement = draft(&[], 1).sign(&organizer.identity).unwrap().statement;
      statement.body.version = 3;
      assert!(matches!(
        resign(statement, &organizer).verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::UnsupportedVersion(3))
      ));
    }
  }

  mod forged {
    use super::*;

    #[test]
    fn a_list_signed_by_another_key_is_refused() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      // The server (or Bob) signs a list naming itself, with a key of its own.
      let server = TestUser::new(1);
      let forged = draft(&[&bob], 9).sign(&server.identity).unwrap();
      assert!(matches!(
        forged.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::WrongOrganizer)
      ));
    }

    #[test]
    fn a_list_with_the_organizers_key_but_another_signature_is_refused() {
      let (organizer, bob, mallory) = (TestUser::new(1), TestUser::new(2), TestUser::new(66));
      let anchor = organizer.identity.public_key().fingerprint();
      let mut signed = draft(&[&bob], 2).sign(&organizer.identity).unwrap();
      // The server adds itself to a list the organizer signed.
      signed.statement.body.admitters.push(admitter(&mallory));
      assert!(matches!(
        signed.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::InvalidSignature(_))
      ));
      assert!(signed.check_shape().is_err());
    }

    #[test]
    fn a_list_of_another_meeting_or_chat_is_refused() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      let mut other_meeting = draft(&[&bob], 1);
      other_meeting.meeting_id = MeetingId::new(8);
      let other_meeting = other_meeting.sign(&organizer.identity).unwrap();
      assert!(matches!(
        other_meeting.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::WrongMeeting { .. })
      ));
      let mut other_chat = draft(&[&bob], 1);
      other_chat.conversation_id = ConversationId::new(71);
      let other_chat = other_chat.sign(&organizer.identity).unwrap();
      // A member, who knows the chat but not the meeting, still refuses another chat's list.
      let mut by_chat = expect(&anchor, None);
      by_chat.maybe_meeting_id = None;
      assert!(matches!(
        other_chat.verify(&by_chat),
        Err(MeetingAdmittersError::WrongConversation { .. })
      ));
      assert!(matches!(
        other_chat.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::WrongConversation { .. })
      ));
    }

    #[test]
    fn an_older_list_than_one_seen_is_refused() {
      let (organizer, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let anchor = organizer.identity.public_key().fingerprint();
      // Carol was an admitter in version 4 and is no longer in version 6.
      let old = draft(&[&bob, &carol], 4).sign(&organizer.identity).unwrap();
      let new = draft(&[&bob], 6).sign(&organizer.identity).unwrap();
      let seen = new.verify(&expect(&anchor, None)).unwrap().sequence();
      assert!(matches!(
        old.verify(&expect(&anchor, Some(seen))),
        Err(MeetingAdmittersError::Rollback { seen: 6, offered: 4 })
      ));
      // The same version again is no rollback.
      assert!(new.verify(&expect(&anchor, Some(seen))).is_ok());
    }

    #[test]
    fn an_expired_list_is_refused() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      let signed = draft(&[&bob], 1).sign(&organizer.identity).unwrap();
      let mut late = expect(&anchor, None);
      late.now_ms = ENDS_MS + 1;
      assert!(matches!(
        signed.verify(&late),
        Err(MeetingAdmittersError::Expired {
          expires_at_ms: ENDS_MS
        })
      ));
    }

    #[test]
    fn a_device_its_listed_key_did_not_certify_is_no_admitters() {
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = organizer.identity.public_key().fingerprint();
      let verified = draft(&[&bob], 1)
        .sign(&organizer.identity)
        .unwrap()
        .verify(&expect(&anchor, None))
        .unwrap();
      // The server's device for Bob, certified by a key of its own; and a stranger.
      let fake_bob = TestUser::new(2);
      let stranger = TestUser::new(5);
      assert!(!verified.admits_device(&device_of(&fake_bob)));
      assert!(!verified.admits_device(&device_of(&stranger)));
      // Bob's key under another user's ID does not count either.
      assert!(!verified.is_admitter(UserId::new(5), &bob.identity.public_key()));
    }

    #[test]
    fn hand_made_lists_out_of_order_or_too_long_are_refused() {
      let (organizer, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let anchor = organizer.identity.public_key().fingerprint();
      let mut statement = draft(&[&bob, &carol], 1)
        .sign(&organizer.identity)
        .unwrap()
        .statement;
      statement.body.admitters.reverse();
      let unsorted = resign(statement, &organizer);
      assert!(matches!(
        unsorted.verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::NotCanonical)
      ));

      let mut statement = draft(&[&bob], 1).sign(&organizer.identity).unwrap().statement;
      statement.body.link_admission = 7;
      assert!(matches!(
        resign(statement, &organizer).verify(&expect(&anchor, None)),
        Err(MeetingAdmittersError::UnknownLinkAdmission(7))
      ));

      let many: Vec<Admitter> = (0..=MAX_ADMITTERS as u64)
        .map(|id| Admitter {
          user_id: UserId::new(id + 10),
          identity_key: bob.identity.public_key(),
        })
        .collect();
      let mut too_many = draft(&[], 1);
      too_many.admitters = many;
      assert!(matches!(
        too_many.sign(&organizer.identity),
        Err(MeetingAdmittersError::TooManyAdmitters(_))
      ));
    }

    #[test]
    fn garbage_and_oversized_bytes_are_refused() {
      assert!(SignedMeetingAdmitters::from_bytes(&[1, 2, 3]).is_err());
      assert!(matches!(
        SignedMeetingAdmitters::from_bytes(&vec![0; MAX_ENCODED_BYTES + 1]),
        Err(MeetingAdmittersError::TooLarge(_))
      ));
      let (organizer, bob) = (TestUser::new(1), TestUser::new(2));
      let mut bytes = draft(&[&bob], 1)
        .sign(&organizer.identity)
        .unwrap()
        .to_bytes()
        .unwrap();
      bytes.push(0);
      assert!(
        SignedMeetingAdmitters::from_bytes(&bytes).is_err(),
        "trailing bytes"
      );
    }
  }

  fn draft(admitters: &[&TestUser], sequence: u64) -> AdmittersDraft {
    AdmittersDraft {
      meeting_id: MEETING,
      conversation_id: CHAT,
      sequence,
      expires_at_ms: ENDS_MS,
      link_admission: LinkAdmission::ByThemselves,
      admitters: admitters.iter().map(|user| admitter(user)).collect(),
      maybe_invitees: None,
    }
  }

  fn expect(organizer: &IdentityFingerprint, maybe_seen_sequence: Option<u64>) -> AdmittersExpectation<'_> {
    AdmittersExpectation {
      organizer,
      maybe_meeting_id: Some(MEETING),
      conversation_id: CHAT,
      now_ms: NOW_MS,
      maybe_seen_sequence,
    }
  }

  fn admitter(user: &TestUser) -> Admitter {
    Admitter {
      user_id: user.user_id,
      identity_key: user.identity.public_key(),
    }
  }

  fn device_of(user: &TestUser) -> VerifiedDevice {
    user.certificate.verify(&user.identity.public_key()).unwrap()
  }

  fn resign(statement: MeetingAdmitters, organizer: &TestUser) -> SignedMeetingAdmitters {
    let signature = organizer.identity.sign_labeled(
      domain::MEETING_ADMITTERS,
      &statement.tls_serialize_detached().unwrap(),
    );
    SignedMeetingAdmitters { statement, signature }
  }
}
