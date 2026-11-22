//! Direct media for 1:1 calls (issue #127, ARCHITECTURE.md §6.7): the client's decisions, without
//! I/O. Whether this device may go direct, which candidates it offers, how it reads the other side's
//! offer, in which order it dials, and when the media moves to the direct path and back.
//!
//! The call engine owns the sockets and feeds this module what happened (a connection came up, a
//! ping was answered or lost, something arrived); it asks [`PathSelector`] where to send. Media on
//! either path is the same DAVE-encrypted datagram.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use rand_core::CryptoRngCore;
use teamcraft_crypto::call_binding::CallId as BindingCallId;
use teamcraft_crypto::direct_path::{DirectPathError, DirectPathKey, OfferContext, sealed_epoch};
use teamcraft_crypto::ids::DeviceId as BindingDeviceId;
use teamcraft_proto::{
  Base64Bytes, CallId, CallPeerStanding, DeviceId, DirectCandidate, DirectCandidateKind, DirectMediaPolicy,
  DirectOfferBody, Id, MAX_DIRECT_CANDIDATES, MAX_DIRECT_OFFER_BYTES,
};
use thiserror::Error;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// How often the direct path is pinged while it is tried or used.
pub const PING_INTERVAL: Duration = Duration::from_millis(200);
/// How long the direct path must answer every ping before it takes the media.
pub const UP_AFTER: Duration = Duration::from_secs(1);
/// Lost pings in a row that send the media back to the SFU.
pub const MAX_LOST_PINGS: u32 = 3;
/// Silence on the direct path, pings included, that sends the media back to the SFU.
pub const MAX_SILENCE: Duration = Duration::from_millis(600);
/// How long both paths carry the media after a switch, so the receiver misses nothing.
pub const SWITCH_OVERLAP: Duration = Duration::from_secs(1);
/// How long a round of dialing lasts before the pair stays on the SFU until the next try.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);
/// Between dialing two candidate pairs.
pub const DIAL_STEP: Duration = Duration::from_millis(50);
/// The wait after the first failed round; it doubles up to [`MAX_RETRY_WAIT`].
pub const FIRST_RETRY_WAIT: Duration = Duration::from_secs(2);
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
/// The length of a certificate hash in an offer (SHA-256).
pub const CERTIFICATE_HASH_LEN: usize = 32;

/// What this device knows when it decides whether to go direct.
#[derive(Debug, Clone, Copy)]
pub struct DirectPathInputs<'a> {
  /// The workspace policy this device verified.
  pub policy: &'a DirectMediaPolicy,
  /// This device's user allows direct connections (`calls.direct_connections`).
  pub does_user_allow: bool,
  pub own: CallPeerStanding,
  /// The other device's standing, once this device verified its key binding; `None` until then.
  pub maybe_peer: Option<CallPeerStanding>,
  /// Devices in the call, this one included.
  pub devices_in_call: usize,
}

/// Why a call stays on the SFU path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotDirect {
  /// The workspace policy doesn't allow it for these two.
  Policy,
  /// This device's user turned it off.
  UserSetting,
  /// The call doesn't have exactly two devices.
  NotOneToOne,
  /// The other device isn't verified yet.
  PeerUnverified,
}

/// An offer from the other device that this device can't use.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OfferError {
  #[error("the sealed offer is too large: {0} bytes")]
  TooLarge(usize),
  #[error(transparent)]
  Sealed(#[from] DirectPathError),
  #[error("the offer isn't a valid offer body")]
  Malformed,
  #[error("the offer names {0} candidates, at most {MAX_DIRECT_CANDIDATES} are allowed")]
  TooManyCandidates(usize),
  #[error("the offer's certificate hash has {0} bytes")]
  CertificateHash(usize),
  #[error("the offer names an address nobody can be reached at")]
  UnusableAddress,
  #[error("an offer of this epoch with counter {held} or higher was already taken")]
  Stale { held: u64 },
}

/// Who this device is in the call and who the other device is: the context of every offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectPair {
  pub call_id: CallId,
  pub own: DeviceId,
  pub peer: DeviceId,
}

/// The other device's offers this device took, for one DAVE epoch: only a higher counter counts.
#[derive(Debug, Default)]
pub struct OfferInbox {
  maybe_latest: Option<(u64, u64)>,
}

/// Where the media goes now, and what the engine should tell the SFU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathState {
  /// Only the SFU path is in use (no direct path, or it is being tried).
  Relay,
  /// The direct path carries the media; the SFU's forwarding to this device may stop once the
  /// other device says the same.
  Direct,
}

/// A change of [`PathState`], to report to the SFU (`direct_path`) and log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChange {
  Up,
  Down(DownReason),
}

/// Why the direct path stopped carrying the media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownReason {
  LostPings,
  Silence,
  /// The connection closed or failed, or the network changed.
  Closed,
  /// The call is no longer 1:1, or the policy or the setting changed.
  NotAllowed,
}

/// Decides, from what happens on the direct path, where the media goes (ARCHITECTURE.md §6.7,
/// "Switching paths"): up after [`UP_AFTER`] of answered pings, down at once on trouble, with
/// [`SWITCH_OVERLAP`] on both paths after each switch.
#[derive(Debug)]
pub struct PathSelector {
  phase: Phase,
  maybe_overlap_until: Option<Instant>,
  failed_rounds: u32,
  maybe_retry_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
enum Phase {
  /// No direct connection.
  Idle,
  /// Dialing since then; no connection yet.
  Dialing { since: Instant },
  /// A connection, proving itself: answered without loss since `clean_since`.
  Proving {
    clean_since: Instant,
    last_heard: Instant,
    lost_in_row: u32,
  },
  /// The direct path carries the media.
  Up { last_heard: Instant, lost_in_row: u32 },
}

/// Whether this device may go direct now (ARCHITECTURE.md §6.7, "Who may" and "When"). One "no" of
/// either device is enough: the other device decides the same for itself.
pub fn eligibility(inputs: &DirectPathInputs<'_>) -> Result<(), NotDirect> {
  if inputs.devices_in_call != 2 {
    return Err(NotDirect::NotOneToOne);
  }
  if !inputs.does_user_allow {
    return Err(NotDirect::UserSetting);
  }
  let Some(peer) = inputs.maybe_peer else {
    return Err(NotDirect::PeerUnverified);
  };
  if !inputs.policy.allows(inputs.own, peer) {
    return Err(NotDirect::Policy);
  }
  Ok(())
}

/// The candidates of this device's direct path socket: its interfaces' addresses with the socket's
/// port, the address the SFU saw, and a mapped port, usable ones only, without duplicates, IPv6 and
/// host candidates first, at most [`MAX_DIRECT_CANDIDATES`].
pub fn own_candidates(
  interfaces: impl IntoIterator<Item = IpAddr>,
  port: u16,
  maybe_reflexive: Option<SocketAddr>,
  maybe_mapped: Option<SocketAddr>,
) -> Vec<DirectCandidate> {
  let hosts = interfaces.into_iter().map(|ip| DirectCandidate {
    kind: DirectCandidateKind::Host,
    address: SocketAddr::new(ip, port),
  });
  let found = maybe_mapped
    .map(|address| DirectCandidate {
      kind: DirectCandidateKind::Mapped,
      address,
    })
    .into_iter()
    .chain(maybe_reflexive.map(|address| DirectCandidate {
      kind: DirectCandidateKind::Reflexive,
      address,
    }));
  let mut candidates: Vec<DirectCandidate> = Vec::new();
  for candidate in hosts.chain(found) {
    let is_new = !candidates.iter().any(|known| known.address == candidate.address);
    if is_usable(candidate.address) && is_new {
      candidates.push(candidate);
    }
  }
  sort_for_dialing(&mut candidates);
  candidates.truncate(MAX_DIRECT_CANDIDATES);
  candidates
}

/// Seals this device's offer for the other device.
pub fn seal_offer(
  key: &DirectPathKey,
  pair: &DirectPair,
  body: &DirectOfferBody,
  rng: &mut (impl CryptoRngCore + ?Sized),
) -> Result<Base64Bytes, OfferError> {
  // Invariant: serializing a struct of numbers, bytes and addresses can't fail.
  let plaintext = serde_json::to_vec(body).map_err(|_| OfferError::Malformed)?;
  let sealed = key.seal(&offer_context(pair.call_id, pair.own, pair.peer), &plaintext, rng)?;
  if sealed.len() > MAX_DIRECT_OFFER_BYTES {
    return Err(OfferError::TooLarge(sealed.len()));
  }
  Ok(Base64Bytes::new(sealed))
}

/// Opens and checks the other device's offer: sealed for this epoch, from the other device to this
/// one, well formed, and newer than the last one taken.
pub fn open_offer(
  key: &DirectPathKey,
  pair: &DirectPair,
  inbox: &mut OfferInbox,
  sealed: &[u8],
) -> Result<DirectOfferBody, OfferError> {
  if sealed.len() > MAX_DIRECT_OFFER_BYTES {
    return Err(OfferError::TooLarge(sealed.len()));
  }
  let plaintext = key.open(&offer_context(pair.call_id, pair.peer, pair.own), sealed)?;
  let mut body: DirectOfferBody =
    serde_json::from_slice(plaintext.expose_secret()).map_err(|_| OfferError::Malformed)?;
  check_body(&body)?;
  inbox.take(key.epoch(), body.counter)?;
  // Kinds from a newer app are dialed like reflexive ones; the order is this device's.
  sort_for_dialing(&mut body.candidates);
  Ok(body)
}

/// The epoch an offer is sealed for: one of another epoch than this device's waits for the next.
pub fn offer_epoch(sealed: &[u8]) -> Result<u64, OfferError> {
  Ok(sealed_epoch(sealed)?)
}

/// Whether this device's accepted connection wins when both directions connect: the device with
/// the lower ID accepts the winning one, so both pick the same connection.
pub fn accepted_connection_wins(pair: &DirectPair) -> bool {
  raw(pair.own) < raw(pair.peer)
}

/// When the `index`th candidate pair of a dialing round is dialed, from the round's start.
pub fn dial_delay(index: usize) -> Duration {
  DIAL_STEP.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX))
}

impl OfferInbox {
  /// Takes `counter` of `epoch` if it is newer than what was taken; a new epoch starts over.
  fn take(&mut self, epoch: u64, counter: u64) -> Result<(), OfferError> {
    match self.maybe_latest {
      Some((held_epoch, held)) if held_epoch == epoch && counter <= held => Err(OfferError::Stale { held }),
      _ => {
        self.maybe_latest = Some((epoch, counter));
        Ok(())
      },
    }
  }
}

impl Default for PathSelector {
  fn default() -> Self {
    Self::new()
  }
}

impl PathSelector {
  pub fn new() -> Self {
    Self {
      phase: Phase::Idle,
      maybe_overlap_until: None,
      failed_rounds: 0,
      maybe_retry_at: None,
    }
  }

  /// Where the media goes now.
  pub fn state(&self) -> PathState {
    match self.phase {
      Phase::Up { .. } => PathState::Direct,
      Phase::Idle | Phase::Dialing { .. } | Phase::Proving { .. } => PathState::Relay,
    }
  }

  /// Whether the media goes to the SFU now: always but while the direct path carries it, and for
  /// [`SWITCH_OVERLAP`] after the switch.
  pub fn sends_on_relay(&self, now: Instant) -> bool {
    self.state() == PathState::Relay || self.is_overlapping(now)
  }

  /// Whether the media goes on the direct path now: while it carries it, and for
  /// [`SWITCH_OVERLAP`] after it went down while its connection may still be there.
  pub fn sends_on_direct(&self, now: Instant) -> bool {
    match self.phase {
      Phase::Up { .. } => true,
      Phase::Proving { .. } => self.is_overlapping(now),
      Phase::Idle | Phase::Dialing { .. } => false,
    }
  }

  /// Whether a dialing round may start now: none is running and the backoff after failed rounds
  /// has passed. A new offer or a network change makes it start again at once
  /// ([`PathSelector::reset_backoff`]).
  pub fn may_dial(&self, now: Instant) -> bool {
    matches!(self.phase, Phase::Idle) && self.maybe_retry_at.is_none_or(|at| now >= at)
  }

  /// A dialing round started.
  pub fn on_dialing(&mut self, now: Instant) {
    if matches!(self.phase, Phase::Idle) {
      self.phase = Phase::Dialing { since: now };
    }
  }

  /// A new offer arrived or the network changed: the next round may start without waiting.
  pub fn reset_backoff(&mut self) {
    self.failed_rounds = 0;
    self.maybe_retry_at = None;
  }

  /// A direct connection completed its pinned handshake.
  pub fn on_connected(&mut self, now: Instant) {
    if matches!(self.phase, Phase::Idle | Phase::Dialing { .. }) {
      self.phase = Phase::Proving {
        clean_since: now,
        last_heard: now,
        lost_in_row: 0,
      };
    }
  }

  /// Anything arrived on the direct connection: a pong, media, the peer's ping.
  pub fn on_heard(&mut self, now: Instant) {
    match &mut self.phase {
      Phase::Proving {
        last_heard,
        lost_in_row,
        ..
      }
      | Phase::Up {
        last_heard,
        lost_in_row,
      } => {
        *last_heard = now;
        *lost_in_row = 0;
      },
      Phase::Idle | Phase::Dialing { .. } => {},
    }
  }

  /// A ping went unanswered. Returns the change if the media went back to the SFU.
  pub fn on_ping_lost(&mut self, now: Instant) -> Option<PathChange> {
    match &mut self.phase {
      Phase::Proving {
        clean_since,
        lost_in_row,
        ..
      } => {
        // Proving starts over: the path must answer every ping for the whole time.
        *clean_since = now;
        *lost_in_row += 1;
        None
      },
      Phase::Up { lost_in_row, .. } => {
        *lost_in_row += 1;
        let is_lost = *lost_in_row >= MAX_LOST_PINGS;
        is_lost.then(|| self.go_down(now, DownReason::LostPings, true))
      },
      Phase::Idle | Phase::Dialing { .. } => None,
    }
  }

  /// The direct connection closed or failed, or the network changed, or direct media is no longer
  /// allowed: back to the SFU at once.
  pub fn on_closed(&mut self, now: Instant, reason: DownReason) -> Option<PathChange> {
    match self.phase {
      Phase::Up { .. } => Some(self.go_down(now, reason, false)),
      Phase::Proving { .. } | Phase::Dialing { .. } => {
        self.fail_round(now);
        None
      },
      Phase::Idle => None,
    }
  }

  /// Advances the timers: the media moves to a direct path that proved itself, and back from one
  /// that went silent; a dialing round that found nothing ends.
  pub fn poll(&mut self, now: Instant) -> Option<PathChange> {
    match self.phase {
      Phase::Dialing { since } if now.saturating_duration_since(since) >= DIAL_TIMEOUT => {
        self.fail_round(now);
        None
      },
      Phase::Proving {
        clean_since,
        last_heard,
        ..
      } => {
        if now.saturating_duration_since(last_heard) >= MAX_SILENCE {
          self.fail_round(now);
          None
        } else if now.saturating_duration_since(clean_since) >= UP_AFTER {
          self.phase = Phase::Up {
            last_heard,
            lost_in_row: 0,
          };
          self.maybe_overlap_until = Some(now + SWITCH_OVERLAP);
          self.reset_backoff();
          Some(PathChange::Up)
        } else {
          None
        }
      },
      Phase::Up { last_heard, .. } if now.saturating_duration_since(last_heard) >= MAX_SILENCE => {
        Some(self.go_down(now, DownReason::Silence, true))
      },
      Phase::Idle | Phase::Dialing { .. } | Phase::Up { .. } => None,
    }
  }

  /// When [`PathSelector::poll`] should run next: the next deadline of the current phase.
  pub fn next_deadline(&self) -> Option<Instant> {
    let phase_deadline = match self.phase {
      Phase::Idle => self.maybe_retry_at,
      Phase::Dialing { since } => Some(since + DIAL_TIMEOUT),
      Phase::Proving {
        clean_since,
        last_heard,
        ..
      } => Some((clean_since + UP_AFTER).min(last_heard + MAX_SILENCE)),
      Phase::Up { last_heard, .. } => Some(last_heard + MAX_SILENCE),
    };
    match (phase_deadline, self.maybe_overlap_until) {
      (Some(phase), Some(overlap)) => Some(phase.min(overlap)),
      (deadline, None) | (None, deadline) => deadline,
    }
  }

  fn is_overlapping(&self, now: Instant) -> bool {
    self.maybe_overlap_until.is_some_and(|until| now < until)
  }

  /// The media goes back to the SFU. A connection that is still there (lost pings, silence) keeps
  /// carrying a copy for [`SWITCH_OVERLAP`] and proves itself again; a closed one is gone.
  fn go_down(&mut self, now: Instant, reason: DownReason, may_still_carry: bool) -> PathChange {
    if may_still_carry {
      self.phase = Phase::Proving {
        clean_since: now,
        last_heard: now,
        lost_in_row: 0,
      };
      self.maybe_overlap_until = Some(now + SWITCH_OVERLAP);
    } else {
      self.maybe_overlap_until = None;
      self.fail_round(now);
    }
    PathChange::Down(reason)
  }

  fn fail_round(&mut self, now: Instant) {
    self.phase = Phase::Idle;
    let wait = FIRST_RETRY_WAIT
      .saturating_mul(1u32 << self.failed_rounds.min(16))
      .min(MAX_RETRY_WAIT);
    self.failed_rounds = self.failed_rounds.saturating_add(1);
    self.maybe_retry_at = Some(now + wait);
  }
}

fn check_body(body: &DirectOfferBody) -> Result<(), OfferError> {
  if body.candidates.len() > MAX_DIRECT_CANDIDATES {
    return Err(OfferError::TooManyCandidates(body.candidates.len()));
  }
  if body.certificate_hash.len() != CERTIFICATE_HASH_LEN {
    return Err(OfferError::CertificateHash(body.certificate_hash.len()));
  }
  if body
    .candidates
    .iter()
    .any(|candidate| !is_usable(candidate.address))
  {
    return Err(OfferError::UnusableAddress);
  }
  Ok(())
}

/// An address a peer on another machine can be reached at: no port 0, no unspecified, loopback,
/// multicast or broadcast address, no IPv6 link-local (it needs a scope only the own machine knows).
fn is_usable(address: SocketAddr) -> bool {
  if address.port() == 0 {
    return false;
  }
  match address.ip() {
    IpAddr::V4(ip) => !(ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast()),
    IpAddr::V6(ip) => {
      let is_link_local = (ip.segments()[0] & 0xffc0) == 0xfe80;
      let is_mapped_v4 = ip.to_ipv4_mapped().is_some();
      !(ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || is_link_local || is_mapped_v4)
    },
  }
}

/// IPv6 first (no NAT in the way, usually), then host, mapped, reflexive: the same network first.
fn sort_for_dialing(candidates: &mut [DirectCandidate]) {
  candidates.sort_by_key(|candidate| {
    let kind_rank = match candidate.kind {
      DirectCandidateKind::Host => 0,
      DirectCandidateKind::Mapped => 1,
      DirectCandidateKind::Reflexive | DirectCandidateKind::Unknown(_) => 2,
    };
    (!candidate.address.is_ipv6(), kind_rank)
  });
}

fn offer_context(call_id: CallId, from: DeviceId, to: DeviceId) -> OfferContext {
  OfferContext {
    call_id: BindingCallId::new(raw(call_id)),
    from: BindingDeviceId::new(raw(from)),
    to: BindingDeviceId::new(raw(to)),
  }
}

fn raw(id: impl Id) -> u64 {
  // Invariant: snowflakes are non-negative, so the conversion never fails.
  u64::try_from(id.snowflake().get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_proto::{DirectMediaMode, Snowflake};

  use super::*;

  const EXPORTED: [u8; 32] = [5; 32];

  mod eligibility {
    use super::*;

    #[test]
    fn two_members_with_the_default_policy_may() {
      let policy = DirectMediaPolicy::default();
      assert_eq!(eligibility(&inputs(&policy)), Ok(()));
    }

    #[test]
    fn each_reason_keeps_the_call_on_the_sfu() {
      let policy = DirectMediaPolicy::default();
      let three = DirectPathInputs {
        devices_in_call: 3,
        ..inputs(&policy)
      };
      assert_eq!(eligibility(&three), Err(NotDirect::NotOneToOne));
      let off = DirectPathInputs {
        does_user_allow: false,
        ..inputs(&policy)
      };
      assert_eq!(eligibility(&off), Err(NotDirect::UserSetting));
      let unverified = DirectPathInputs {
        maybe_peer: None,
        ..inputs(&policy)
      };
      assert_eq!(eligibility(&unverified), Err(NotDirect::PeerUnverified));
      let guest = DirectPathInputs {
        maybe_peer: Some(CallPeerStanding::Outsider),
        ..inputs(&policy)
      };
      assert_eq!(eligibility(&guest), Err(NotDirect::Policy));
      let policy_off = DirectMediaPolicy {
        mode: DirectMediaMode::Off,
      };
      assert_eq!(eligibility(&inputs(&policy_off)), Err(NotDirect::Policy));
    }
  }

  mod candidates {
    use super::*;

    #[test]
    fn unusable_and_duplicate_addresses_are_left_out_and_ipv6_hosts_come_first() {
      let interfaces: Vec<IpAddr> = [
        "127.0.0.1",
        "192.168.1.20",
        "fe80::1",
        "2001:db8::7",
        "0.0.0.0",
        "::ffff:192.0.2.1",
        "224.0.0.1",
      ]
      .iter()
      .map(|ip| ip.parse().unwrap())
      .collect();
      let reflexive: SocketAddr = "198.51.100.4:61000".parse().unwrap();
      let candidates = own_candidates(interfaces, 50000, Some(reflexive), Some(reflexive));
      let listed: Vec<(DirectCandidateKind, String)> = candidates
        .iter()
        .map(|candidate| (candidate.kind.clone(), candidate.address.to_string()))
        .collect();
      assert_eq!(
        listed,
        vec![
          (DirectCandidateKind::Host, "[2001:db8::7]:50000".to_owned()),
          (DirectCandidateKind::Host, "192.168.1.20:50000".to_owned()),
          (DirectCandidateKind::Mapped, "198.51.100.4:61000".to_owned()),
        ]
      );
    }

    #[test]
    fn at_most_eight_are_offered() {
      let interfaces = (1..=20u8).map(|last| IpAddr::from([10, 0, 0, last]));
      assert_eq!(
        own_candidates(interfaces, 4000, None, None).len(),
        MAX_DIRECT_CANDIDATES
      );
    }

    #[test]
    fn dialing_steps_through_the_pairs() {
      assert_eq!(dial_delay(0), Duration::ZERO);
      assert_eq!(dial_delay(3), Duration::from_millis(150));
    }
  }

  mod offers {
    use super::*;

    #[test]
    fn an_offer_round_trips_between_the_two_devices() {
      let key = key(2);
      let sealed = seal_offer(&key, &alice(), &body(1), &mut OsRng)
        .unwrap()
        .into_vec();
      assert_eq!(offer_epoch(&sealed), Ok(2));
      let mut inbox = OfferInbox::default();
      let opened = open_offer(&key, &bob(), &mut inbox, &sealed).unwrap();
      assert_eq!(opened, body(1));
    }

    #[test]
    fn an_older_or_repeated_offer_of_the_epoch_is_stale_and_a_new_epoch_starts_over() {
      let mut inbox = OfferInbox::default();
      let second = seal_offer(&key(2), &alice(), &body(2), &mut OsRng)
        .unwrap()
        .into_vec();
      let first = seal_offer(&key(2), &alice(), &body(1), &mut OsRng)
        .unwrap()
        .into_vec();
      open_offer(&key(2), &bob(), &mut inbox, &second).unwrap();
      assert_eq!(
        open_offer(&key(2), &bob(), &mut inbox, &first).unwrap_err(),
        OfferError::Stale { held: 2 }
      );
      assert_eq!(
        open_offer(&key(2), &bob(), &mut inbox, &second).unwrap_err(),
        OfferError::Stale { held: 2 }
      );
      let next_epoch = seal_offer(&key(3), &alice(), &body(1), &mut OsRng)
        .unwrap()
        .into_vec();
      assert!(open_offer(&key(3), &bob(), &mut inbox, &next_epoch).is_ok());
    }

    #[test]
    fn the_own_offer_bounced_back_does_not_open() {
      let key = key(2);
      let sealed = seal_offer(&key, &alice(), &body(1), &mut OsRng)
        .unwrap()
        .into_vec();
      let mut inbox = OfferInbox::default();
      assert!(matches!(
        open_offer(&key, &alice(), &mut inbox, &sealed),
        Err(OfferError::Sealed(DirectPathError::Unreadable))
      ));
    }

    #[test]
    fn malformed_bodies_are_refused() {
      let key = key(2);
      let mut too_many = body(1);
      too_many.candidates = (0..9u8)
        .map(|last| DirectCandidate {
          kind: DirectCandidateKind::Host,
          address: SocketAddr::from(([192, 0, 2, last + 1], 4000)),
        })
        .collect();
      assert_eq!(opened(&key, &too_many), Err(OfferError::TooManyCandidates(9)));
      let mut short_hash = body(1);
      short_hash.certificate_hash = Base64Bytes::new(vec![1; 8]);
      assert_eq!(opened(&key, &short_hash), Err(OfferError::CertificateHash(8)));
      let mut loopback = body(1);
      loopback.candidates[0].address = "127.0.0.1:4000".parse().unwrap();
      assert_eq!(opened(&key, &loopback), Err(OfferError::UnusableAddress));
      let not_json = key
        .seal(
          &offer_context(call(), alice().own, alice().peer),
          b"{",
          &mut OsRng,
        )
        .unwrap();
      assert_eq!(
        open_offer(&key, &bob(), &mut OfferInbox::default(), &not_json),
        Err(OfferError::Malformed)
      );
      assert_eq!(
        open_offer(
          &key,
          &bob(),
          &mut OfferInbox::default(),
          &[0; MAX_DIRECT_OFFER_BYTES + 1]
        ),
        Err(OfferError::TooLarge(MAX_DIRECT_OFFER_BYTES + 1))
      );
    }

    #[test]
    fn both_devices_pick_the_same_connection() {
      assert!(accepted_connection_wins(&alice()));
      assert!(!accepted_connection_wins(&bob()));
    }

    fn opened(key: &DirectPathKey, body: &DirectOfferBody) -> Result<DirectOfferBody, OfferError> {
      let plaintext = serde_json::to_vec(body).unwrap();
      let sealed = key
        .seal(
          &offer_context(call(), alice().own, alice().peer),
          &plaintext,
          &mut OsRng,
        )
        .unwrap();
      open_offer(key, &bob(), &mut OfferInbox::default(), &sealed)
    }
  }

  mod switching {
    use super::*;

    #[test]
    fn a_path_that_answers_for_a_second_takes_the_media_with_an_overlap() {
      let start = Instant::now();
      let mut selector = PathSelector::new();
      assert!(selector.may_dial(start));
      selector.on_dialing(start);
      selector.on_connected(at(start, 100));
      for step in 1..=4 {
        selector.on_heard(at(start, 100 + step * 200));
        assert_eq!(selector.poll(at(start, 100 + step * 200)), None);
      }
      selector.on_heard(at(start, 1100));
      assert_eq!(selector.poll(at(start, 1100)), Some(PathChange::Up));
      assert_eq!(selector.state(), PathState::Direct);
      assert!(selector.sends_on_direct(at(start, 1100)));
      assert!(
        selector.sends_on_relay(at(start, 1500)),
        "both paths during the overlap"
      );
      assert!(!selector.sends_on_relay(at(start, 2100)));
    }

    #[test]
    fn three_lost_pings_send_the_media_back_at_once_and_keep_a_copy_on_the_direct_path() {
      let (mut selector, up) = up_selector();
      assert_eq!(selector.on_ping_lost(at(up, 200)), None);
      assert_eq!(selector.on_ping_lost(at(up, 400)), None);
      let down = at(up, 600);
      assert_eq!(
        selector.on_ping_lost(down),
        Some(PathChange::Down(DownReason::LostPings))
      );
      assert_eq!(selector.state(), PathState::Relay);
      assert!(selector.sends_on_relay(down));
      assert!(
        selector.sends_on_direct(at(up, 900)),
        "the overlap covers the switch back"
      );
      assert!(!selector.sends_on_direct(at(up, 1700)));
    }

    #[test]
    fn an_answer_between_lost_pings_resets_the_count() {
      let (mut selector, up) = up_selector();
      selector.on_ping_lost(at(up, 200));
      selector.on_ping_lost(at(up, 400));
      selector.on_heard(at(up, 450));
      assert_eq!(selector.on_ping_lost(at(up, 600)), None);
      assert_eq!(selector.state(), PathState::Direct);
    }

    #[test]
    fn silence_sends_the_media_back() {
      let (mut selector, up) = up_selector();
      assert_eq!(
        selector.next_deadline(),
        Some(at(up, 600)),
        "the silence limit comes before the overlap ends"
      );
      assert_eq!(selector.poll(at(up, 599)), None);
      assert_eq!(
        selector.poll(at(up, 600)),
        Some(PathChange::Down(DownReason::Silence))
      );
    }

    #[test]
    fn a_closed_path_goes_down_without_overlap_and_waits_before_the_next_round() {
      let (mut selector, up) = up_selector();
      let closed = at(up, 100);
      assert_eq!(
        selector.on_closed(closed, DownReason::Closed),
        Some(PathChange::Down(DownReason::Closed))
      );
      assert!(!selector.sends_on_direct(closed));
      assert!(selector.sends_on_relay(closed));
      // A path that was up reset the backoff: the first wait again.
      assert!(!selector.may_dial(at(up, 2099)));
      assert!(selector.may_dial(at(up, 2100)));
    }

    #[test]
    fn failed_rounds_back_off_up_to_a_minute_and_a_new_offer_resets_it() {
      let start = Instant::now();
      let mut selector = PathSelector::new();
      let mut now = start;
      let mut waits = Vec::new();
      for _ in 0..7 {
        selector.on_dialing(now);
        let ended = now + DIAL_TIMEOUT;
        assert_eq!(selector.poll(ended), None);
        let retry = selector.next_deadline().unwrap();
        waits.push(retry.duration_since(ended).as_secs());
        now = retry;
      }
      assert_eq!(waits, vec![2, 4, 8, 16, 32, 60, 60]);
      selector.on_dialing(now);
      selector.poll(now + DIAL_TIMEOUT);
      selector.reset_backoff();
      assert!(selector.may_dial(now + DIAL_TIMEOUT));
    }

    #[test]
    fn a_lost_ping_while_proving_starts_the_proof_over() {
      let start = Instant::now();
      let mut selector = PathSelector::new();
      selector.on_connected(start);
      selector.on_heard(at(start, 500));
      selector.on_ping_lost(at(start, 700));
      selector.on_heard(at(start, 900));
      assert_eq!(
        selector.poll(at(start, 1100)),
        None,
        "a second after the loss, not the start"
      );
      selector.on_heard(at(start, 1300));
      selector.on_heard(at(start, 1600));
      assert_eq!(selector.poll(at(start, 1700)), Some(PathChange::Up));
    }

    #[test]
    fn a_proving_path_that_goes_silent_fails_the_round() {
      let start = Instant::now();
      let mut selector = PathSelector::new();
      selector.on_connected(start);
      assert_eq!(selector.poll(at(start, 600)), None);
      assert_eq!(selector.state(), PathState::Relay);
      assert!(!selector.may_dial(at(start, 600)));
      assert!(selector.may_dial(at(start, 2600)));
    }

    fn up_selector() -> (PathSelector, Instant) {
      let start = Instant::now();
      let mut selector = PathSelector::new();
      selector.on_connected(start);
      let up = at(start, 1000);
      selector.on_heard(up);
      assert_eq!(selector.poll(up), Some(PathChange::Up));
      (selector, up)
    }

    fn at(start: Instant, millis: u64) -> Instant {
      start + Duration::from_millis(millis)
    }
  }

  fn inputs(policy: &DirectMediaPolicy) -> DirectPathInputs<'_> {
    DirectPathInputs {
      policy,
      does_user_allow: true,
      own: CallPeerStanding::Member,
      maybe_peer: Some(CallPeerStanding::Member),
      devices_in_call: 2,
    }
  }

  fn key(epoch: u64) -> DirectPathKey {
    DirectPathKey::from_exporter(epoch, &EXPORTED).unwrap()
  }

  fn body(counter: u64) -> DirectOfferBody {
    DirectOfferBody {
      counter,
      certificate_hash: Base64Bytes::new(vec![9; CERTIFICATE_HASH_LEN]),
      candidates: vec![
        DirectCandidate {
          kind: DirectCandidateKind::Host,
          address: "[2001:db8::2]:4000".parse().unwrap(),
        },
        DirectCandidate {
          kind: DirectCandidateKind::Reflexive,
          address: "198.51.100.9:62000".parse().unwrap(),
        },
      ],
    }
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(7).unwrap())
  }

  fn alice() -> DirectPair {
    DirectPair {
      call_id: call(),
      own: device(1),
      peer: device(2),
    }
  }

  fn bob() -> DirectPair {
    DirectPair {
      call_id: call(),
      own: device(2),
      peer: device(1),
    }
  }

  fn device(id: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
