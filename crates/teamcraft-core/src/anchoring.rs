//! The channel anchor on this device (e2ee.md §4.6.5): when it founds channels anchored, what it
//! makes of the anchor a group state carries against the pin and the floor in the account data,
//! and whether a channel's name lets a signed deletion purge at once (D3).
//!
//! The rule of the whole design holds here too: **the anchor only grants.** A group without one,
//! or with one this device can't verify, is judged as before anchors, unless the user's devices
//! pinned the conversation to an anchor: then a group state without it, or with another founding,
//! is refused.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::channel_anchor::{
  AnchorFloor, AnchorHash, ChannelAnchor, DEFAULT_MAX_ANCHOR_BYTES, FloorError, VerifiedAnchor, name_hash,
};
use teamcraft_crypto::mls::MlsError;
use teamcraft_proto::channel_name::keeps_channel_name_rules;

/// The release whose apps all announce the anchor in their key packages and leaves (#1741): once
/// the server's `minimum_client_version` reaches it, no app that can join a channel lacks it, and
/// new channels are founded anchored (D2).
pub const ANCHORING_RELEASE: ReleaseVersion = ReleaseVersion::new(1, 0, 31);

/// A `major.minor.patch` release, as the server's `minimum_client_version` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ReleaseVersion {
  pub major: u64,
  pub minor: u64,
  pub patch: u64,
}

/// What a group state says about its anchor, before the pin and the floor.
#[derive(Debug)]
pub enum AnchorOutcome {
  /// The group context carries no anchor (a DM, a channel from before anchors).
  Absent,
  /// Checks 1 to 5 hold.
  Verified(Box<VerifiedAnchor>),
  /// An anchor that doesn't verify here (yet): it grants nothing. `maybe_founding_hash` is its
  /// founding's pin if it parses, to tell "the pinned founding, unverifiable for now" (a device
  /// list not fetched) from another founding.
  Unverified { maybe_founding_hash: Option<AnchorHash> },
}

/// What to do with a group state, judged against the pin and the floor.
#[derive(Debug, PartialEq, Eq)]
pub enum AnchorVerdict {
  /// No pin and no verified anchor: judged as before anchors.
  Unanchored,
  /// Accepted; record this pin and floor (the first, or raised) in the account data.
  Record(AnchorFloor),
  /// Accepted: the pinned founding, which this device can't verify right now. Nothing is raised,
  /// and the anchor grants nothing until it verifies.
  KeepPin,
  /// Refused: the device stays out and shows the channel as unreachable (e2ee.md §4.6.5,
  /// "Stripping and replacing").
  Refuse(FloorError),
}

impl ReleaseVersion {
  pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
    Self { major, minor, patch }
  }

  /// Reads `1.2.3`, ignoring a pre-release or build suffix (`1.2.3-rc.1`); `None` for anything else.
  pub fn parse(text: &str) -> Option<Self> {
    let core = text.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = Self::new(parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
  }
}

impl AnchorOutcome {
  /// The outcome of `verified_anchor` on a group or a staged welcome whose context holds
  /// `maybe_bytes`.
  pub fn of(result: Result<Option<VerifiedAnchor>, MlsError>, maybe_bytes: Option<&[u8]>) -> Self {
    match result {
      Ok(None) => Self::Absent,
      Ok(Some(verified)) => Self::Verified(Box::new(verified)),
      Err(_) => Self::Unverified {
        maybe_founding_hash: maybe_bytes
          .and_then(|bytes| ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES).ok())
          .and_then(|anchor| anchor.founding_hash().ok()),
      },
    }
  }

  pub fn verified(&self) -> Option<&VerifiedAnchor> {
    match self {
      Self::Verified(verified) => Some(verified),
      Self::Absent | Self::Unverified { .. } => None,
    }
  }
}

/// Whether this device founds a new channel anchored: once the highest `minimum_client_version`
/// it has seen (kept monotonic in its store, so a server can't turn anchoring off again by
/// reporting a lower one) reaches [`ANCHORING_RELEASE`].
pub fn founds_anchored(maybe_highest_minimum: Option<ReleaseVersion>) -> bool {
  maybe_highest_minimum.is_some_and(|minimum| minimum >= ANCHORING_RELEASE)
}

/// Check 6 when the device **enters** a group state at `epoch` (a welcome, an add-back, an external
/// commit, its own founding): the pinned founding, nothing below the floor.
pub fn judge_entering(
  maybe_floor: Option<&AnchorFloor>,
  outcome: &AnchorOutcome,
  epoch: u64,
) -> AnchorVerdict {
  let Some(floor) = maybe_floor else {
    return match outcome {
      AnchorOutcome::Verified(verified) => AnchorVerdict::Record(AnchorFloor::first(verified, epoch)),
      AnchorOutcome::Absent | AnchorOutcome::Unverified { .. } => AnchorVerdict::Unanchored,
    };
  };
  match outcome {
    AnchorOutcome::Verified(verified) => match floor.check_entering(verified, epoch) {
      Ok(()) => raised(floor, verified, epoch),
      Err(error) => AnchorVerdict::Refuse(error),
    },
    AnchorOutcome::Absent => AnchorVerdict::Refuse(FloorError::Stripped),
    AnchorOutcome::Unverified { maybe_founding_hash } => keep_if_pinned(floor, *maybe_founding_hash),
  }
}

/// Check 6 at a commit **inside** a group state the device holds (a move or rename, its own
/// included): only the founding pin is checked, the floor only rises. The MLS chain carries the
/// continuity, and the floor, merged across the user's devices, may be ahead of this device's.
pub fn judge_inside(maybe_floor: Option<&AnchorFloor>, outcome: &AnchorOutcome, epoch: u64) -> AnchorVerdict {
  let Some(floor) = maybe_floor else {
    return judge_entering(None, outcome, epoch);
  };
  match outcome {
    AnchorOutcome::Verified(verified) => match floor.check_inside(verified) {
      Ok(()) => raised(floor, verified, epoch),
      Err(error) => AnchorVerdict::Refuse(error),
    },
    AnchorOutcome::Absent => AnchorVerdict::Refuse(FloorError::Stripped),
    AnchorOutcome::Unverified { maybe_founding_hash } => keep_if_pinned(floor, *maybe_founding_hash),
  }
}

/// D3 and D5: whether the name lets a signed deletion of an anchored channel purge at once. It
/// must be the anchor's confirmed name, not set by a member, the deletion must name it, and the
/// name the server lists must hash to it and keep the name rules. The floor must grant (no fork).
/// Otherwise the deletion takes the retention. A deletion without a name hash is a mismatch.
pub fn name_counts_for_deletion(
  anchor: &VerifiedAnchor,
  floor_grants: bool,
  maybe_listed_name: Option<&str>,
  maybe_deletion_name_hash: Option<AnchorHash>,
) -> bool {
  let is_listed_name_the_anchors = maybe_listed_name
    .is_some_and(|name| keeps_channel_name_rules(name) && name_hash(name) == anchor.properties().name_hash);
  floor_grants && is_listed_name_the_anchors && anchor.name_counts_for_deletion(maybe_deletion_name_hash)
}

/// Whether the listed name differs from what the anchor signed: shown to members as a mark.
pub fn is_listed_name_stale(anchor: &VerifiedAnchor, listed_name: &str) -> bool {
  name_hash(listed_name) != anchor.properties().name_hash
}

fn raised(floor: &AnchorFloor, verified: &VerifiedAnchor, epoch: u64) -> AnchorVerdict {
  let mut next = *floor;
  next.raise(verified, epoch);
  AnchorVerdict::Record(next)
}

fn keep_if_pinned(floor: &AnchorFloor, maybe_founding_hash: Option<AnchorHash>) -> AnchorVerdict {
  if maybe_founding_hash == Some(floor.founding_hash) {
    AnchorVerdict::KeepPin
  } else {
    AnchorVerdict::Refuse(FloorError::OtherFounding)
  }
}

#[cfg(test)]
mod tests;
