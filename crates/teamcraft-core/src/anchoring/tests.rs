use std::cell::Cell;
use std::collections::BTreeSet;

use rand_core::OsRng;
use teamcraft_crypto::channel_anchor::{
  AnchorExpectation, AnchorKind, ExpectedKind, FloorState, Founding, Judged, NameAuthority, PropertiesTarget,
  Standing,
};
use teamcraft_crypto::identity::{
  DeviceKeyPair, IdentityKeyPair, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice,
};
use teamcraft_crypto::ids::{ConversationId, DeviceId, SpaceId, UserId};
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};

use super::*;

const CONVERSATION: ConversationId = ConversationId::new(40);
const SPACE: SpaceId = SpaceId::new(5);
const FOUNDER: u64 = 1;
const MEMBER: u64 = 2;

mod releases {
  use super::*;

  #[test]
  fn versions_parse_and_compare_by_number() {
    assert_eq!(
      ReleaseVersion::parse("1.0.31"),
      Some(ReleaseVersion::new(1, 0, 31))
    );
    assert_eq!(
      ReleaseVersion::parse("1.2.3-rc.1"),
      Some(ReleaseVersion::new(1, 2, 3))
    );
    assert_eq!(ReleaseVersion::parse("1.0"), None);
    assert_eq!(ReleaseVersion::parse("1.0.x"), None);
    assert_eq!(ReleaseVersion::parse("1.0.0.1"), None);
    assert!(ReleaseVersion::new(1, 0, 100) > ReleaseVersion::new(1, 0, 31));
  }

  /// D2: anchored only once the minimum reaches the release; never without an answer.
  #[test]
  fn channels_are_founded_anchored_from_the_anchoring_release_on() {
    assert!(!founds_anchored(None));
    assert!(!founds_anchored(ReleaseVersion::parse("1.0.30")));
    assert!(founds_anchored(Some(ANCHORING_RELEASE)));
    assert!(founds_anchored(ReleaseVersion::parse("2.0.0")));
  }
}

mod entering {
  use super::*;

  #[test]
  fn the_first_verified_anchor_is_pinned_and_nothing_else_pins() {
    let world = World::new();
    let anchor = world.verified(&world.founded("general"), 0).unwrap();
    let outcome = AnchorOutcome::Verified(Box::new(anchor.clone()));
    assert_eq!(
      judge_entering(None, &outcome, 3),
      AnchorVerdict::Record(AnchorFloor::first(&anchor, 3))
    );
    assert_eq!(
      judge_entering(None, &AnchorOutcome::Absent, 3),
      AnchorVerdict::Unanchored
    );
    let unverified = AnchorOutcome::Unverified {
      maybe_parsed: Some(ParsedAnchor {
        founding_hash: anchor.founding_hash(),
        properties_version: 1,
        properties_hash: anchor.properties_hash(),
      }),
    };
    assert_eq!(judge_entering(None, &unverified, 3), AnchorVerdict::Unanchored);
  }

  /// An add-back or welcome into a group without the anchor, or with another founding.
  #[test]
  fn a_stripped_or_refounded_group_is_refused_where_pinned() {
    let world = World::new();
    let floor = world.floor("general", 3);
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Absent, 5),
      AnchorVerdict::Refuse(FloorError::Stripped)
    );
    let other = world.verified(&world.founded("general again"), 0).unwrap();
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Verified(Box::new(other)), 5),
      AnchorVerdict::Refuse(FloorError::OtherFounding)
    );
  }

  /// The server relists a pinned channel as a DM (or the welcome comes before its record): the
  /// device still checks it, and a stripped or rolled-back state is refused.
  #[test]
  fn a_pinned_conversation_relisted_as_another_kind_is_still_checked() {
    let world = World::new();
    let floor = world.floor("general", 9);
    assert_eq!(
      anchor_expectation(CONVERSATION, false, false),
      None,
      "nothing pinned or carried"
    );
    let expected = anchor_expectation(CONVERSATION, true, false).expect("pinned: checked");
    assert_eq!(expected.kind, ExpectedKind::Channel);
    assert!(
      anchor_expectation(CONVERSATION, false, true).is_some(),
      "carried: checked"
    );
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Absent, 10),
      AnchorVerdict::Refuse(FloorError::Stripped)
    );
    let old = world.verified(&world.founded("general"), 3).unwrap();
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Verified(Box::new(old)), 3),
      AnchorVerdict::Refuse(FloorError::OlderEpoch { offered: 3, floor: 9 })
    );
  }

  /// A tampered anchor doesn't verify: where pinned it is refused unless its founding is the pinned
  /// one (then it is kept, granting nothing, as for a device list not fetched yet).
  #[test]
  fn a_tampered_anchor_grants_nothing_and_another_founding_is_refused() {
    let world = World::new();
    let anchor = world.founded("general");
    let floor = world.floor("general", 3);
    let mut bytes = anchor.to_bytes().unwrap();
    // The founding entry's signature ends right before the (empty) last change.
    let in_signature = bytes.len() - 2;
    bytes[in_signature] ^= 1;
    let tampered = AnchorOutcome::of(
      ChannelAnchor::from_bytes(&bytes, DEFAULT_MAX_ANCHOR_BYTES)
        .map_err(MlsError::from)
        .and_then(|parsed| world.verify(&parsed, 3).map_err(MlsError::from))
        .map(Some),
      Some(&bytes),
    );
    assert!(tampered.verified().is_none());
    assert_eq!(judge_entering(Some(&floor), &tampered, 4), AnchorVerdict::KeepPin);
    let garbage = AnchorOutcome::of(Err(MlsError::NotAnchored), Some(&[1, 2, 3]));
    assert_eq!(
      judge_entering(Some(&floor), &garbage, 4),
      AnchorVerdict::Refuse(FloorError::OtherFounding)
    );
  }

  /// A server can make the anchor unverifiable (withholding a device list): that never gets a
  /// rolled-back state past the floor, an old epoch or old properties alike.
  #[test]
  fn an_unverified_welcome_below_the_floor_is_refused() {
    let world = World::new();
    let founding = world.founded("general");
    let renamed = world.renamed(&founding, "renamed", 1);
    let floor = world.floor_of(&renamed, 1, 9);
    world.listing.set(Standing::Unknown);
    let unverified = |anchor: &ChannelAnchor, epoch| {
      let bytes = anchor.to_bytes().unwrap();
      let outcome = AnchorOutcome::of(world.verified(anchor, epoch).map(Some), Some(&bytes));
      assert!(outcome.verified().is_none(), "the device list is withheld");
      outcome
    };
    assert_eq!(
      judge_entering(Some(&floor), &unverified(&renamed, 4), 4),
      AnchorVerdict::Refuse(FloorError::OlderEpoch { offered: 4, floor: 9 })
    );
    assert_eq!(
      judge_entering(Some(&floor), &unverified(&founding, 12), 12),
      AnchorVerdict::Refuse(FloorError::OlderProperties { offered: 1, floor: 2 })
    );
    let other_rename = world.renamed(&founding, "other", 1);
    assert_eq!(
      judge_entering(Some(&floor), &unverified(&other_rename, 12), 12),
      AnchorVerdict::Refuse(FloorError::OtherProperties(2))
    );
    assert_eq!(
      judge_entering(Some(&floor), &unverified(&renamed, 12), 12),
      AnchorVerdict::KeepPin
    );
  }

  /// A revoked founding device: the anchor doesn't verify, so it grants nothing, also no first pin.
  #[test]
  fn a_revoked_founding_device_pins_nothing() {
    let world = World::new();
    let anchor = world.founded("general");
    world.listing.set(Standing::Refused);
    let result = world.verify(&anchor, 0).map(Some).map_err(MlsError::from);
    let outcome = AnchorOutcome::of(result, Some(&anchor.to_bytes().unwrap()));
    assert!(outcome.verified().is_none());
    assert_eq!(judge_entering(None, &outcome, 0), AnchorVerdict::Unanchored);
  }

  /// A copy of the real founding with old properties, in a fork: refused below the floor.
  #[test]
  fn a_copied_founding_with_old_properties_is_refused_below_the_floor() {
    let world = World::new();
    let founding = world.founded("general");
    let renamed = world.renamed(&founding, "renamed", 1);
    let floor = world.floor_of(&renamed, 1, 5);
    let old = world.verified(&founding, 6).unwrap();
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Verified(Box::new(old)), 6),
      AnchorVerdict::Refuse(FloorError::OlderProperties { offered: 1, floor: 2 })
    );
  }

  /// An old `GroupInfo` for an external commit: its epoch is below the floor.
  #[test]
  fn an_old_group_info_is_refused_below_the_floor_epoch() {
    let world = World::new();
    let floor = world.floor("general", 9);
    let anchor = world.verified(&world.founded("general"), 4).unwrap();
    assert_eq!(
      judge_entering(
        Some(&floor),
        &AnchorOutcome::Verified(Box::new(anchor.clone())),
        4
      ),
      AnchorVerdict::Refuse(FloorError::OlderEpoch { offered: 4, floor: 9 })
    );
    let mut raised = floor;
    raised.epoch = 12;
    assert_eq!(
      judge_entering(Some(&floor), &AnchorOutcome::Verified(Box::new(anchor)), 12),
      AnchorVerdict::Record(raised)
    );
  }
}

mod inside {
  use super::*;

  /// A move or rename inside the group raises the floor, also when a sibling's floor is ahead in
  /// epoch: only the founding is checked there.
  #[test]
  fn the_floor_only_rises_and_a_lagging_device_keeps_up() {
    let world = World::new();
    let founding = world.founded("general");
    let floor = world.floor("general", 20);
    let renamed = world.renamed(&founding, "renamed", 2);
    let verified = world.verified(&renamed, 3).unwrap();
    let AnchorVerdict::Record(raised) =
      judge_inside(Some(&floor), &AnchorOutcome::Verified(Box::new(verified)), 3)
    else {
      panic!("a rename inside raises the floor");
    };
    assert_eq!((raised.properties_version, raised.epoch), (2, 20));
    // The raised floor refuses the old properties when entering again, whatever the epoch.
    let old = world.verified(&founding, 30).unwrap();
    assert_eq!(
      judge_entering(Some(&raised), &AnchorOutcome::Verified(Box::new(old)), 30),
      AnchorVerdict::Refuse(FloorError::OlderProperties { offered: 1, floor: 2 })
    );
    assert_eq!(
      judge_inside(Some(&raised), &AnchorOutcome::Absent, 31),
      AnchorVerdict::Refuse(FloorError::Stripped)
    );
  }
}

mod deletions {
  use super::*;

  #[test]
  fn the_founders_name_counts_when_listed_and_named() {
    let world = World::new();
    let anchor = world.verified(&world.founded("general"), 0).unwrap();
    let hash = Some(name_hash("general"));
    assert!(name_counts_for_deletion(&anchor, true, Some("general"), hash));
    assert!(!is_listed_name_stale(&anchor, "general"));
  }

  /// D3: a deletion without the hash, or naming another name, takes the retention.
  #[test]
  fn a_missing_or_swapped_name_takes_the_retention() {
    let world = World::new();
    let anchor = world.verified(&world.founded("general"), 0).unwrap();
    assert!(!name_counts_for_deletion(&anchor, true, Some("general"), None));
    let old_test = Some(name_hash("old-test"));
    assert!(!name_counts_for_deletion(
      &anchor,
      true,
      Some("general"),
      old_test
    ));
    // The server lists another name: the deleter was shown "old-test" for this channel.
    assert!(!name_counts_for_deletion(
      &anchor,
      true,
      Some("old-test"),
      old_test
    ));
    assert!(is_listed_name_stale(&anchor, "old-test"));
    assert!(!name_counts_for_deletion(
      &anchor,
      true,
      None,
      Some(name_hash("general"))
    ));
  }

  /// D5: a member's rename counts for what the channel is called, never for a purge.
  #[test]
  fn a_members_rename_is_stale_for_deletions() {
    let world = World::new();
    let renamed = world.renamed(&world.founded("general"), "old-test", 1);
    let anchor = world.verified(&renamed, 2).unwrap();
    assert_eq!(anchor.properties().name_set_by.authority, NameAuthority::Member);
    assert!(!name_counts_for_deletion(
      &anchor,
      true,
      Some("old-test"),
      Some(name_hash("old-test"))
    ));
  }

  /// A guest's rename never verifies as a member's: the name is stale.
  #[test]
  fn a_guests_rename_counts_for_nothing() {
    let world = World::new();
    let renamed = world.renamed(&world.founded("general"), "lobby", 1);
    world.is_member_guest.set(true);
    let anchor = world.verified(&renamed, 2).unwrap();
    assert!(!name_counts_for_deletion(
      &anchor,
      true,
      Some("lobby"),
      Some(name_hash("lobby"))
    ));
  }

  #[test]
  fn a_forked_floor_or_a_name_breaking_the_rules_grants_nothing() {
    let world = World::new();
    let anchor = world.verified(&world.founded("general"), 0).unwrap();
    assert!(!name_counts_for_deletion(
      &anchor,
      false,
      Some("general"),
      Some(name_hash("general"))
    ));
    let mut floor = AnchorFloor::first(&anchor, 0);
    floor.state = FloorState::ForkedFounding;
    assert!(!floor.grants());
    // A modified founder app signed an invisible character into the name.
    let invisible = "gen\u{200B}eral";
    let sneaky = world.verified(&world.founded(invisible), 0).unwrap();
    assert!(!name_counts_for_deletion(
      &sneaky,
      true,
      Some(invisible),
      Some(name_hash(invisible))
    ));
  }
}

/// The founder (a member of the channel, not a workspace admin) and a member.
struct World {
  founder: Person,
  member: Person,
  listing: Cell<Standing>,
  is_member_guest: Cell<bool>,
}

struct Person {
  user_id: UserId,
  identity: IdentityKeyPair,
  device: DeviceKeyPair,
  certificate: SignedDeviceCertificate,
}

impl World {
  fn new() -> Self {
    Self {
      founder: Person::new(FOUNDER),
      member: Person::new(MEMBER),
      listing: Cell::new(Standing::Confirmed),
      is_member_guest: Cell::new(false),
    }
  }

  fn founded(&self, name: &str) -> ChannelAnchor {
    ChannelAnchor::found(
      &self.founder.device,
      &self.founder.certificate,
      &Founding {
        conversation_id: CONVERSATION,
        kind: AnchorKind::Channel,
        device_list_version: 1,
        signed_at_ms: 1_700_000_000_000,
        maybe_space_id: Some(SPACE),
        name,
      },
    )
    .unwrap()
  }

  /// The member renames `anchor` in `epoch`.
  fn renamed(&self, anchor: &ChannelAnchor, name: &str, epoch: u64) -> ChannelAnchor {
    let signed = anchor
      .sign_change(
        &self.member.device,
        &self.member.certificate,
        NameAuthority::Member,
        &PropertiesTarget {
          maybe_space_id: Some(SPACE),
          name,
        },
        epoch,
        0,
      )
      .unwrap();
    anchor.with_change(signed)
  }

  fn floor(&self, name: &str, epoch: u64) -> AnchorFloor {
    self.floor_of(&self.founded(name), 0, epoch)
  }

  fn floor_of(&self, anchor: &ChannelAnchor, verified_at: u64, epoch: u64) -> AnchorFloor {
    AnchorFloor::first(&self.verified(anchor, verified_at).unwrap(), epoch)
  }

  fn verified(&self, anchor: &ChannelAnchor, epoch: u64) -> Result<VerifiedAnchor, MlsError> {
    self.verify(anchor, epoch).map_err(MlsError::from)
  }

  fn verify(
    &self,
    anchor: &ChannelAnchor,
    epoch: u64,
  ) -> Result<VerifiedAnchor, teamcraft_crypto::channel_anchor::AnchorError> {
    let tree: BTreeSet<UserId> = [self.founder.user_id, self.member.user_id].into_iter().collect();
    let expected = AnchorExpectation {
      conversation_id: CONVERSATION,
      kind: ExpectedKind::Channel,
    };
    anchor.verify(&expected, epoch, &tree, self, &NoAdmins)
  }
}

impl IdentityDirectory for World {
  fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey> {
    [&self.founder, &self.member]
      .into_iter()
      .find(|person| person.user_id == user_id)
      .map(|person| person.identity.public_key())
  }

  fn device_listing(&self, _device: &VerifiedDevice, _list_version: u64, _judged: Judged) -> Standing {
    self.listing.get()
  }

  fn is_guest(&self, user_id: UserId) -> bool {
    self.is_member_guest.get() && user_id == self.member.user_id
  }
}

/// No workspace admins: everybody is judged as a member.
struct NoAdmins;

impl MembershipAuthority for NoAdmins {
  fn is_authorized(
    &self,
    _actor: UserId,
    _conversation_id: ConversationId,
    _action: &MembershipAction,
  ) -> bool {
    false
  }
}

impl Person {
  fn new(user_id: u64) -> Self {
    let user_id = UserId::new(user_id);
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let device = DeviceKeyPair::generate(user_id, DeviceId::new(user_id.get() * 10), &mut OsRng);
    let certificate = identity.certify_device(user_id, &device.public_keys()).unwrap();
    Self {
      user_id,
      identity,
      device,
      certificate,
    }
  }
}
