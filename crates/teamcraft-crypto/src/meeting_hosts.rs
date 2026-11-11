//! Who hosts a meeting, as its organizer signs it (#1720, e2ee.md §4.6, "Meeting hosts").
//!
//! A meeting has one organizer and any number of co-hosts, who let people in and make guest links
//! like the organizer. Guests and members must not take the server's word for who that is: a guest
//! knows only `v`, the fingerprint of the organizer's identity key from the meeting link, a member
//! the key of the person who founded the chat's encryption group. So the organizer's identity key
//! signs the host list: the meeting and its chat, a sequence that only grows, the organizer from
//! then on (the signer, or somebody the signer hands the meeting over to) and the co-hosts, each
//! with the identity key that certifies their devices.
//!
//! A handover is a list signed by the organizer so far that names another organizer. From then on
//! only the new organizer's key signs; the server keeps every handover, so whoever trusts an older
//! organizer's key (a link made before) follows the chain to the newest list:
//! [`verify_chain`] starts at the key the checker trusts and accepts a list only if the organizer
//! the list before it named signed it. A list signed by anybody else is passed over, so the server
//! can't add a host, and a former organizer can't take the meeting back.

use tls_codec::{
  DeserializeBytes as _, Serialize as _, Size as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};

use crate::domain;
use crate::identity::{
  Ed25519Signature, IdentityError, IdentityFingerprint, IdentityKeyPair, IdentityPublicKey, VerifiedDevice,
};
use crate::ids::{ConversationId, MeetingId, UserId};

const MEETING_HOSTS_VERSION: u8 = 1;
/// At most this many co-hosts besides the organizer.
pub const MAX_CO_HOSTS: usize = 100;
/// An encoded list is never larger: the server refuses larger ones before reading them.
pub const MAX_ENCODED_BYTES: usize = 8 * 1024;
/// At most this many lists in a chain a checker reads: the handovers and the newest list. The
/// server refuses a handover beyond [`MAX_HANDOVERS`].
pub const MAX_HANDOVERS: usize = 32;
pub const MAX_CHAIN: usize = MAX_HANDOVERS + 1;

/// The hosts of one meeting from one sequence on, as the organizer of the time signs them.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct MeetingHosts {
  version: u8,
  meeting_id: MeetingId,
  conversation_id: ConversationId,
  sequence: u64,
  /// Who signed: the organizer until this list.
  signer: Host,
  /// The organizer from this list on: the signer, or the one the signer hands the meeting to.
  organizer: Host,
  /// Sorted by user, one entry per user, never the organizer.
  co_hosts: Vec<Host>,
}

/// One host: the user and the identity key that certifies their devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct Host {
  pub user_id: UserId,
  pub identity_key: IdentityPublicKey,
}

/// What the organizer's device signs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsDraft {
  pub meeting_id: MeetingId,
  pub conversation_id: ConversationId,
  pub sequence: u64,
  /// The signing organizer's user: the key comes from the signature's key pair.
  pub signer_user_id: UserId,
  /// The organizer from now on; the signer's own user and key unless the meeting is handed over.
  pub organizer: Host,
  /// In any order; the list sorts them and leaves the organizer out. One entry per user.
  pub co_hosts: Vec<Host>,
}

/// A host list with the signer's signature, as the server stores it.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct SignedMeetingHosts {
  statement: MeetingHosts,
  signature: Ed25519Signature,
}

/// What a chain must match to count.
#[derive(Clone, Copy, Debug)]
pub struct HostsExpectation<'a> {
  /// The key the checker trusts as the meeting's organizer: `v` of the meeting link (a guest), or
  /// the key of the group's founder as the member's device verified it.
  pub anchor: &'a IdentityFingerprint,
  /// The meeting the guest joined. A member's device knows the chat only: a chat belongs to one
  /// meeting.
  pub maybe_meeting_id: Option<MeetingId>,
  pub conversation_id: ConversationId,
  /// The newest sequence the checker accepted before for this chat.
  pub maybe_seen_sequence: Option<u64>,
}

/// The hosts a chain says, from the checker's anchor on. Only [`verify_chain`] makes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMeetingHosts {
  /// 0 when no list reached from the anchor: then the anchor alone hosts.
  sequence: u64,
  organizer_fingerprint: IdentityFingerprint,
  /// The organizer's user, where a list named it.
  maybe_organizer: Option<Host>,
  co_hosts: Vec<Host>,
  /// The newest list was signed by an organizer before the current one: a handover the new
  /// organizer hasn't signed a list of their own after.
  is_handover_pending: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum MeetingHostsError {
  #[error("unsupported host list version {0}")]
  UnsupportedVersion(u8),
  #[error("the host list's signature is invalid: {0}")]
  InvalidSignature(IdentityError),
  #[error("the host list is for meeting {actual}, expected {expected}")]
  WrongMeeting { expected: MeetingId, actual: MeetingId },
  #[error("the host list is for conversation {actual}, expected {expected}")]
  WrongConversation {
    expected: ConversationId,
    actual: ConversationId,
  },
  #[error("host list {offered} is older than {seen}, seen before")]
  Rollback { seen: u64, offered: u64 },
  #[error("{0} co-hosts are more than allowed")]
  TooManyCoHosts(usize),
  #[error("the co-hosts are not sorted by user, name a user twice or name the organizer")]
  NotCanonical,
  #[error("the host list takes {0} bytes, more than allowed")]
  TooLarge(usize),
  #[error("{0} host lists are more than a chain holds")]
  ChainTooLong(usize),
  #[error("encoding failed: {0}")]
  Encoding(#[from] tls_codec::Error),
}

impl HostsDraft {
  /// Signs the list with the signing organizer's identity key (every device of theirs holds it).
  pub fn sign(self, signer: &IdentityKeyPair) -> Result<SignedMeetingHosts, MeetingHostsError> {
    let mut co_hosts = self.co_hosts;
    co_hosts.retain(|host| host.user_id != self.organizer.user_id);
    co_hosts.sort_by_key(|host| host.user_id);
    co_hosts.dedup_by_key(|host| host.user_id);
    if co_hosts.len() > MAX_CO_HOSTS {
      return Err(MeetingHostsError::TooManyCoHosts(co_hosts.len()));
    }
    let statement = MeetingHosts {
      version: MEETING_HOSTS_VERSION,
      meeting_id: self.meeting_id,
      conversation_id: self.conversation_id,
      sequence: self.sequence,
      signer: Host {
        user_id: self.signer_user_id,
        identity_key: signer.public_key(),
      },
      organizer: self.organizer,
      co_hosts,
    };
    let signature = signer.sign_labeled(domain::MEETING_HOSTS, &statement.tls_serialize_detached()?);
    let signed = SignedMeetingHosts { statement, signature };
    signed.check_size()?;
    Ok(signed)
  }
}

impl SignedMeetingHosts {
  pub fn to_bytes(&self) -> Result<Vec<u8>, MeetingHostsError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MeetingHostsError> {
    if bytes.len() > MAX_ENCODED_BYTES {
      return Err(MeetingHostsError::TooLarge(bytes.len()));
    }
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }

  /// The claimed meeting, before verification (the server files the list under it).
  pub fn unverified_meeting_id(&self) -> MeetingId {
    self.statement.meeting_id
  }

  pub fn unverified_conversation_id(&self) -> ConversationId {
    self.statement.conversation_id
  }

  pub fn unverified_sequence(&self) -> u64 {
    self.statement.sequence
  }

  /// The signer's user as the list claims it. The server checks it is the caller.
  pub fn unverified_signer(&self) -> UserId {
    self.statement.signer.user_id
  }

  /// The organizer from this list on, as it claims.
  pub fn unverified_organizer(&self) -> UserId {
    self.statement.organizer.user_id
  }

  /// The co-hosts' users, as the list claims them.
  pub fn unverified_co_hosts(&self) -> Vec<UserId> {
    self.statement.co_hosts.iter().map(|host| host.user_id).collect()
  }

  /// Whether the list hands the meeting to somebody else: the server keeps every one of these, so
  /// a checker who trusts an older organizer follows the chain.
  pub fn is_handover(&self) -> bool {
    self.statement.signer.identity_key != self.statement.organizer.identity_key
      || self.statement.signer.user_id != self.statement.organizer.user_id
  }

  /// Whether the list is well formed and signed by the key it names. The server's sanity check: it
  /// says nothing about whether that key is the organizer's.
  pub fn check_shape(&self) -> Result<(), MeetingHostsError> {
    self.check_size()?;
    self.check_signed_by_named_key()
  }

  fn check_binding(&self, expected: &HostsExpectation<'_>) -> Result<(), MeetingHostsError> {
    let statement = &self.statement;
    if let Some(meeting_id) = expected.maybe_meeting_id
      && statement.meeting_id != meeting_id
    {
      return Err(MeetingHostsError::WrongMeeting {
        expected: meeting_id,
        actual: statement.meeting_id,
      });
    }
    if statement.conversation_id != expected.conversation_id {
      return Err(MeetingHostsError::WrongConversation {
        expected: expected.conversation_id,
        actual: statement.conversation_id,
      });
    }
    Ok(())
  }

  fn check_size(&self) -> Result<(), MeetingHostsError> {
    let size = self.tls_serialized_len();
    if size > MAX_ENCODED_BYTES {
      return Err(MeetingHostsError::TooLarge(size));
    }
    Ok(())
  }

  fn check_signed_by_named_key(&self) -> Result<(), MeetingHostsError> {
    let statement = &self.statement;
    if statement.version != MEETING_HOSTS_VERSION {
      return Err(MeetingHostsError::UnsupportedVersion(statement.version));
    }
    statement
      .signer
      .identity_key
      .verify_labeled(
        domain::MEETING_HOSTS,
        &statement.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(MeetingHostsError::InvalidSignature)?;
    if statement.co_hosts.len() > MAX_CO_HOSTS {
      return Err(MeetingHostsError::TooManyCoHosts(statement.co_hosts.len()));
    }
    let is_canonical = statement
      .co_hosts
      .windows(2)
      .all(|pair| pair[0].user_id < pair[1].user_id)
      && statement
        .co_hosts
        .iter()
        .all(|host| host.user_id != statement.organizer.user_id);
    if !is_canonical {
      return Err(MeetingHostsError::NotCanonical);
    }
    Ok(())
  }
}

/// Follows the host lists of a meeting chat from the key the checker trusts: in the order of their
/// sequence, a list counts only if the organizer the counted list before it named (at first the
/// anchor) signed it, for this chat (and meeting, where known); each counted list names the
/// organizer whose key signs the next. Lists that don't verify or that somebody else signed are
/// passed over, so a server can't add a host and a former organizer can't take the meeting back.
/// Without a list from the anchor on, the anchor alone hosts (sequence 0). A newest counted list
/// older than one the checker saw is a rollback.
pub fn verify_chain(
  statements: &[SignedMeetingHosts],
  expected: &HostsExpectation<'_>,
) -> Result<VerifiedMeetingHosts, MeetingHostsError> {
  if statements.len() > MAX_CHAIN {
    return Err(MeetingHostsError::ChainTooLong(statements.len()));
  }
  let mut ordered: Vec<&SignedMeetingHosts> = statements.iter().collect();
  ordered.sort_by_key(|signed| signed.statement.sequence);
  let mut trusted = *expected.anchor;
  let mut maybe_current: Option<&MeetingHosts> = None;
  for signed in ordered {
    let statement = &signed.statement;
    if maybe_current.is_some_and(|current| statement.sequence <= current.sequence) {
      continue;
    }
    if statement.signer.identity_key.fingerprint() != trusted {
      continue;
    }
    if signed.check_signed_by_named_key().is_err() || signed.check_binding(expected).is_err() {
      continue;
    }
    trusted = statement.organizer.identity_key.fingerprint();
    maybe_current = Some(statement);
  }
  let sequence = maybe_current.map_or(0, |current| current.sequence);
  if let Some(seen) = expected.maybe_seen_sequence
    && sequence < seen
  {
    return Err(MeetingHostsError::Rollback {
      seen,
      offered: sequence,
    });
  }
  Ok(match maybe_current {
    Some(current) => VerifiedMeetingHosts {
      sequence,
      organizer_fingerprint: trusted,
      maybe_organizer: Some(current.organizer),
      co_hosts: current.co_hosts.clone(),
      is_handover_pending: current.signer.identity_key != current.organizer.identity_key,
    },
    None => VerifiedMeetingHosts {
      sequence,
      organizer_fingerprint: trusted,
      maybe_organizer: None,
      co_hosts: Vec::new(),
      is_handover_pending: false,
    },
  })
}

impl VerifiedMeetingHosts {
  pub fn sequence(&self) -> u64 {
    self.sequence
  }

  /// The fingerprint of the current organizer's identity key: what signs the next list and the
  /// meeting's admitters (`meeting_admitters`), and the `v` of links made from now on.
  pub fn organizer_fingerprint(&self) -> &IdentityFingerprint {
    &self.organizer_fingerprint
  }

  /// The current organizer, where a list named them (not for the anchor alone).
  pub fn maybe_organizer(&self) -> Option<&Host> {
    self.maybe_organizer.as_ref()
  }

  pub fn co_hosts(&self) -> &[Host] {
    &self.co_hosts
  }

  /// The newest list hands the meeting over and the new organizer hasn't signed one since: their
  /// device signs the same hosts again, so that links with their key find the co-hosts too.
  pub fn is_handover_pending(&self) -> bool {
    self.is_handover_pending
  }

  /// Whether `user` with exactly `identity_key` hosts: the current organizer (by key), or a listed
  /// co-host.
  pub fn is_host(&self, user: UserId, identity_key: &IdentityPublicKey) -> bool {
    identity_key.fingerprint() == self.organizer_fingerprint
      || self
        .co_hosts
        .iter()
        .any(|host| host.user_id == user && host.identity_key == *identity_key)
  }

  /// Whether a device's certificate (checked against its user's pinned key) makes it a host's.
  pub fn hosts_device(&self, device: &VerifiedDevice) -> bool {
    self.is_host(device.user_id(), device.identity_key())
  }

  /// Whether `identity_key` is the current organizer's.
  pub fn is_organizer_key(&self, identity_key: &IdentityPublicKey) -> bool {
    identity_key.fingerprint() == self.organizer_fingerprint
  }

  /// Whether `organizer` and `co_hosts` are the hosts this says (order aside).
  pub fn has_same_hosts(&self, organizer: &Host, co_hosts: &[Host]) -> bool {
    let mut sorted: Vec<Host> = co_hosts
      .iter()
      .copied()
      .filter(|host| host.user_id != organizer.user_id)
      .collect();
    sorted.sort_by_key(|host| host.user_id);
    sorted.dedup_by_key(|host| host.user_id);
    self.maybe_organizer.as_ref() == Some(organizer) && self.co_hosts == sorted
  }
}

#[cfg(test)]
mod tests {
  use crate::membership::tests::TestUser;

  use super::*;

  const MEETING: MeetingId = MeetingId::new(7);
  const CHAT: ConversationId = ConversationId::new(70);

  #[test]
  fn the_organizers_list_names_the_hosts() {
    let (alice, bob) = (TestUser::new(1), TestUser::new(2));
    let signed = draft(&alice, &alice, &[&bob], 5).sign(&alice.identity).unwrap();
    let decoded = SignedMeetingHosts::from_bytes(&signed.to_bytes().unwrap()).unwrap();
    assert!(decoded.check_shape().is_ok());
    assert!(!decoded.is_handover());

    let verified = verify_chain(&[decoded], &expect(&fingerprint(&alice), None)).unwrap();

    assert_eq!(verified.sequence(), 5);
    assert!(verified.hosts_device(&device_of(&bob)));
    assert!(verified.hosts_device(&device_of(&alice)), "the organizer hosts");
    assert!(!verified.is_handover_pending());
    assert!(verified.has_same_hosts(&host(&alice), &[host(&bob), host(&alice)]));
  }

  #[test]
  fn without_a_list_the_anchor_alone_hosts() {
    let (alice, bob) = (TestUser::new(1), TestUser::new(2));
    let verified = verify_chain(&[], &expect(&fingerprint(&alice), None)).unwrap();
    assert_eq!(verified.sequence(), 0);
    assert!(verified.hosts_device(&device_of(&alice)));
    assert!(!verified.hosts_device(&device_of(&bob)));
    assert!(verified.maybe_organizer().is_none());
  }

  mod handover {
    use super::*;

    /// Alice names Bob co-host, hands the meeting to Carol (staying co-host), and Carol signs her
    /// own list without Bob.
    fn history(alice: &TestUser, bob: &TestUser, carol: &TestUser) -> Vec<SignedMeetingHosts> {
      vec![
        draft(alice, alice, &[bob], 1).sign(&alice.identity).unwrap(),
        draft(alice, carol, &[bob, alice], 2)
          .sign(&alice.identity)
          .unwrap(),
        draft(carol, carol, &[alice], 3).sign(&carol.identity).unwrap(),
      ]
    }

    #[test]
    fn a_link_with_the_old_organizers_key_follows_the_chain() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let chain = history(&alice, &bob, &carol);
      assert!(chain[1].is_handover());

      let verified = verify_chain(&chain, &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(verified.sequence(), 3);
      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&carol));
      assert!(verified.hosts_device(&device_of(&carol)));
      assert!(verified.hosts_device(&device_of(&alice)), "Alice stays co-host");
      assert!(!verified.hosts_device(&device_of(&bob)), "Carol took Bob off");
    }

    #[test]
    fn a_link_with_the_new_organizers_key_starts_at_her_lists() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let chain = history(&alice, &bob, &carol);

      let verified = verify_chain(&chain, &expect(&fingerprint(&carol), None)).unwrap();

      assert_eq!(verified.sequence(), 3);
      assert!(verified.hosts_device(&device_of(&alice)));
      // Before Carol signed one, her key alone hosts: Alice's handover isn't hers.
      let only_handover = verify_chain(&chain[..2], &expect(&fingerprint(&carol), None)).unwrap();
      assert_eq!(only_handover.sequence(), 0);
      assert!(!only_handover.hosts_device(&device_of(&alice)));
      assert!(only_handover.hosts_device(&device_of(&carol)));
    }

    #[test]
    fn the_handover_counts_before_the_new_organizer_signs() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let chain = history(&alice, &bob, &carol);

      let verified = verify_chain(&chain[..2], &expect(&fingerprint(&alice), None)).unwrap();

      assert!(verified.is_handover_pending());
      assert_eq!(verified.maybe_organizer(), Some(&host(&carol)));
      assert!(verified.hosts_device(&device_of(&bob)));
      assert!(verified.hosts_device(&device_of(&alice)));
    }

    #[test]
    fn the_former_organizer_cannot_take_the_meeting_back() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let mut chain = history(&alice, &bob, &carol);
      // Alice, co-host now, signs a later list naming herself organizer and Bob again.
      chain.push(draft(&alice, &alice, &[&bob], 4).sign(&alice.identity).unwrap());

      let verified = verify_chain(&chain, &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(verified.sequence(), 3);
      assert!(!verified.hosts_device(&device_of(&bob)));
      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&carol));
    }

    #[test]
    fn a_handover_back_and_forth_ends_with_the_last_organizer() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let mut chain = history(&alice, &bob, &carol);
      chain.push(draft(&carol, &alice, &[&carol], 4).sign(&carol.identity).unwrap());
      chain.push(draft(&alice, &alice, &[&bob], 5).sign(&alice.identity).unwrap());

      let verified = verify_chain(&chain, &expect(&fingerprint(&carol), None)).unwrap();

      assert_eq!(verified.sequence(), 5);
      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&alice));
      assert!(verified.hosts_device(&device_of(&bob)));
      assert!(!verified.hosts_device(&device_of(&carol)));
    }
  }

  mod forged {
    use super::*;

    #[test]
    fn a_list_signed_by_another_key_adds_no_host() {
      let (alice, bob, mallory) = (TestUser::new(1), TestUser::new(2), TestUser::new(66));
      let real = draft(&alice, &alice, &[&bob], 1).sign(&alice.identity).unwrap();
      // The server signs a later list with a key of its own under Alice's user, naming Mallory.
      let server = TestUser::new(1);
      let forged = draft(&server, &server, &[&bob, &mallory], 2)
        .sign(&server.identity)
        .unwrap();

      let verified = verify_chain(&[real, forged], &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(verified.sequence(), 1);
      assert!(!verified.hosts_device(&device_of(&mallory)));
      assert!(verified.hosts_device(&device_of(&bob)));
    }

    #[test]
    fn an_entry_added_to_a_signed_list_breaks_its_signature() {
      let (alice, bob, mallory) = (TestUser::new(1), TestUser::new(2), TestUser::new(66));
      let mut signed = draft(&alice, &alice, &[&bob], 2).sign(&alice.identity).unwrap();
      signed.statement.co_hosts.push(host(&mallory));
      assert!(matches!(
        signed.check_shape(),
        Err(MeetingHostsError::InvalidSignature(_))
      ));

      let verified = verify_chain(&[signed], &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(verified.sequence(), 0, "nothing counts");
      assert!(!verified.hosts_device(&device_of(&mallory)));
      assert!(!verified.hosts_device(&device_of(&bob)));
    }

    #[test]
    fn a_forged_handover_moves_nothing() {
      let (alice, mallory) = (TestUser::new(1), TestUser::new(66));
      // Mallory signs a "handover" from Alice's user to herself with her own key.
      let mut forged = draft(&mallory, &mallory, &[], 2);
      forged.signer_user_id = alice.user_id;
      let forged = forged.sign(&mallory.identity).unwrap();

      let verified = verify_chain(&[forged], &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&alice));
      assert!(!verified.hosts_device(&device_of(&mallory)));
    }

    #[test]
    fn a_revoked_co_host_stays_revoked_for_whoever_saw_the_newer_list() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let older = draft(&alice, &alice, &[&bob, &carol], 4)
        .sign(&alice.identity)
        .unwrap();
      let newer = draft(&alice, &alice, &[&carol], 6).sign(&alice.identity).unwrap();
      let anchor = fingerprint(&alice);
      let seen = verify_chain(&[older.clone(), newer.clone()], &expect(&anchor, None)).unwrap();
      assert!(!seen.hosts_device(&device_of(&bob)), "Bob was taken off");

      // The server serves the older list alone again.
      assert!(matches!(
        verify_chain(&[older], &expect(&anchor, Some(seen.sequence()))),
        Err(MeetingHostsError::Rollback { seen: 6, offered: 4 })
      ));
      // And withholding every list is a rollback too.
      assert!(matches!(
        verify_chain(&[], &expect(&anchor, Some(seen.sequence()))),
        Err(MeetingHostsError::Rollback { seen: 6, offered: 0 })
      ));
      // The same list again is fine.
      assert!(verify_chain(&[newer], &expect(&anchor, Some(6))).is_ok());
    }

    #[test]
    fn a_list_of_another_meeting_or_chat_is_passed_over() {
      let (alice, bob) = (TestUser::new(1), TestUser::new(2));
      let anchor = fingerprint(&alice);
      let mut other_meeting = draft(&alice, &alice, &[&bob], 1);
      other_meeting.meeting_id = MeetingId::new(8);
      let other_meeting = other_meeting.sign(&alice.identity).unwrap();
      let mut other_chat = draft(&alice, &alice, &[&bob], 2);
      other_chat.conversation_id = ConversationId::new(71);
      let other_chat = other_chat.sign(&alice.identity).unwrap();

      let as_guest = verify_chain(
        &[other_meeting.clone(), other_chat.clone()],
        &expect(&anchor, None),
      )
      .unwrap();
      assert!(!as_guest.hosts_device(&device_of(&bob)));
      // A member, who knows the chat only, still passes over another chat's list.
      let mut by_chat = expect(&anchor, None);
      by_chat.maybe_meeting_id = None;
      let as_member = verify_chain(&[other_chat], &by_chat).unwrap();
      assert!(!as_member.hosts_device(&device_of(&bob)));
    }

    #[test]
    fn a_device_its_listed_key_did_not_certify_is_no_hosts() {
      let (alice, bob) = (TestUser::new(1), TestUser::new(2));
      let signed = draft(&alice, &alice, &[&bob], 1).sign(&alice.identity).unwrap();
      let verified = verify_chain(&[signed], &expect(&fingerprint(&alice), None)).unwrap();
      let fake_bob = TestUser::new(2);
      assert!(!verified.hosts_device(&device_of(&fake_bob)));
      assert!(!verified.is_host(UserId::new(5), &bob.identity.public_key()));
    }

    #[test]
    fn hand_made_lists_out_of_order_or_naming_the_organizer_are_refused() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let mut statement = draft(&alice, &alice, &[&bob, &carol], 1)
        .sign(&alice.identity)
        .unwrap()
        .statement;
      statement.co_hosts.reverse();
      assert!(matches!(
        resign(statement, &alice).check_shape(),
        Err(MeetingHostsError::NotCanonical)
      ));
      let mut statement = draft(&alice, &alice, &[&bob], 1)
        .sign(&alice.identity)
        .unwrap()
        .statement;
      statement.co_hosts.insert(0, host(&alice));
      assert!(matches!(
        resign(statement, &alice).check_shape(),
        Err(MeetingHostsError::NotCanonical)
      ));
      let many: Vec<Host> = (0..=MAX_CO_HOSTS as u64)
        .map(|id| Host {
          user_id: UserId::new(id + 10),
          identity_key: bob.identity.public_key(),
        })
        .collect();
      let mut too_many = draft(&alice, &alice, &[], 1);
      too_many.co_hosts = many;
      assert!(matches!(
        too_many.sign(&alice.identity),
        Err(MeetingHostsError::TooManyCoHosts(_))
      ));
    }

    #[test]
    fn garbage_oversized_bytes_and_long_chains_are_refused() {
      assert!(SignedMeetingHosts::from_bytes(&[1, 2, 3]).is_err());
      assert!(matches!(
        SignedMeetingHosts::from_bytes(&vec![0; MAX_ENCODED_BYTES + 1]),
        Err(MeetingHostsError::TooLarge(_))
      ));
      let alice = TestUser::new(1);
      let mut bytes = draft(&alice, &alice, &[], 1)
        .sign(&alice.identity)
        .unwrap()
        .to_bytes()
        .unwrap();
      bytes.push(0);
      assert!(SignedMeetingHosts::from_bytes(&bytes).is_err(), "trailing bytes");
      let chain: Vec<SignedMeetingHosts> = (1..=MAX_CHAIN as u64 + 1)
        .map(|sequence| {
          draft(&alice, &alice, &[], sequence)
            .sign(&alice.identity)
            .unwrap()
        })
        .collect();
      assert!(matches!(
        verify_chain(&chain, &expect(&fingerprint(&alice), None)),
        Err(MeetingHostsError::ChainTooLong(_))
      ));
    }
  }

  fn draft(signer: &TestUser, organizer: &TestUser, co_hosts: &[&TestUser], sequence: u64) -> HostsDraft {
    HostsDraft {
      meeting_id: MEETING,
      conversation_id: CHAT,
      sequence,
      signer_user_id: signer.user_id,
      organizer: host(organizer),
      co_hosts: co_hosts.iter().map(|user| host(user)).collect(),
    }
  }

  fn expect(anchor: &IdentityFingerprint, maybe_seen_sequence: Option<u64>) -> HostsExpectation<'_> {
    HostsExpectation {
      anchor,
      maybe_meeting_id: Some(MEETING),
      conversation_id: CHAT,
      maybe_seen_sequence,
    }
  }

  fn host(user: &TestUser) -> Host {
    Host {
      user_id: user.user_id,
      identity_key: user.identity.public_key(),
    }
  }

  fn fingerprint(user: &TestUser) -> IdentityFingerprint {
    user.identity.public_key().fingerprint()
  }

  fn device_of(user: &TestUser) -> VerifiedDevice {
    user.certificate.verify(&user.identity.public_key()).unwrap()
  }

  fn resign(statement: MeetingHosts, signer: &TestUser) -> SignedMeetingHosts {
    let signature = signer.identity.sign_labeled(
      domain::MEETING_HOSTS,
      &statement.tls_serialize_detached().unwrap(),
    );
    SignedMeetingHosts { statement, signature }
  }
}
