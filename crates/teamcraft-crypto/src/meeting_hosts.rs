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
//!
//! Since #1864 a list (version 2) also names the hash of the handover its signer took the meeting
//! over with (`previous`; none in the founder's first term), so the lists the server keeps (every
//! handover and the newest) link up into one chain, and a checker keeps the newest handover it
//! counted: a chain without it is a fork and refused. A former organizer who signs again after
//! handing the meeting on, with a server that withholds the handover, can't take the meeting back
//! from whoever saw the handover even with a higher sequence. Lists of version 1 (no link) still
//! verify as before.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use tls_codec::{
  DeserializeBytes as _, Serialize as _, Size as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};

use crate::domain;
use crate::identity::{
  Ed25519Signature, IdentityError, IdentityFingerprint, IdentityKeyPair, IdentityPublicKey, VerifiedDevice,
};
use crate::ids::{ConversationId, MeetingId, UserId};

/// At most this many co-hosts besides the organizer.
pub const MAX_CO_HOSTS: usize = 100;
/// An encoded list is never larger: the server refuses larger ones before reading them.
pub const MAX_ENCODED_BYTES: usize = 8 * 1024;
/// At most this many lists in a chain a checker reads: the handovers and the newest list. The
/// server refuses a handover beyond [`MAX_HANDOVERS`].
pub const MAX_HANDOVERS: usize = 32;
pub const MAX_CHAIN: usize = MAX_HANDOVERS + 1;

/// The hosts of one meeting from one sequence on, as the organizer of the time signs them. The
/// discriminant is the list's version byte: a version 1 list encodes byte for byte as before.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
#[repr(u8)]
enum MeetingHosts {
  /// Before #1864: no link to the lists before.
  #[tls_codec(discriminant = 1)]
  Unlinked { body: HostsBody },
  /// Names the handover the signer took the meeting over with (#1864).
  #[tls_codec(discriminant = 2)]
  Linked {
    body: HostsBody,
    /// The hash of the newest handover before this list in the chain: the one that made the
    /// signer organizer. `None` in the founder's first term, before any handover.
    previous: Option<HostsHash>,
  },
}

/// What every version of a list says.
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct HostsBody {
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

/// The hash of a host list (of what its signer signed, under a label of its own): what a later
/// list names as `previous` and what a checker keeps of the newest handover it counted.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct HostsHash([u8; 32]);

/// What the next list of a chain must name as `previous`, as far as the counted lists tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
  /// Nothing counted yet, or lists of version 1 only.
  Unknown,
  /// The newest handover before it; `None` in the founder's first term.
  Known(Option<HostsHash>),
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
  /// The handover the signer took the meeting over with, `None` in the founder's first term
  /// ([`previous_for_next`] finds it).
  pub maybe_previous: Option<HostsHash>,
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
  /// The newest sequence the checker accepted before for this chat from this anchor.
  pub maybe_seen_sequence: Option<u64>,
  /// The newest handover the checker counted before for this chat from this anchor
  /// ([`VerifiedMeetingHosts::maybe_handover`]): a chain without it is a fork.
  pub maybe_seen_handover: Option<HostsHash>,
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
  /// The newest handover counted.
  maybe_handover: Option<HostsHash>,
  /// What the next list must name as `previous`, where the counted lists tell.
  link: Link,
}

#[derive(Debug, thiserror::Error)]
pub enum MeetingHostsError {
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
  #[error("the host lists leave out handover {seen}, seen before: a fork")]
  Fork { seen: HostsHash },
  #[error("{0} co-hosts are more than allowed")]
  TooManyCoHosts(usize),
  #[error("the co-hosts are not sorted by user, name a user twice or name the organizer")]
  NotCanonical,
  #[error("the host list takes {0} bytes, more than allowed")]
  TooLarge(usize),
  #[error("{0} host lists are more than a chain holds")]
  ChainTooLong(usize),
  #[error("a host list hash is 64 hex digits")]
  InvalidHash,
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
    let statement = MeetingHosts::Linked {
      body: HostsBody {
        meeting_id: self.meeting_id,
        conversation_id: self.conversation_id,
        sequence: self.sequence,
        signer: Host {
          user_id: self.signer_user_id,
          identity_key: signer.public_key(),
        },
        organizer: self.organizer,
        co_hosts,
      },
      previous: self.maybe_previous,
    };
    let signature = signer.sign_labeled(domain::MEETING_HOSTS, &statement.tls_serialize_detached()?);
    let signed = SignedMeetingHosts { statement, signature };
    signed.check_size()?;
    Ok(signed)
  }
}

impl MeetingHosts {
  fn body(&self) -> &HostsBody {
    match self {
      Self::Unlinked { body } | Self::Linked { body, .. } => body,
    }
  }

  /// The `previous` a version 2 list names; `None` for version 1.
  fn link(&self) -> Link {
    match self {
      Self::Unlinked { .. } => Link::Unknown,
      Self::Linked { previous, .. } => Link::Known(*previous),
    }
  }

  fn is_handover(&self) -> bool {
    let body = self.body();
    body.signer.identity_key != body.organizer.identity_key || body.signer.user_id != body.organizer.user_id
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

  /// The list's hash: over what its signer signed (version byte included), so the same list has
  /// the same hash however it was carried.
  pub fn hash(&self) -> Result<HostsHash, MeetingHostsError> {
    let encoded = self.statement.tls_serialize_detached()?;
    Ok(HostsHash(
      Sha256::digest(domain::labeled(domain::MEETING_HOSTS_HASH, &encoded)).into(),
    ))
  }

  /// The claimed meeting, before verification (the server files the list under it).
  pub fn unverified_meeting_id(&self) -> MeetingId {
    self.statement.body().meeting_id
  }

  pub fn unverified_conversation_id(&self) -> ConversationId {
    self.statement.body().conversation_id
  }

  pub fn unverified_sequence(&self) -> u64 {
    self.statement.body().sequence
  }

  /// The signer's user as the list claims it. The server checks it is the caller.
  pub fn unverified_signer(&self) -> UserId {
    self.statement.body().signer.user_id
  }

  /// The organizer from this list on, as it claims.
  pub fn unverified_organizer(&self) -> UserId {
    self.statement.body().organizer.user_id
  }

  /// The co-hosts' users, as the list claims them.
  pub fn unverified_co_hosts(&self) -> Vec<UserId> {
    self
      .statement
      .body()
      .co_hosts
      .iter()
      .map(|host| host.user_id)
      .collect()
  }

  /// The handover the list says its signer took the meeting over with: `None` for a list of
  /// version 1 or of the founder's first term.
  pub fn unverified_previous(&self) -> Option<HostsHash> {
    match self.statement.link() {
      Link::Known(maybe_previous) => maybe_previous,
      Link::Unknown => None,
    }
  }

  /// Whether the list hands the meeting to somebody else: the server keeps every one of these, so
  /// a checker who trusts an older organizer follows the chain.
  pub fn is_handover(&self) -> bool {
    self.statement.is_handover()
  }

  /// Whether the list is well formed and signed by the key it names. The server's sanity check: it
  /// says nothing about whether that key is the organizer's.
  pub fn check_shape(&self) -> Result<(), MeetingHostsError> {
    self.check_size()?;
    self.check_signed_by_named_key()
  }

  fn check_binding(&self, expected: &HostsExpectation<'_>) -> Result<(), MeetingHostsError> {
    let body = self.statement.body();
    if let Some(meeting_id) = expected.maybe_meeting_id
      && body.meeting_id != meeting_id
    {
      return Err(MeetingHostsError::WrongMeeting {
        expected: meeting_id,
        actual: body.meeting_id,
      });
    }
    if body.conversation_id != expected.conversation_id {
      return Err(MeetingHostsError::WrongConversation {
        expected: expected.conversation_id,
        actual: body.conversation_id,
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
    let body = self.statement.body();
    body
      .signer
      .identity_key
      .verify_labeled(
        domain::MEETING_HOSTS,
        &self.statement.tls_serialize_detached()?,
        &self.signature,
      )
      .map_err(MeetingHostsError::InvalidSignature)?;
    if body.co_hosts.len() > MAX_CO_HOSTS {
      return Err(MeetingHostsError::TooManyCoHosts(body.co_hosts.len()));
    }
    let is_canonical = body
      .co_hosts
      .windows(2)
      .all(|pair| pair[0].user_id < pair[1].user_id)
      && body
        .co_hosts
        .iter()
        .all(|host| host.user_id != body.organizer.user_id);
    if !is_canonical {
      return Err(MeetingHostsError::NotCanonical);
    }
    Ok(())
  }
}

/// Follows the host lists of a meeting chat from the key the checker trusts: in the order of their
/// sequence, a list counts only if the organizer the counted list before it named (at first the
/// anchor) signed it, for this chat (and meeting, where known), and, for a list of version 2, if it
/// links to the counted ones: it names as `previous` the newest handover counted before it (the
/// first list counted from the anchor may name any, since the chain may have begun before the
/// anchor's term). Each counted list names the organizer whose key signs the next. Lists that don't
/// verify, that somebody else signed or that link elsewhere are passed over, so a server can't add
/// a host and a former organizer can't take the meeting back. Without a list from the anchor on,
/// the anchor alone hosts (sequence 0). A newest counted list older than one the checker saw is a
/// rollback; a chain that doesn't count the newest handover the checker counted before is a fork.
pub fn verify_chain(
  statements: &[SignedMeetingHosts],
  expected: &HostsExpectation<'_>,
) -> Result<VerifiedMeetingHosts, MeetingHostsError> {
  if statements.len() > MAX_CHAIN {
    return Err(MeetingHostsError::ChainTooLong(statements.len()));
  }
  let mut ordered: Vec<&SignedMeetingHosts> = statements.iter().collect();
  ordered.sort_by_key(|signed| signed.unverified_sequence());
  let mut trusted = *expected.anchor;
  let mut maybe_current: Option<&HostsBody> = None;
  let mut link = Link::Unknown;
  let mut handovers: Vec<HostsHash> = Vec::new();
  for signed in ordered {
    let body = signed.statement.body();
    if maybe_current.is_some_and(|current| body.sequence <= current.sequence) {
      continue;
    }
    if body.signer.identity_key.fingerprint() != trusted {
      continue;
    }
    if signed.check_signed_by_named_key().is_err() || signed.check_binding(expected).is_err() {
      continue;
    }
    let own_link = signed.statement.link();
    if let (Link::Known(previous), Link::Known(expected_previous)) = (own_link, link)
      && previous != expected_previous
    {
      continue;
    }
    let Ok(hash) = signed.hash() else {
      continue;
    };
    if signed.statement.is_handover() {
      handovers.push(hash);
      link = Link::Known(Some(hash));
    } else if own_link != Link::Unknown {
      link = own_link;
    }
    trusted = body.organizer.identity_key.fingerprint();
    maybe_current = Some(body);
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
  if let Some(seen) = expected.maybe_seen_handover
    && !handovers.contains(&seen)
  {
    return Err(MeetingHostsError::Fork { seen });
  }
  let maybe_handover = handovers.last().copied();
  Ok(match maybe_current {
    Some(current) => VerifiedMeetingHosts {
      sequence,
      organizer_fingerprint: trusted,
      maybe_organizer: Some(current.organizer),
      co_hosts: current.co_hosts.clone(),
      is_handover_pending: current.signer.identity_key != current.organizer.identity_key,
      maybe_handover,
      link,
    },
    None => VerifiedMeetingHosts {
      sequence,
      organizer_fingerprint: trusted,
      maybe_organizer: None,
      co_hosts: Vec::new(),
      is_handover_pending: false,
      maybe_handover,
      link,
    },
  })
}

/// What the next list `signer` signs names as `previous`: where the chain from the signer's own key
/// tells (a handover back to the signer, the signer's own lists of version 2), what it links to;
/// otherwise the newest handover among `statements` that names the signer's key as organizer (the
/// one that made them organizer), and none without one (the founder's first term). A wrong one
/// costs availability only: whoever follows the chain passes the list over.
pub fn previous_for_next(
  statements: &[SignedMeetingHosts],
  signer: &IdentityPublicKey,
  maybe_meeting_id: Option<MeetingId>,
  conversation_id: ConversationId,
) -> Option<HostsHash> {
  let own = signer.fingerprint();
  let expectation = HostsExpectation {
    anchor: &own,
    maybe_meeting_id,
    conversation_id,
    maybe_seen_sequence: None,
    maybe_seen_handover: None,
  };
  if let Ok(verified) = verify_chain(statements, &expectation)
    && verified.organizer_fingerprint == own
    && let Link::Known(maybe_previous) = verified.link
  {
    return maybe_previous;
  }
  statements
    .iter()
    .filter(|signed| {
      signed.is_handover()
        && signed.statement.body().organizer.identity_key == *signer
        && signed.check_shape().is_ok()
        && signed.check_binding(&expectation).is_ok()
    })
    .max_by_key(|signed| signed.unverified_sequence())
    .and_then(|signed| signed.hash().ok())
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

  /// The newest handover counted from the anchor: what the checker keeps, so that a later chain
  /// without it is refused as a fork ([`HostsExpectation::maybe_seen_handover`]).
  pub fn maybe_handover(&self) -> Option<HostsHash> {
    self.maybe_handover
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

impl HostsHash {
  pub const fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub const fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }
}

impl fmt::Display for HostsHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    Ok(())
  }
}

impl fmt::Debug for HostsHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "HostsHash({self})")
  }
}

/// The hash from its hex form (64 digits, either case), as a device keeps it.
impl std::str::FromStr for HostsHash {
  type Err = MeetingHostsError;

  fn from_str(hex: &str) -> Result<Self, Self::Err> {
    let digits = hex.as_bytes();
    if digits.len() != 64 {
      return Err(MeetingHostsError::InvalidHash);
    }
    let mut bytes = [0u8; 32];
    for (byte, pair) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
      let pair = std::str::from_utf8(pair.as_slice()).map_err(|_| MeetingHostsError::InvalidHash)?;
      *byte = u8::from_str_radix(pair, 16).map_err(|_| MeetingHostsError::InvalidHash)?;
    }
    Ok(Self(bytes))
  }
}

impl Serialize for HostsHash {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for HostsHash {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    text
      .parse()
      .map_err(|_| serde::de::Error::custom("expected 64 hex digits"))
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
    assert_eq!(decoded.hash().unwrap(), signed.hash().unwrap());

    let verified = verify_chain(&[decoded], &expect(&fingerprint(&alice), None)).unwrap();

    assert_eq!(verified.sequence(), 5);
    assert!(verified.hosts_device(&device_of(&bob)));
    assert!(verified.hosts_device(&device_of(&alice)), "the organizer hosts");
    assert!(!verified.is_handover_pending());
    assert!(verified.has_same_hosts(&host(&alice), &[host(&bob), host(&alice)]));
    assert_eq!(verified.maybe_handover(), None);
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

  #[test]
  fn a_hash_reads_back_from_its_hex_form() {
    let alice = TestUser::new(1);
    let hash = draft(&alice, &alice, &[], 1)
      .sign(&alice.identity)
      .unwrap()
      .hash()
      .unwrap();
    assert_eq!(hash.to_string().parse::<HostsHash>().unwrap(), hash);
    assert_eq!(
      hash.to_string().to_uppercase().parse::<HostsHash>().unwrap(),
      hash
    );
    assert!("00".parse::<HostsHash>().is_err());
    assert!("zz".repeat(32).parse::<HostsHash>().is_err());
    assert!("é".repeat(32).parse::<HostsHash>().is_err());
  }

  /// Lists signed before #1864 (version 1, no link) encode, verify and chain as before.
  mod version_1 {
    use super::*;

    /// How a list was encoded before #1864.
    #[derive(TlsSerialize, TlsSize)]
    struct LegacyMeetingHosts {
      version: u8,
      meeting_id: MeetingId,
      conversation_id: ConversationId,
      sequence: u64,
      signer: Host,
      organizer: Host,
      co_hosts: Vec<Host>,
    }

    #[test]
    fn a_list_from_before_decodes_byte_for_byte_and_verifies() {
      let (alice, bob) = (TestUser::new(1), TestUser::new(2));
      let legacy = LegacyMeetingHosts {
        version: 1,
        meeting_id: MEETING,
        conversation_id: CHAT,
        sequence: 3,
        signer: host(&alice),
        organizer: host(&alice),
        co_hosts: vec![host(&bob)],
      };
      let legacy_bytes = legacy.tls_serialize_detached().unwrap();
      let signature = alice.identity.sign_labeled(domain::MEETING_HOSTS, &legacy_bytes);
      let mut encoded = legacy_bytes.clone();
      encoded.extend(signature.tls_serialize_detached().unwrap());

      let decoded = SignedMeetingHosts::from_bytes(&encoded).unwrap();

      assert!(matches!(decoded.statement, MeetingHosts::Unlinked { .. }));
      assert_eq!(decoded.to_bytes().unwrap(), encoded, "re-encodes as it came");
      assert!(decoded.check_shape().is_ok());
      assert_eq!(decoded.unverified_previous(), None);
      let verified = verify_chain(&[decoded], &expect(&fingerprint(&alice), None)).unwrap();
      assert_eq!(verified.sequence(), 3);
      assert!(verified.hosts_device(&device_of(&bob)));
    }

    #[test]
    fn a_version_1_handover_is_followed_and_kept_like_one_of_version_2() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let handover = sign_v1(draft(&alice, &bob, &[&alice], 1), &alice);
      // Bob's app links his list to Alice's handover of version 1.
      let mut own = draft(&bob, &bob, &[&carol], 2);
      own.maybe_previous = Some(handover.hash().unwrap());
      let own = own.sign(&bob.identity).unwrap();

      let verified = verify_chain(&[handover.clone(), own], &expect(&fingerprint(&alice), None)).unwrap();

      assert_eq!(verified.sequence(), 2);
      assert!(verified.hosts_device(&device_of(&carol)));
      assert_eq!(verified.maybe_handover(), Some(handover.hash().unwrap()));
    }

    #[test]
    fn an_unknown_version_is_unreadable() {
      let alice = TestUser::new(1);
      let mut bytes = draft(&alice, &alice, &[], 1)
        .sign(&alice.identity)
        .unwrap()
        .to_bytes()
        .unwrap();
      bytes[0] = 3;
      assert!(SignedMeetingHosts::from_bytes(&bytes).is_err());
    }
  }

  mod handover {
    use super::*;

    /// Alice names Bob co-host, hands the meeting to Carol (staying co-host), and Carol signs her
    /// own list without Bob.
    fn history(alice: &TestUser, bob: &TestUser, carol: &TestUser) -> Vec<SignedMeetingHosts> {
      let to_carol = draft(alice, carol, &[bob, alice], 2)
        .sign(&alice.identity)
        .unwrap();
      vec![
        draft(alice, alice, &[bob], 1).sign(&alice.identity).unwrap(),
        to_carol.clone(),
        linked(draft(carol, carol, &[alice], 3), &to_carol)
          .sign(&carol.identity)
          .unwrap(),
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
      assert_eq!(verified.maybe_handover(), Some(chain[1].hash().unwrap()));
    }

    #[test]
    fn a_link_with_the_new_organizers_key_starts_at_her_lists() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let chain = history(&alice, &bob, &carol);

      let verified = verify_chain(&chain, &expect(&fingerprint(&carol), None)).unwrap();

      assert_eq!(verified.sequence(), 3);
      assert!(verified.hosts_device(&device_of(&alice)));
      assert_eq!(verified.maybe_handover(), None, "no handover of hers yet");
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
      let back = linked(draft(&carol, &alice, &[&carol], 4), &chain[1])
        .sign(&carol.identity)
        .unwrap();
      chain.push(back.clone());
      chain.push(
        linked(draft(&alice, &alice, &[&bob], 5), &back)
          .sign(&alice.identity)
          .unwrap(),
      );

      let verified = verify_chain(&chain, &expect(&fingerprint(&carol), None)).unwrap();

      assert_eq!(verified.sequence(), 5);
      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&alice));
      assert!(verified.hosts_device(&device_of(&bob)));
      assert!(!verified.hosts_device(&device_of(&carol)));
      assert_eq!(verified.maybe_handover(), Some(back.hash().unwrap()));
    }
  }

  /// A former organizer signing again after the handover, with a server that withholds it (#1864).
  mod forks {
    use super::*;

    /// Alice hands the meeting to Bob, Bob to Carol, and Carol signs her own list.
    struct History {
      to_bob: SignedMeetingHosts,
      to_carol: SignedMeetingHosts,
      carols: SignedMeetingHosts,
    }

    fn history(alice: &TestUser, bob: &TestUser, carol: &TestUser) -> History {
      let to_bob = draft(alice, bob, &[], 1).sign(&alice.identity).unwrap();
      let to_carol = linked(draft(bob, carol, &[], 2), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      let carols = linked(draft(carol, carol, &[], 3), &to_carol)
        .sign(&carol.identity)
        .unwrap();
      History {
        to_bob,
        to_carol,
        carols,
      }
    }

    #[test]
    fn a_former_organizers_fork_is_refused_by_whoever_saw_the_handover() {
      let (alice, bob, carol, mallory) = (
        TestUser::new(1),
        TestUser::new(2),
        TestUser::new(3),
        TestUser::new(66),
      );
      let history = history(&alice, &bob, &carol);
      let anchor = fingerprint(&alice);
      let seen = verify_chain(
        &[
          history.to_bob.clone(),
          history.to_carol.clone(),
          history.carols.clone(),
        ],
        &expect(&anchor, None),
      )
      .unwrap();
      assert_eq!(seen.maybe_handover(), Some(history.to_carol.hash().unwrap()));
      // Bob signs a list above Carol's naming himself organizer again, linked to Alice's handover
      // to him; the server withholds his handover to Carol.
      let fork = linked(draft(&bob, &bob, &[&mallory], 4), &history.to_bob)
        .sign(&bob.identity)
        .unwrap();
      let forked = [history.to_bob.clone(), fork.clone()];

      // The sequence alone doesn't tell: the fork is above the newest seen.
      let by_sequence = verify_chain(&forked, &expect(&anchor, Some(seen.sequence()))).unwrap();
      assert!(by_sequence.hosts_device(&device_of(&mallory)));

      let mut checked = expect(&anchor, Some(seen.sequence()));
      checked.maybe_seen_handover = seen.maybe_handover();
      assert!(matches!(
        verify_chain(&forked, &checked),
        Err(MeetingHostsError::Fork { seen }) if seen == history.to_carol.hash().unwrap()
      ));
      // Served with the handover, the fork is passed over: Bob no longer signs.
      let verified = verify_chain(
        &[history.to_bob, history.to_carol, history.carols, fork],
        &checked,
      )
      .unwrap();
      assert_eq!(*verified.organizer_fingerprint(), fingerprint(&carol));
      assert!(!verified.hosts_device(&device_of(&mallory)));
    }

    #[test]
    fn a_list_linked_to_another_handover_is_passed_over() {
      let (alice, bob, carol) = (TestUser::new(1), TestUser::new(2), TestUser::new(3));
      let history = history(&alice, &bob, &carol);
      // Carol's list naming Alice's handover instead of Bob's.
      let elsewhere = linked(draft(&carol, &carol, &[&alice], 4), &history.to_bob)
        .sign(&carol.identity)
        .unwrap();

      let verified = verify_chain(
        &[history.to_bob, history.to_carol, history.carols, elsewhere],
        &expect(&fingerprint(&alice), None),
      )
      .unwrap();

      assert_eq!(verified.sequence(), 3);
      assert!(!verified.hosts_device(&device_of(&alice)));
    }

    /// The server keeps every handover and the newest list only: a device that kept a handover
    /// follows later ones, from the founder's key and from a later organizer's alike.
    #[test]
    fn honest_handovers_keep_verifying_on_every_device() {
      let (alice, bob, carol, dave) = (
        TestUser::new(1),
        TestUser::new(2),
        TestUser::new(3),
        TestUser::new(4),
      );
      let to_bob = draft(&alice, &bob, &[], 1).sign(&alice.identity).unwrap();
      // A member's device (from Alice's key) sees the first handover.
      let member_anchor = fingerprint(&alice);
      let member = verify_chain(std::slice::from_ref(&to_bob), &expect(&member_anchor, None)).unwrap();
      assert_eq!(member.maybe_handover(), Some(to_bob.hash().unwrap()));
      // Bob names Carol co-host and then hands the meeting to her; the server drops his list.
      let bobs = linked(draft(&bob, &bob, &[&carol], 2), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      assert!(verify_chain(&[to_bob.clone(), bobs], &seen(&member_anchor, &member)).is_ok());
      let to_carol = linked(draft(&bob, &carol, &[&bob], 3), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      let carols = linked(draft(&carol, &carol, &[&bob], 4), &to_carol)
        .sign(&carol.identity)
        .unwrap();
      let served = [to_bob.clone(), to_carol.clone(), carols];
      let member = verify_chain(&served, &seen(&member_anchor, &member)).unwrap();
      assert_eq!(member.maybe_handover(), Some(to_carol.hash().unwrap()));
      // A guest with a link of Carol's: no handover of hers yet.
      let guest_anchor = fingerprint(&carol);
      let guest = verify_chain(&served, &expect(&guest_anchor, None)).unwrap();
      assert_eq!(guest.sequence(), 4);
      assert_eq!(guest.maybe_handover(), None);

      // Carol hands the meeting to Dave, who signs his own list; Carol's list is dropped.
      let to_dave = linked(draft(&carol, &dave, &[&carol], 5), &to_carol)
        .sign(&carol.identity)
        .unwrap();
      let daves = linked(draft(&dave, &dave, &[&carol], 6), &to_dave)
        .sign(&dave.identity)
        .unwrap();
      let served = [to_bob, to_carol, to_dave.clone(), daves];

      let member = verify_chain(&served, &seen(&member_anchor, &member)).unwrap();
      let guest = verify_chain(&served, &seen(&guest_anchor, &guest)).unwrap();

      for verified in [&member, &guest] {
        assert_eq!(verified.sequence(), 6);
        assert_eq!(*verified.organizer_fingerprint(), fingerprint(&dave));
        assert_eq!(verified.maybe_handover(), Some(to_dave.hash().unwrap()));
        assert!(verified.hosts_device(&device_of(&carol)));
      }
    }

    #[test]
    fn the_next_list_names_the_handover_its_signer_took_over_with() {
      let (alice, bob) = (TestUser::new(1), TestUser::new(2));
      let previous = |statements: &[SignedMeetingHosts], signer: &TestUser| {
        previous_for_next(statements, &signer.identity.public_key(), Some(MEETING), CHAT)
      };
      // The founder's first term: none.
      assert_eq!(previous(&[], &alice), None);
      let alices = draft(&alice, &alice, &[&bob], 1).sign(&alice.identity).unwrap();
      assert_eq!(previous(std::slice::from_ref(&alices), &alice), None);
      // Bob, handed the meeting, before and after a list of his own.
      let to_bob = draft(&alice, &bob, &[], 2).sign(&alice.identity).unwrap();
      let to_bob_hash = to_bob.hash().unwrap();
      assert_eq!(
        previous(&[alices.clone(), to_bob.clone()], &bob),
        Some(to_bob_hash)
      );
      let bobs = linked(draft(&bob, &bob, &[], 3), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      assert_eq!(
        previous(&[alices.clone(), to_bob.clone(), bobs.clone()], &bob),
        Some(to_bob_hash)
      );
      // Alice, handed the meeting back.
      let back = linked(draft(&bob, &alice, &[], 4), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      assert_eq!(
        previous(&[alices, to_bob, bobs, back.clone()], &alice),
        Some(back.hash().unwrap())
      );
    }

    fn seen<'a>(anchor: &'a IdentityFingerprint, before: &VerifiedMeetingHosts) -> HostsExpectation<'a> {
      HostsExpectation {
        maybe_seen_handover: before.maybe_handover(),
        ..expect(anchor, Some(before.sequence()))
      }
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
      body_mut(&mut signed.statement).co_hosts.push(host(&mallory));
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
    fn a_changed_link_breaks_the_signature() {
      let (alice, bob) = (TestUser::new(1), TestUser::new(2));
      let to_bob = draft(&alice, &bob, &[], 1).sign(&alice.identity).unwrap();
      let mut signed = linked(draft(&bob, &bob, &[], 2), &to_bob)
        .sign(&bob.identity)
        .unwrap();
      if let MeetingHosts::Linked { previous, .. } = &mut signed.statement {
        *previous = None;
      }
      assert!(matches!(
        signed.check_shape(),
        Err(MeetingHostsError::InvalidSignature(_))
      ));
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
      body_mut(&mut statement).co_hosts.reverse();
      assert!(matches!(
        resign(statement, &alice).check_shape(),
        Err(MeetingHostsError::NotCanonical)
      ));
      let mut statement = draft(&alice, &alice, &[&bob], 1)
        .sign(&alice.identity)
        .unwrap()
        .statement;
      body_mut(&mut statement).co_hosts.insert(0, host(&alice));
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
      maybe_previous: None,
    }
  }

  /// `draft` naming `handover` as the one its signer took the meeting over with.
  fn linked(draft: HostsDraft, handover: &SignedMeetingHosts) -> HostsDraft {
    HostsDraft {
      maybe_previous: Some(handover.hash().unwrap()),
      ..draft
    }
  }

  fn expect(anchor: &IdentityFingerprint, maybe_seen_sequence: Option<u64>) -> HostsExpectation<'_> {
    HostsExpectation {
      anchor,
      maybe_meeting_id: Some(MEETING),
      conversation_id: CHAT,
      maybe_seen_sequence,
      maybe_seen_handover: None,
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

  fn body_mut(statement: &mut MeetingHosts) -> &mut HostsBody {
    match statement {
      MeetingHosts::Unlinked { body } | MeetingHosts::Linked { body, .. } => body,
    }
  }

  /// The list as a device from before #1864 signs it (version 1).
  fn sign_v1(draft: HostsDraft, signer: &TestUser) -> SignedMeetingHosts {
    let statement = MeetingHosts::Unlinked {
      body: HostsBody {
        meeting_id: draft.meeting_id,
        conversation_id: draft.conversation_id,
        sequence: draft.sequence,
        signer: host(signer),
        organizer: draft.organizer,
        co_hosts: draft.co_hosts,
      },
    };
    resign(statement, signer)
  }

  fn resign(statement: MeetingHosts, signer: &TestUser) -> SignedMeetingHosts {
    let signature = signer.identity.sign_labeled(
      domain::MEETING_HOSTS,
      &statement.tls_serialize_detached().unwrap(),
    );
    SignedMeetingHosts { statement, signature }
  }
}
