use std::cell::Cell;
use std::collections::HashMap;

use super::*;
use crate::identity::VerifiedDevice;
use crate::membership::tests::TestUser;
use crate::membership::{MembershipAction, MembershipAuthority};

const CONVERSATION: ConversationId = ConversationId::new(42);
const MEETING: MeetingId = MeetingId::new(9);
const SPACE: SpaceId = SpaceId::new(5);
const OTHER_SPACE: SpaceId = SpaceId::new(6);
const FOUNDER: u64 = 1;
const ADMIN: u64 = 2;
const MEMBER: u64 = 3;
const STRANGER: u64 = 4;

mod encoding {
  use super::*;

  #[test]
  fn an_anchor_round_trips_and_its_pin_leaves_out_the_signature() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let bytes = anchor.to_bytes().unwrap();
    let parsed = ChannelAnchor::from_bytes(&bytes, DEFAULT_MAX_ANCHOR_BYTES).unwrap();
    assert_eq!(parsed, anchor);

    // Another device of the founder signs the same content: another signature, the same pin.
    let second = world.founder.new_device(150);
    let resigned = ChannelAnchor::found(
      &second.device,
      &second.certificate,
      &world.founding(AnchorKind::Channel, "general"),
    )
    .unwrap();
    assert_ne!(resigned.founding_entry, anchor.founding_entry);
    assert_eq!(resigned.founding_hash().unwrap(), anchor.founding_hash().unwrap());
  }

  #[test]
  fn too_large_empty_truncated_and_trailing_input_is_refused() {
    let world = World::new();
    let bytes = world.found(AnchorKind::Channel, "general").to_bytes().unwrap();
    assert!(matches!(
      ChannelAnchor::from_bytes(&bytes, bytes.len() - 1),
      Err(AnchorError::TooLarge { .. })
    ));
    assert!(matches!(
      ChannelAnchor::from_bytes(&[], DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::Malformed(_))
    ));
    assert!(matches!(
      ChannelAnchor::from_bytes(&bytes[..bytes.len() - 1], DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::Malformed(_))
    ));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
      ChannelAnchor::from_bytes(&trailing, DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::Malformed(_))
    ));
  }

  #[test]
  fn a_newer_format_kind_or_origin_asks_for_an_update() {
    let world = World::new();
    let bytes = world.found(AnchorKind::Channel, "general").to_bytes().unwrap();
    let mut format = bytes.clone();
    format[0] = 2;
    assert!(matches!(
      ChannelAnchor::from_bytes(&format, DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::MustUpdate)
    ));
    // format (1) ‖ conversation (8) ‖ kind (1)
    let mut kind = bytes.clone();
    kind[9] = 3;
    assert!(matches!(
      ChannelAnchor::from_bytes(&kind, DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::MustUpdate)
    ));
    // … ‖ origin (1), right after a channel's kind
    let mut origin = bytes;
    origin[10] = 4;
    assert!(matches!(
      ChannelAnchor::from_bytes(&origin, DEFAULT_MAX_ANCHOR_BYTES),
      Err(AnchorError::MustUpdate)
    ));
  }

  #[test]
  fn the_hashes_are_labelled_length_prefixed_and_stable() {
    // Known answer, computed apart: SHA-256(25 ‖ label ‖ 7 ‖ "general"). Part of every pin.
    assert_eq!(
      name_hash("general").to_hex(),
      "e0c5e1a616c7db0ab0a95802b91233f845bc3d1194ef04a22aebee68280c9174"
    );
    assert_ne!(name_hash("general"), name_hash("general "));
    assert_ne!(name_hash(""), AnchorHash::ZERO);
    // The label keeps a name's hash apart from a properties hash of the same bytes.
    let encoded = VLByteSlice(b"x").tls_serialize_detached().unwrap();
    assert_ne!(name_hash("x"), labeled_hash(domain::CHANNEL_PROPERTIES, &encoded));
  }

  #[test]
  fn a_hash_survives_the_account_data_as_hex() {
    let hash = name_hash("general");
    let json = serde_json::to_string(&hash).unwrap();
    assert_eq!(json, format!("\"{}\"", hash.to_hex()));
    assert_eq!(serde_json::from_str::<AnchorHash>(&json).unwrap(), hash);
    assert!(serde_json::from_str::<AnchorHash>("\"zz\"").is_err());
    assert!(serde_json::from_str::<AnchorHash>(&format!("\"{}\"", "é".repeat(32))).is_err());
  }
}

mod founding {
  use super::*;

  #[test]
  fn a_founded_channel_verifies_with_the_founder_and_confirmed_properties() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let verified = world.verify(&anchor, ExpectedKind::Channel, 0).unwrap();
    assert_eq!(
      verified.founder().map(|(user, _)| user),
      Some(UserId::new(FOUNDER))
    );
    assert_eq!(verified.properties().version, 1);
    assert_eq!(verified.properties().maybe_space_id, Some(SPACE));
    assert_eq!(verified.space_confidence(), Confidence::Confirmed);
    assert!(verified.name_counts_for_deletion(Some(name_hash("general"))));
    assert!(!verified.name_counts_for_deletion(Some(name_hash("old-test"))));
    assert!(
      !verified.name_counts_for_deletion(None),
      "a deletion without a name hash is a mismatch on an anchored channel (D3)"
    );
  }

  #[test]
  fn another_conversation_kind_or_meeting_is_refused() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let mut expected = world.expectation(ExpectedKind::Channel);
    expected.conversation_id = ConversationId::new(43);
    assert!(matches!(
      anchor.verify(&expected, 0, &world.tree, &world.directory, &world.authority),
      Err(AnchorError::WrongConversation { .. })
    ));
    assert!(matches!(
      world.verify(&anchor, world.meeting_chat(FOUNDER), 0),
      Err(AnchorError::WrongKind)
    ));
    let meeting = world.found(AnchorKind::MeetingChat { meeting_id: MEETING }, "standup");
    assert!(matches!(
      world.verify(&meeting, ExpectedKind::Channel, 0),
      Err(AnchorError::WrongKind)
    ));
    let other_meeting = ExpectedKind::MeetingChat {
      meeting_id: MeetingId::new(10),
      organizer: UserId::new(FOUNDER),
    };
    assert!(matches!(
      world.verify(&meeting, other_meeting, 0),
      Err(AnchorError::WrongKind)
    ));
  }

  #[test]
  fn a_meeting_chat_is_founded_by_the_organizer_the_server_names() {
    let world = World::new();
    let meeting = world.found(AnchorKind::MeetingChat { meeting_id: MEETING }, "standup");
    world.verify(&meeting, world.meeting_chat(FOUNDER), 0).unwrap();
    // An invitee who places the organizer's leaf first can't sign as the organizer (D10).
    assert!(matches!(
      world.verify(&meeting, world.meeting_chat(MEMBER), 0),
      Err(AnchorError::NotTheOrganizer)
    ));
  }

  #[test]
  fn a_tampered_founding_or_signature_does_not_verify() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let mut moved = anchor.clone();
    moved.founding_properties.maybe_space_id = Some(OTHER_SPACE);
    assert!(matches!(
      world.verify(&moved, ExpectedKind::Channel, 0),
      Err(AnchorError::FoundingSignature(_))
    ));
    let mut later = anchor.clone();
    later.founding.signed_at_ms += 1;
    assert!(matches!(
      world.verify(&later, ExpectedKind::Channel, 0),
      Err(AnchorError::FoundingSignature(_))
    ));
    let mut damaged = anchor;
    let mut entry = damaged.founding_entry.as_slice().to_vec();
    let last = entry.len() - 1;
    entry[last] ^= 1;
    damaged.founding_entry = VLBytes::new(entry);
    assert!(matches!(
      world.verify(&damaged, ExpectedKind::Channel, 0),
      Err(AnchorError::FoundingSignature(_))
    ));
  }

  #[test]
  fn a_founding_under_another_key_than_the_pinned_one_is_refused() {
    let world = World::new();
    // Somebody makes up a founder identity for user 1 and signs with it.
    let impostor = TestUser::new(FOUNDER);
    let anchor = ChannelAnchor::found(
      &impostor.device,
      &impostor.certificate,
      &world.founding(AnchorKind::Channel, "general"),
    )
    .unwrap();
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 0),
      Err(AnchorError::PinnedKeyDiffers(user)) if user == UserId::new(FOUNDER)
    ));
    // Unpinned, the key is trust on first use (§3): it verifies.
    let mut unpinned = World::new();
    unpinned.directory.pins.remove(&UserId::new(FOUNDER));
    unpinned.verify(&anchor, ExpectedKind::Channel, 0).unwrap();
  }

  #[test]
  fn the_founder_device_must_be_on_the_signed_device_list() {
    let mut world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    world.directory.listing = Standing::Unknown;
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 0),
      Err(AnchorError::DeviceNotListed)
    ));
    world.directory.listing = Standing::Refused;
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 0),
      Err(AnchorError::DeviceRevoked)
    ));
  }

  #[test]
  fn revocation_is_judged_as_of_the_anchoring() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    world.verify(&anchor, ExpectedKind::Channel, 32).unwrap();
    assert_eq!(world.directory.judged.get(), Some(Judged::Newest));
    world.verify(&anchor, ExpectedKind::Channel, 33).unwrap();
    assert_eq!(world.directory.judged.get(), Some(Judged::AsNamed));
  }

  #[test]
  fn founding_properties_must_be_version_one_by_the_founder() {
    let world = World::new();
    let edits: [fn(&mut ChannelProperties); 4] = [
      |properties: &mut ChannelProperties| properties.version = 2,
      |properties: &mut ChannelProperties| properties.prev_hash = name_hash("x"),
      |properties: &mut ChannelProperties| properties.name_set_by.authority = NameAuthority::Member,
      |properties: &mut ChannelProperties| properties.name_set_by.user_id = UserId::new(MEMBER),
    ];
    for edit in edits {
      let anchor = world.found_with(edit);
      assert!(
        matches!(
          world.verify(&anchor, ExpectedKind::Channel, 0),
          Err(AnchorError::FoundingProperties)
        ),
        "{anchor:?}"
      );
    }
  }

  #[test]
  fn a_review_needs_a_workspace_signer_and_no_future_epoch() {
    let mut world = World::new();
    let anchor = world.reviewed(7);
    let verified = world.verify(&anchor, ExpectedKind::Channel, 7).unwrap();
    assert_eq!(verified.founder(), None, "a review names no founder");
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 6),
      Err(AnchorError::ReviewFromTheFuture { .. })
    ));
    world
      .authority
      .admins
      .insert(UserId::new(ADMIN), Standing::Unknown);
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 7),
      Err(AnchorError::RosterUnverifiable(3))
    ));
    world
      .authority
      .admins
      .insert(UserId::new(ADMIN), Standing::Refused);
    assert!(matches!(
      world.verify(&anchor, ExpectedKind::Channel, 7),
      Err(AnchorError::SignerNotAdmin(_))
    ));
  }
}

mod changes {
  use super::*;

  #[test]
  fn a_workspace_admin_moves_and_renames() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let signed = world.change(
      &anchor,
      ADMIN,
      NameAuthority::Workspace,
      Some(OTHER_SPACE),
      "announcements",
      4,
    );
    assert_eq!(
      anchor
        .check_change(&signed, 4, &world.tree, &world.directory, &world.authority)
        .unwrap(),
      NameAuthority::Workspace
    );
    let changed = anchor.with_change(signed);
    let verified = world.verify(&changed, ExpectedKind::Channel, 5).unwrap();
    assert_eq!(verified.properties().version, 2);
    assert_eq!(verified.space_confidence(), Confidence::Confirmed);
    assert!(verified.name_counts_for_deletion(Some(name_hash("announcements"))));
    assert_eq!(
      verified.founding_hash(),
      anchor.founding_hash().unwrap(),
      "the founding stays"
    );
  }

  #[test]
  fn a_member_renames_but_their_name_is_stale_for_deletions() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let signed = world.change(&anchor, MEMBER, NameAuthority::Member, Some(SPACE), "old-test", 4);
    assert_eq!(
      anchor
        .check_change(&signed, 4, &world.tree, &world.directory, &world.authority)
        .unwrap(),
      NameAuthority::Member
    );
    let verified = world
      .verify(&anchor.with_change(signed), ExpectedKind::Channel, 5)
      .unwrap();
    assert_eq!(verified.name_confidence(), Confidence::Confirmed);
    assert_eq!(
      verified.properties().name_set_by,
      NameSetter {
        authority: NameAuthority::Member,
        user_id: UserId::new(MEMBER)
      }
    );
    assert!(!verified.name_counts_for_deletion(Some(name_hash("old-test"))));
  }

  #[test]
  fn a_member_may_not_move_and_a_stranger_may_do_nothing() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let moved = world.change(
      &anchor,
      MEMBER,
      NameAuthority::Member,
      Some(OTHER_SPACE),
      "general",
      4,
    );
    assert!(matches!(
      anchor.check_change(&moved, 4, &world.tree, &world.directory, &world.authority),
      Err(PropertiesChangeError::NotAllowed(_))
    ));
    let renamed = world.change(&anchor, STRANGER, NameAuthority::Member, Some(SPACE), "x", 4);
    assert!(matches!(
      anchor.check_change(&renamed, 4, &world.tree, &world.directory, &world.authority),
      Err(PropertiesChangeError::NotAllowed(_))
    ));
  }

  #[test]
  fn a_member_cannot_claim_a_higher_role_for_their_name() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let claimed = world.change(&anchor, MEMBER, NameAuthority::Workspace, Some(SPACE), "x", 4);
    assert!(matches!(
      anchor.check_change(&claimed, 4, &world.tree, &world.directory, &world.authority),
      Err(PropertiesChangeError::WrongNameSetter)
    ));
  }

  #[test]
  fn a_move_without_a_rename_keeps_the_names_setter() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let signed = world.change(&anchor, ADMIN, NameAuthority::Workspace, None, "general", 4);
    assert_eq!(
      signed.change().properties.name_set_by,
      anchor.properties().name_set_by
    );
    anchor
      .check_change(&signed, 4, &world.tree, &world.directory, &world.authority)
      .unwrap();
  }

  #[test]
  fn replayed_stale_and_foreign_changes_are_refused() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let signed = world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "a", 4);
    let check = |anchor: &ChannelAnchor, signed: &SignedPropertiesChange, epoch| {
      anchor.check_change(signed, epoch, &world.tree, &world.directory, &world.authority)
    };
    assert!(matches!(
      check(&anchor, &signed, 3),
      Err(PropertiesChangeError::Stale { .. })
    ));
    assert!(matches!(
      check(&anchor, &signed, 37),
      Err(PropertiesChangeError::Stale { .. })
    ));
    check(&anchor, &signed, 36).unwrap();
    // Replayed onto the anchor it already changed: the version isn't one up.
    let changed = anchor.with_change(signed.clone());
    assert!(matches!(
      check(&changed, &signed, 5),
      Err(PropertiesChangeError::WrongVersion { .. })
    ));
    // Another founding of the same conversation.
    let other = world.found(AnchorKind::Channel, "other");
    assert!(matches!(
      check(&other, &signed, 5),
      Err(PropertiesChangeError::WrongFounding)
    ));
  }

  #[test]
  fn a_change_must_chain_and_carry_a_valid_signature() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let mut change = world
      .change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "a", 4)
      .change;
    change.properties.prev_hash = AnchorHash::ZERO;
    let unchained =
      SignedPropertiesChange::sign(&world.admin.device, &world.admin.certificate, change).unwrap();
    assert!(matches!(
      anchor.check_change(&unchained, 4, &world.tree, &world.directory, &world.authority),
      Err(PropertiesChangeError::BrokenChain)
    ));
    let mut forged = world.change(
      &anchor,
      ADMIN,
      NameAuthority::Workspace,
      Some(OTHER_SPACE),
      "a",
      4,
    );
    forged.change.properties.maybe_space_id = Some(SpaceId::new(99));
    assert!(matches!(
      anchor.check_change(&forged, 4, &world.tree, &world.directory, &world.authority),
      Err(PropertiesChangeError::Signature(_))
    ));
    let mut wrong_conversation = world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "a", 4);
    wrong_conversation.change.conversation_id = ConversationId::new(43);
    assert!(matches!(
      anchor.check_change(
        &wrong_conversation,
        4,
        &world.tree,
        &world.directory,
        &world.authority
      ),
      Err(PropertiesChangeError::WrongConversation(_))
    ));
  }

  #[test]
  fn a_last_change_it_cannot_confirm_makes_the_property_stale() {
    let mut world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let renamed =
      anchor.with_change(world.change(&anchor, MEMBER, NameAuthority::Member, Some(SPACE), "x", 4));
    // The member has left: the anchor verifies, the name is stale for this device.
    world.tree.remove(&UserId::new(MEMBER));
    let verified = world.verify(&renamed, ExpectedKind::Channel, 5).unwrap();
    assert_eq!(verified.name_confidence(), Confidence::Stale);
    assert_eq!(verified.space_confidence(), Confidence::Stale);

    // A member's change that claims the founder set the name (honest members refused its
    // commit): stale for a device welcomed into it.
    world.tree.insert(UserId::new(MEMBER));
    let mut change = world
      .change(&anchor, MEMBER, NameAuthority::Member, Some(SPACE), "x", 4)
      .change;
    change.properties.name_set_by = anchor.properties().name_set_by;
    let claimed =
      SignedPropertiesChange::sign(&world.member.device, &world.member.certificate, change).unwrap();
    let verified = world
      .verify(&anchor.with_change(claimed), ExpectedKind::Channel, 5)
      .unwrap();
    assert_eq!(verified.name_confidence(), Confidence::Stale);
    assert!(!verified.name_counts_for_deletion(Some(name_hash("x"))));

    // A member's change to another space: a member never moves.
    let mut change = world
      .change(&anchor, MEMBER, NameAuthority::Member, Some(SPACE), "x", 4)
      .change;
    change.properties.maybe_space_id = Some(OTHER_SPACE);
    let moved =
      SignedPropertiesChange::sign(&world.member.device, &world.member.certificate, change).unwrap();
    let verified = world
      .verify(&anchor.with_change(moved), ExpectedKind::Channel, 5)
      .unwrap();
    assert_eq!(verified.space_confidence(), Confidence::Stale);
  }
}

mod floor {
  use super::*;

  #[test]
  fn entering_needs_the_pinned_founding_and_nothing_below_the_floor() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let v1 = world.verify(&anchor, ExpectedKind::Channel, 0).unwrap();
    let v2_anchor = anchor.with_change(world.change(
      &anchor,
      ADMIN,
      NameAuthority::Workspace,
      Some(OTHER_SPACE),
      "general",
      4,
    ));
    let v2 = world.verify(&v2_anchor, ExpectedKind::Channel, 5).unwrap();
    let floor = AnchorFloor::first(&v2, 5);

    floor.check_entering(&v2, 5).unwrap();
    floor.check_entering(&v2, 9).unwrap();
    assert_eq!(
      floor.check_entering(&v1, 9),
      Err(FloorError::OlderProperties { offered: 1, floor: 2 }),
      "a fork that rolls the move back"
    );
    assert_eq!(
      floor.check_entering(&v2, 4),
      Err(FloorError::OlderEpoch { offered: 4, floor: 5 })
    );
    let other_v2 =
      anchor.with_change(world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "x", 4));
    let other_v2 = world.verify(&other_v2, ExpectedKind::Channel, 5).unwrap();
    assert_eq!(
      floor.check_entering(&other_v2, 9),
      Err(FloorError::OtherProperties(2))
    );
    let refounded = world
      .verify(
        &world.found(AnchorKind::Channel, "general2"),
        ExpectedKind::Channel,
        9,
      )
      .unwrap();
    assert_eq!(
      floor.check_entering(&refounded, 9),
      Err(FloorError::OtherFounding)
    );
    assert_eq!(floor.check_inside(&refounded), Err(FloorError::OtherFounding));
    // Inside a held group state only the founding counts: a sibling's floor may be ahead.
    floor.check_inside(&v1).unwrap();
  }

  #[test]
  fn the_floor_only_rises() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let v1 = world.verify(&anchor, ExpectedKind::Channel, 0).unwrap();
    let v2_anchor =
      anchor.with_change(world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "a", 4));
    let v2 = world.verify(&v2_anchor, ExpectedKind::Channel, 5).unwrap();
    let mut floor = AnchorFloor::first(&v1, 0);
    assert!(floor.raise(&v2, 5));
    assert!(!floor.raise(&v1, 3), "lower version and epoch change nothing");
    assert_eq!(floor.properties_version, 2);
    assert_eq!(floor.epoch, 5);
    assert!(floor.grants());
  }

  #[test]
  fn merging_keeps_the_maximum_and_marks_forks() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let v1 = AnchorFloor::first(&world.verify(&anchor, ExpectedKind::Channel, 0).unwrap(), 3);
    let a = anchor.with_change(world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "a", 4));
    let v2a = AnchorFloor::first(&world.verify(&a, ExpectedKind::Channel, 5).unwrap(), 5);
    let b = anchor.with_change(world.change(&anchor, ADMIN, NameAuthority::Workspace, Some(SPACE), "b", 4));
    let v2b = AnchorFloor::first(&world.verify(&b, ExpectedKind::Channel, 5).unwrap(), 6);

    let mut merged = v1;
    assert!(merged.merge(&v2a));
    assert_eq!(merged, v2a);
    assert!(!merged.merge(&v1), "an older copy changes nothing");

    // Two hashes for one version: a fork both orders agree on, no grants.
    let mut left = v2a;
    left.merge(&v2b);
    let mut right = v2b;
    right.merge(&v2a);
    assert_eq!(left.state, FloorState::ForkedProperties);
    assert_eq!(right.state, FloorState::ForkedProperties);
    assert_eq!(left.epoch, 6);
    assert!(!left.grants());
    // A device that only saw the older version still learns of the fork.
    let mut older = v1;
    older.merge(&left);
    assert_eq!(older.state, FloorState::ForkedProperties);

    // A higher version settles it.
    let c = a.with_change(world.change(&a, ADMIN, NameAuthority::Workspace, Some(SPACE), "c", 6));
    let v3 = world.verify(&c, ExpectedKind::Channel, 7).unwrap();
    assert!(left.raise(&v3, 7));
    assert!(left.grants());

    // Two foundings: only a re-founding settles that.
    let mut forked = v2a;
    let other = AnchorFloor::first(
      &world
        .verify(&world.found(AnchorKind::Channel, "x"), ExpectedKind::Channel, 0)
        .unwrap(),
      0,
    );
    assert!(forked.merge(&other));
    assert_eq!(forked.state, FloorState::ForkedFounding);
    assert_eq!(
      forked.founding_hash, v2a.founding_hash,
      "the device keeps its own record"
    );
    forked.raise(&v3, 8);
    assert!(!forked.grants());
    let mut agreed = v2a;
    agreed.merge(&forked);
    assert_eq!(agreed.state, FloorState::ForkedFounding);
  }

  #[test]
  fn a_floor_survives_the_account_data() {
    let world = World::new();
    let anchor = world.found(AnchorKind::Channel, "general");
    let mut floor = AnchorFloor::first(&world.verify(&anchor, ExpectedKind::Channel, 0).unwrap(), 4);
    let json = serde_json::to_string(&floor).unwrap();
    assert!(!json.contains("state"), "an agreed floor stays small: {json}");
    assert_eq!(serde_json::from_str::<AnchorFloor>(&json).unwrap(), floor);
    floor.state = FloorState::ForkedFounding;
    let json = serde_json::to_string(&floor).unwrap();
    assert!(json.contains("\"forked_founding\""));
    assert_eq!(serde_json::from_str::<AnchorFloor>(&json).unwrap(), floor);
  }
}

/// A founder, a workspace admin and a member in the group; a stranger outside it.
struct World {
  founder: TestUser,
  admin: TestUser,
  member: TestUser,
  stranger: TestUser,
  directory: Directory,
  authority: Authority,
  tree: BTreeSet<UserId>,
}

impl World {
  fn new() -> Self {
    let founder = TestUser::new(FOUNDER);
    let admin = TestUser::new(ADMIN);
    let member = TestUser::new(MEMBER);
    let stranger = TestUser::new(STRANGER);
    let pins = [&founder, &admin, &member, &stranger]
      .into_iter()
      .map(|user| (user.user_id, user.identity.public_key()))
      .collect();
    Self {
      directory: Directory {
        pins,
        listing: Standing::Confirmed,
        judged: Cell::new(None),
      },
      authority: Authority {
        admins: HashMap::from([(UserId::new(ADMIN), Standing::Confirmed)]),
      },
      tree: [FOUNDER, MEMBER].into_iter().map(UserId::new).collect(),
      founder,
      admin,
      member,
      stranger,
    }
  }

  fn founding<'a>(&self, kind: AnchorKind, name: &'a str) -> Founding<'a> {
    Founding {
      conversation_id: CONVERSATION,
      kind,
      device_list_version: 1,
      signed_at_ms: 1_700_000_000_000,
      maybe_space_id: Some(SPACE),
      name,
    }
  }

  fn found(&self, kind: AnchorKind, name: &str) -> ChannelAnchor {
    ChannelAnchor::found(
      &self.founder.device,
      &self.founder.certificate,
      &self.founding(kind, name),
    )
    .unwrap()
  }

  /// A founding whose first properties `edit` changes before the founder signs them.
  fn found_with(&self, edit: impl FnOnce(&mut ChannelProperties)) -> ChannelAnchor {
    let mut anchor = self.found(AnchorKind::Channel, "general");
    edit(&mut anchor.founding_properties);
    let payload = founding_payload(&anchor.founding, &anchor.founding_properties).unwrap();
    let entry = sign_log_entry(
      &self.founder.device,
      &self.founder.certificate,
      WorkspaceLog::ChannelAnchor,
      1,
      &payload,
    )
    .unwrap();
    anchor.founding_entry = VLBytes::new(entry);
    anchor
  }

  /// An anchor of a channel from before anchors, reviewed by the admin in `epoch` (#1728).
  fn reviewed(&self, epoch: u64) -> ChannelAnchor {
    let founding = ChannelAnchorFounding {
      conversation_id: CONVERSATION,
      kind: AnchorKind::Channel,
      origin: AnchorOrigin::Reviewed {
        reviewer_user_id: self.admin.user_id,
        reviewer_identity_key: self.admin.identity.public_key(),
        roster_version: 3,
        signed_for_epoch: epoch,
        role_log_head: AnchorHash::ZERO,
      },
      device_list_version: 1,
      signed_at_ms: 0,
    };
    let properties = ChannelProperties {
      version: 1,
      maybe_space_id: None,
      name_hash: name_hash("legacy"),
      name_set_by: NameSetter {
        authority: NameAuthority::Workspace,
        user_id: self.admin.user_id,
      },
      prev_hash: AnchorHash::ZERO,
    };
    let payload = founding_payload(&founding, &properties).unwrap();
    let entry = sign_log_entry(
      &self.admin.device,
      &self.admin.certificate,
      WorkspaceLog::ChannelAnchor,
      1,
      &payload,
    )
    .unwrap();
    ChannelAnchor {
      format: ANCHOR_FORMAT,
      founding,
      founding_properties: properties,
      founding_entry: VLBytes::new(entry),
      maybe_last_change: None,
    }
  }

  fn change(
    &self,
    anchor: &ChannelAnchor,
    signer: u64,
    class: NameAuthority,
    maybe_space_id: Option<SpaceId>,
    name: &str,
    epoch: u64,
  ) -> SignedPropertiesChange {
    let user = [&self.founder, &self.admin, &self.member, &self.stranger]
      .into_iter()
      .find(|user| user.user_id == UserId::new(signer))
      .unwrap();
    anchor
      .sign_change(
        &user.device,
        &user.certificate,
        class,
        &PropertiesTarget { maybe_space_id, name },
        epoch,
        3,
      )
      .unwrap()
  }

  fn expectation(&self, kind: ExpectedKind) -> AnchorExpectation {
    AnchorExpectation {
      conversation_id: CONVERSATION,
      kind,
    }
  }

  fn meeting_chat(&self, organizer: u64) -> ExpectedKind {
    ExpectedKind::MeetingChat {
      meeting_id: MEETING,
      organizer: UserId::new(organizer),
    }
  }

  fn verify(
    &self,
    anchor: &ChannelAnchor,
    kind: ExpectedKind,
    epoch: u64,
  ) -> Result<VerifiedAnchor, AnchorError> {
    anchor.verify(
      &self.expectation(kind),
      epoch,
      &self.tree,
      &self.directory,
      &self.authority,
    )
  }
}

struct Directory {
  pins: HashMap<UserId, IdentityPublicKey>,
  listing: Standing,
  /// How the last device list check was judged.
  judged: Cell<Option<Judged>>,
}

impl IdentityDirectory for Directory {
  fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey> {
    self.pins.get(&user_id).copied()
  }

  fn device_listing(&self, _device: &VerifiedDevice, _list_version: u64, judged: Judged) -> Standing {
    self.judged.set(Some(judged));
    self.listing
  }
}

struct Authority {
  admins: HashMap<UserId, Standing>,
}

impl MembershipAuthority for Authority {
  fn is_authorized(
    &self,
    _actor: UserId,
    _conversation_id: ConversationId,
    _action: &MembershipAction,
  ) -> bool {
    false
  }

  fn workspace_signer(&self, user_id: UserId, _roster_version: u64, _judged: Judged) -> Standing {
    self.admins.get(&user_id).copied().unwrap_or(Standing::Refused)
  }
}
