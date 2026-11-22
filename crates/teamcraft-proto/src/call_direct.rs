//! Direct media for 1:1 calls (issue #127, ARCHITECTURE.md §6.7, e2ee.md §8.4, admin.md §3.16):
//! whether two devices may send their media to each other instead of through the SFU, as a group of
//! the owner-signed workspace policy, and the limits of the offers the SFU relays for it.
//!
//! A direct path shows each side's IP addresses to the other, so each device decides for itself from
//! the policy it verified and its user's setting; the server can't turn it on for anybody.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, Role};

/// The largest sealed `direct_offer` the SFU relays. An offer of 8 candidates is about 500 bytes.
pub const MAX_DIRECT_OFFER_BYTES: usize = 2048;
/// The most candidates an offer may name; receivers refuse an offer with more.
pub const MAX_DIRECT_CANDIDATES: usize = 8;
/// The length of an offer's probe token (birthday probing, ARCHITECTURE.md §6.7, "Hard NATs").
pub const DIRECT_PROBE_TOKEN_BYTES: usize = 8;

/// The `direct_media` group of the signed workspace policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DirectMediaPolicy {
  /// Between whom direct media is allowed.
  pub mode: DirectMediaMode,
}

wire_enum! {
  /// Between whom two devices in a 1:1 call may send their media directly.
  pub enum DirectMediaMode {
    /// Nobody: every call goes through the SFU.
    Off => "off",
    /// Two members of this instance; guests and external people never. The default.
    SameOrganization => "same_organization",
    /// Any two participants, guests included.
    Everyone => "everyone",
  }
}

impl Default for DirectMediaPolicy {
  fn default() -> Self {
    // The owner's decision for #127: members of the instance may, guests and external people never.
    Self {
      mode: DirectMediaMode::SameOrganization,
    }
  }
}

/// What a participant of a call is, as far as direct media is concerned: taken from the verified
/// roster and the participant's verified DAVE key binding, never from the server's word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPeerStanding {
  /// A member of this instance (any role but guest).
  Member,
  /// A meeting guest, a guest account, somebody external, or somebody this device can't place.
  Outsider,
}

impl CallPeerStanding {
  /// From a workspace role: a role from a newer app counts as an outsider.
  pub fn of(role: &Role) -> Self {
    match role {
      Role::Owner | Role::Admin | Role::Member => Self::Member,
      Role::Guest | Role::Unknown(_) => Self::Outsider,
    }
  }
}

impl DirectMediaPolicy {
  /// Whether a device of `own` standing may go direct with a device of `peer` standing. A mode from
  /// a newer app (`Unknown`) allows nobody: the SFU path is always safe.
  pub fn allows(&self, own: CallPeerStanding, peer: CallPeerStanding) -> bool {
    match self.mode {
      DirectMediaMode::Everyone => true,
      DirectMediaMode::SameOrganization => {
        own == CallPeerStanding::Member && peer == CallPeerStanding::Member
      },
      DirectMediaMode::Off | DirectMediaMode::Unknown(_) => false,
    }
  }
}

/// What a `direct_offer` carries, sealed (`teamcraft_crypto::direct_path`): only the other device of
/// the call reads it. JSON inside the seal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectOfferBody {
  /// Orders the sender's offers of one DAVE epoch: a receiver keeps only a higher one, so an older
  /// offer replayed by the SFU can't send it back to stale addresses.
  pub counter: u64,
  /// SHA-256 of the sender's ephemeral direct path certificate, which the receiver pins.
  pub certificate_hash: Base64Bytes,
  /// Where the sender can be reached, most promising first; at most [`MAX_DIRECT_CANDIDATES`].
  pub candidates: Vec<DirectCandidate>,
  /// How the sender's NAT maps its direct path socket, once two reflections told it; `None` while
  /// it doesn't know (one reflection, or none).
  #[serde(default, rename = "nat", skip_serializing_if = "Option::is_none")]
  pub maybe_nat: Option<DirectNatKind>,
  /// [`DIRECT_PROBE_TOKEN_BYTES`] random bytes the receiver puts into its birthday probes to the
  /// sender, so the sender takes only the receiver's probes (ARCHITECTURE.md §6.7, "Hard NATs").
  #[serde(default, rename = "probe_token", skip_serializing_if = "Option::is_none")]
  pub maybe_probe_token: Option<Base64Bytes>,
}

/// One address a device's direct path socket may be reached at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectCandidate {
  pub kind: DirectCandidateKind,
  pub address: SocketAddr,
}

wire_enum! {
  /// Where a candidate address comes from (ARCHITECTURE.md §6.7, "Endpoint discovery").
  pub enum DirectCandidateKind {
    /// An address of one of the device's interfaces.
    Host => "host",
    /// The address the SFU saw the direct path socket's packets come from.
    Reflexive => "reflexive",
    /// A port the device's router mapped on request (UPnP-IGD, NAT-PMP, PCP).
    Mapped => "mapped",
  }
}

wire_enum! {
  /// How a device's NAT maps its direct path socket, from the SFU's reflections of it on two
  /// ports (ARCHITECTURE.md §6.7, "Hard NATs").
  pub enum DirectNatKind {
    /// The same public address and port toward every destination, or no NAT at all: the
    /// reflexive candidate is where the other device reaches it.
    Easy => "easy",
    /// A new public port per destination (endpoint-dependent mapping): only birthday probing finds
    /// the port its NAT opened toward the other device.
    Hard => "hard",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use CallPeerStanding::{Member, Outsider};

  #[test]
  fn same_organization_is_the_default_and_connects_members_only() {
    let policy = DirectMediaPolicy::default();
    assert_eq!(policy.mode, DirectMediaMode::SameOrganization);
    assert!(policy.allows(Member, Member));
    assert!(!policy.allows(Member, Outsider));
    assert!(!policy.allows(Outsider, Member));
    assert!(!policy.allows(Outsider, Outsider));
  }

  #[test]
  fn off_and_unknown_modes_allow_nobody() {
    for mode in [
      DirectMediaMode::Off,
      DirectMediaMode::Unknown("friends_only".into()),
    ] {
      let policy = DirectMediaPolicy { mode };
      assert!(!policy.allows(Member, Member));
    }
  }

  #[test]
  fn everyone_includes_guests() {
    let policy = DirectMediaPolicy {
      mode: DirectMediaMode::Everyone,
    };
    assert!(policy.allows(Outsider, Member));
    assert!(policy.allows(Outsider, Outsider));
  }

  #[test]
  fn wire_format_is_stable_and_older_policies_parse() {
    let policy = DirectMediaPolicy {
      mode: DirectMediaMode::Everyone,
    };
    assert_eq!(serde_json::to_string(&policy).unwrap(), r#"{"mode":"everyone"}"#);
    let empty: DirectMediaPolicy = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, DirectMediaPolicy::default());
    let newer: DirectMediaPolicy = serde_json::from_str(r#"{"mode":"friends_only"}"#).unwrap();
    assert_eq!(newer.mode, DirectMediaMode::Unknown("friends_only".into()));
    assert_eq!(
      serde_json::to_string(&newer).unwrap(),
      r#"{"mode":"friends_only"}"#
    );
  }

  #[test]
  fn offer_body_wire_format() {
    let body = DirectOfferBody {
      counter: 2,
      certificate_hash: Base64Bytes::new(vec![1]),
      candidates: vec![
        DirectCandidate {
          kind: DirectCandidateKind::Host,
          address: "[2001:db8::1]:4000".parse().unwrap(),
        },
        DirectCandidate {
          kind: DirectCandidateKind::Reflexive,
          address: "192.0.2.1:5000".parse().unwrap(),
        },
      ],
      maybe_nat: None,
      maybe_probe_token: None,
    };
    let json = serde_json::to_string(&body).unwrap();
    assert_eq!(
      json,
      r#"{"counter":2,"certificate_hash":"AQ==","candidates":[{"kind":"host","address":"[2001:db8::1]:4000"},{"kind":"reflexive","address":"192.0.2.1:5000"}]}"#
    );
    assert_eq!(serde_json::from_str::<DirectOfferBody>(&json).unwrap(), body);
    let hard = DirectOfferBody {
      maybe_nat: Some(DirectNatKind::Hard),
      maybe_probe_token: Some(Base64Bytes::new(vec![2; 2])),
      candidates: Vec::new(),
      ..body
    };
    assert_eq!(
      serde_json::to_string(&hard).unwrap(),
      r#"{"counter":2,"certificate_hash":"AQ==","candidates":[],"nat":"hard","probe_token":"AgI="}"#
    );
    let newer: DirectOfferBody =
      serde_json::from_str(r#"{"counter":1,"certificate_hash":"AQ==","candidates":[],"nat":"cone"}"#)
        .unwrap();
    assert_eq!(newer.maybe_nat, Some(DirectNatKind::Unknown("cone".into())));
    let newer: DirectCandidate = serde_json::from_str(r#"{"kind":"relay","address":"192.0.2.1:1"}"#).unwrap();
    assert_eq!(newer.kind, DirectCandidateKind::Unknown("relay".into()));
  }

  #[test]
  fn roles_map_to_standing() {
    assert_eq!(CallPeerStanding::of(&Role::Owner), Member);
    assert_eq!(CallPeerStanding::of(&Role::Member), Member);
    assert_eq!(CallPeerStanding::of(&Role::Guest), Outsider);
    assert_eq!(CallPeerStanding::of(&Role::Unknown("auditor".into())), Outsider);
  }
}
