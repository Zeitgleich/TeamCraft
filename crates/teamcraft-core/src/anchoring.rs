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
  AnchorError, AnchorExpectation, AnchorFloor, AnchorHash, AnchorKind, AnchorOrigin, ChannelAnchor,
  DEFAULT_MAX_ANCHOR_BYTES, ExpectedKind, FloorError, VerifiedAnchor, name_hash,
};
use teamcraft_crypto::ids::{MeetingId, UserId};
use teamcraft_crypto::mls::MlsError;
use teamcraft_proto::channel_name::keeps_channel_name_rules;

use crate::channel_roles::RoleFounding;
use crate::convert;

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

/// What a meeting chat's anchor must name (D10): the meeting and the organizer as founder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeetingExpectation {
  pub meeting_id: MeetingId,
  pub organizer: UserId,
}

/// What a group state says about its anchor, before the pin and the floor.
#[derive(Debug)]
pub enum AnchorOutcome {
  /// The group context carries no anchor (a DM, a channel from before anchors).
  Absent,
  /// Checks 1 to 5 hold.
  Verified(Box<VerifiedAnchor>),
  /// An anchor that doesn't verify here (yet): it grants nothing. `maybe_parsed` is what it says
  /// if it parses, unverified: enough to tell "the pinned founding, unverifiable for now" (a device
  /// list not fetched, which the server can cause) from another founding, and to hold it to the
  /// floor all the same.
  Unverified { maybe_parsed: Option<ParsedAnchor> },
}

/// What an anchor that doesn't verify says about itself: its founding's pin and its current
/// properties. Never grants; only lets the floor refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedAnchor {
  pub founding_hash: AnchorHash,
  pub properties_version: u64,
  pub properties_hash: AnchorHash,
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
        maybe_parsed: maybe_bytes.and_then(ParsedAnchor::of),
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

impl ParsedAnchor {
  /// The anchor's own word about itself, `None` if it doesn't parse.
  pub fn of(bytes: &[u8]) -> Option<Self> {
    let anchor = ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES).ok()?;
    Some(Self {
      founding_hash: anchor.founding_hash().ok()?,
      properties_version: anchor.properties().version,
      properties_hash: anchor.properties().hash().ok()?,
    })
  }
}

/// What an anchor of a conversation must name, or `None` to judge it as before anchors. Whether to
/// check is decided on what this device holds, never on the server's kind for the conversation: a
/// group that carries an anchor, or a conversation the user's devices pinned, is checked. A server
/// that relists a pinned channel as a DM, or a welcome that comes before the conversation's
/// record, can't skip the check. With `maybe_meeting` the anchor must be the meeting chat's (D10,
/// #1747), else a channel's.
pub fn anchor_expectation(
  conversation_id: teamcraft_crypto::ids::ConversationId,
  is_pinned: bool,
  carries_anchor: bool,
  maybe_meeting: Option<MeetingExpectation>,
) -> Option<AnchorExpectation> {
  let kind = maybe_meeting.map_or(ExpectedKind::Channel, |meeting| ExpectedKind::MeetingChat {
    meeting_id: meeting.meeting_id,
    organizer: meeting.organizer,
  });
  (is_pinned || carries_anchor).then_some(AnchorExpectation {
    conversation_id,
    kind,
  })
}

/// The meeting and founder a meeting chat's anchor names about itself, unverified; `None` for
/// another kind or an anchor that doesn't parse. For a group state the device already holds (a
/// commit inside it): the founding can't change there, and was checked against the server's
/// meeting and organizer when the device entered.
pub fn meeting_named_by(bytes: &[u8]) -> Option<MeetingExpectation> {
  let anchor = ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES).ok()?;
  let founding = anchor.founding();
  match (founding.kind, founding.origin) {
    (AnchorKind::MeetingChat { meeting_id }, AnchorOrigin::Founded { founder_user_id, .. }) => {
      Some(MeetingExpectation {
        meeting_id,
        organizer: founder_user_id,
      })
    },
    _ => None,
  }
}

/// D10: whether a meeting chat's group state is refused for what its anchor names, pin or not. An
/// anchor of another meeting or conversation, or one whose founder isn't the organizer, is a copy
/// or a forgery: no honest device makes one. Anything else that doesn't verify (a device list not
/// fetched, which a server can cause) grants nothing, as for channels.
pub fn refuses_meeting_anchor(
  expected: &AnchorExpectation,
  result: &Result<Option<VerifiedAnchor>, MlsError>,
) -> bool {
  matches!(expected.kind, ExpectedKind::MeetingChat { .. })
    && matches!(
      result,
      Err(MlsError::Anchor(
        AnchorError::WrongKind | AnchorError::NotTheOrganizer | AnchorError::WrongConversation { .. }
      ))
    )
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
    // Not verifiable doesn't mean exempt: a server that withholds a device list must not get a
    // rolled-back state past the floor.
    AnchorOutcome::Unverified { maybe_parsed } => match keep_if_pinned(floor, *maybe_parsed) {
      AnchorVerdict::KeepPin => match maybe_parsed.map(|parsed| unverified_at_floor(floor, &parsed, epoch)) {
        Some(Err(error)) => AnchorVerdict::Refuse(error),
        _ => AnchorVerdict::KeepPin,
      },
      other => other,
    },
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
    AnchorOutcome::Unverified { maybe_parsed } => keep_if_pinned(floor, *maybe_parsed),
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

/// Who founds a channel's roles (e2ee.md §4.6.5, "What the founding decides"). `is_anchored`: the
/// group carries an anchor or the user's devices pinned the channel. Then only the verified
/// anchor's founder, under its identity key, while the floor grants; an anchor that doesn't verify
/// (whatever the reason, a refused device list included), a fork, or an anchor that names no
/// founder (a review or a re-founding, #1728) gives nobody: nothing founds and nothing anchors.
/// Only a channel with neither anchor nor pin is [`RoleFounding::Claimed`].
pub fn role_founding(
  is_anchored: bool,
  maybe_verified: Option<&VerifiedAnchor>,
  floor_grants: bool,
) -> RoleFounding {
  if !is_anchored {
    return RoleFounding::Claimed;
  }
  let Some((user, key)) = maybe_verified
    .filter(|_| floor_grants)
    .and_then(VerifiedAnchor::founder)
  else {
    return RoleFounding::Withheld;
  };
  match convert::user_from_crypto(user) {
    Ok(founder) => RoleFounding::Anchored {
      founder,
      identity_key: *key.as_bytes(),
    },
    Err(_) => RoleFounding::Withheld,
  }
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

/// The floor's checks on an unverified anchor's own word: not an older properties version, not
/// another hash for the same one, not an older epoch.
fn unverified_at_floor(floor: &AnchorFloor, parsed: &ParsedAnchor, epoch: u64) -> Result<(), FloorError> {
  if parsed.properties_version < floor.properties_version {
    return Err(FloorError::OlderProperties {
      offered: parsed.properties_version,
      floor: floor.properties_version,
    });
  }
  if parsed.properties_version == floor.properties_version && parsed.properties_hash != floor.properties_hash
  {
    return Err(FloorError::OtherProperties(parsed.properties_version));
  }
  if epoch < floor.epoch {
    return Err(FloorError::OlderEpoch {
      offered: epoch,
      floor: floor.epoch,
    });
  }
  Ok(())
}

fn keep_if_pinned(floor: &AnchorFloor, maybe_parsed: Option<ParsedAnchor>) -> AnchorVerdict {
  if maybe_parsed.map(|parsed| parsed.founding_hash) == Some(floor.founding_hash) {
    AnchorVerdict::KeepPin
  } else {
    AnchorVerdict::Refuse(FloorError::OtherFounding)
  }
}

#[cfg(test)]
mod tests;
