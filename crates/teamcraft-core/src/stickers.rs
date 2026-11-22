//! Sticker packs of the workspace (issue #2132, e2ee.md §4.14): signing a pack's document on an
//! admin's device, verifying the server's list against this device's own verified roster, and
//! keeping the verified packs and their pictures on the device.
//!
//! The custom emoji model (§4.13, [`crate::custom_emoji`]) applies to every version of a pack: it
//! counts when its document is signed by a device of somebody who was an owner or admin of the
//! verified roster when it was signed. Of the versions the server lists for one pack, the highest
//! that verifies counts. The server's word (its list, its `name` and `version` fields) counts for
//! nothing; a picture is kept only when its SHA-256 is the one a verified document names. Pictures
//! stay on the device, by hash, after their sticker left the set, so earlier messages keep showing
//! them: those of the set, and up to [`MAX_KEPT_REMOVED_PICTURES`] more.

use std::collections::BTreeMap;

use teamcraft_crypto::identity::{DeviceKeyPair, SignedDeviceCertificate};
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, sign_log_entry, verify_log_entry};
use teamcraft_proto::{
  Base64Bytes, CustomEmojiHash, MAX_STICKERS_PER_PACK_LIMIT, SignedStickerPack, StickerPackDocument,
  StickerPackKey, StickerPackResponse, StickerSet, VerifiedStickerPack,
};

use crate::asset_ledger::AssetLedger;
use crate::convert;
pub use crate::custom_emoji::MAX_KEPT_REMOVED_PICTURES;
use crate::custom_emoji::{OneKey, PictureCache, SignedUnder, asset_signer};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::WorkspaceState;

/// The verified packs, in [`Table::Workspace`].
const SET_KEY: &str = "sticker_packs";
const PICTURES: PictureCache = PictureCache::new("sticker_pictures", "sticker_picture/");

/// Signs a pack's document for the server to keep and the members' devices to verify.
pub(crate) fn sign_sticker_pack(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  document: &StickerPackDocument,
) -> Result<SignedStickerPack, CoreError> {
  let payload = serde_json::to_vec(document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  let signature = sign_log_entry(
    device,
    certificate,
    WorkspaceLog::StickerPack,
    document.version,
    &payload,
  )?;
  Ok(SignedStickerPack {
    payload: Base64Bytes::new(payload),
    signature: Base64Bytes::new(signature),
  })
}

/// The packs of the server's list whose documents verify against `workspace`, in the list's order.
/// Of two versions of one pack (a server mixing old and new documents), the higher wins.
/// Every pack version accepted is noted in `ledger`.
pub fn verified_packs(
  listed: &[StickerPackResponse],
  workspace: &WorkspaceState,
  ledger: &mut AssetLedger,
) -> Vec<VerifiedStickerPack> {
  let mut order: Vec<StickerPackKey> = Vec::new();
  let mut by_key: BTreeMap<StickerPackKey, VerifiedStickerPack> = BTreeMap::new();
  for response in listed {
    let Some(pack) = verify(response, workspace, ledger) else {
      tracing::debug!(pack = %response.pack, "a sticker pack's signature doesn't verify; left out");
      continue;
    };
    ledger.hold(response.signed.payload.as_slice());
    match by_key.get(&pack.pack) {
      Some(known) if (known.version, known.signed_at) >= (pack.version, pack.signed_at) => {},
      Some(_) => {
        by_key.insert(pack.pack, pack);
      },
      None => {
        order.push(pack.pack);
        by_key.insert(pack.pack, pack);
      },
    }
  }
  order.into_iter().filter_map(|key| by_key.remove(&key)).collect()
}

/// One listed pack, if an owner or admin of the verified roster signed its document (or a former
/// one and `ledger` admits it) and the document keeps the rules every device checks.
pub fn verify(
  response: &StickerPackResponse,
  workspace: &WorkspaceState,
  ledger: &AssetLedger,
) -> Option<VerifiedStickerPack> {
  let document: StickerPackDocument = serde_json::from_slice(response.signed.payload.as_slice()).ok()?;
  // A pack an older, higher limit allowed stays valid: devices check the hard ceiling only.
  document.check(MAX_STICKERS_PER_PACK_LIMIT).ok()?;
  let signer = claimed_signer(response.signed.signature.as_slice()).ok()?;
  let signer_user = convert::user_from_crypto(signer).ok()?;
  let under = SignedUnder::of(document.maybe_roster_version, document.signed_at_ms);
  let signed_by = asset_signer(workspace, signer_user, under, |key| {
    verify_log_entry(
      response.signed.signature.as_slice(),
      WorkspaceLog::StickerPack,
      document.version,
      response.signed.payload.as_slice(),
      &OneKey(signer, key),
    )
    .is_ok()
  })?;
  ledger
    .admits(response.signed.payload.as_slice(), signed_by)
    .then_some(VerifiedStickerPack {
      pack: document.pack,
      version: document.version,
      name: document.name,
      stickers: document.stickers,
      signed_by: signer_user,
      signed_at: document.signed_at_ms,
    })
}

/// The packs this device verified last; empty before the first read.
pub fn stored_set(store: &impl LocalStore) -> Result<StickerSet, CoreError> {
  Ok(records::get_json(store, Table::Workspace, SET_KEY)?.unwrap_or_default())
}

pub(crate) fn store_set(store: &impl LocalStore, set: &StickerSet) -> Result<(), CoreError> {
  records::put_json(store, Table::Workspace, SET_KEY, set)
}

/// A picture this device holds, checked against its hash when it was kept.
pub fn picture(store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<Option<Vec<u8>>, CoreError> {
  PICTURES.get(store, sha256)
}

pub(crate) fn has_picture(store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<bool, CoreError> {
  PICTURES.has(store, sha256)
}

/// Keeps a downloaded picture if it is the one `sha256` names, and forgets the pictures of stickers
/// gone from `set` beyond [`MAX_KEPT_REMOVED_PICTURES`], oldest first. Returns whether it was kept.
pub(crate) fn keep_picture(
  store: &impl LocalStore,
  set: &StickerSet,
  sha256: CustomEmojiHash,
  bytes: Vec<u8>,
) -> Result<bool, CoreError> {
  PICTURES.keep(store, |hash| set.has_picture(hash), sha256, bytes)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_crypto::ids as crypto;
  use teamcraft_proto::{CustomEmojiFormat, Id, Snowflake, StickerEntry, UnixMillis, UserId};

  use super::*;
  use crate::store::MemoryStore;
  use crate::workspace::{AdminHistory, AdminTenure, RosterDocument, RosterMember, RosterRole, TenureEnd};

  const PACK: StickerPackKey = StickerPackKey::from_bytes([3; 16]);

  struct Signer {
    user: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  impl Signer {
    fn new(raw: i64) -> Self {
      let user = UserId::from_snowflake(Snowflake::new(raw).unwrap());
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(convert::user(user), crypto::DeviceId::new(7), &mut OsRng);
      let certificate = identity
        .certify_device(convert::user(user), &device.public_keys())
        .unwrap();
      Self {
        user,
        identity,
        device,
        certificate,
      }
    }

    fn key(&self) -> Base64Bytes {
      Base64Bytes::new(self.identity.public_key().as_bytes().to_vec())
    }

    /// A document from before the roster version: judged by its signing time.
    fn listed(&self, version: u64, name: &str, signed_at_ms: i64) -> StickerPackResponse {
      self.listed_under(version, name, signed_at_ms, None)
    }

    fn listed_under(
      &self,
      version: u64,
      name: &str,
      signed_at_ms: i64,
      maybe_roster_version: Option<u64>,
    ) -> StickerPackResponse {
      let document = StickerPackDocument {
        maybe_roster_version,
        ..document(version, name, signed_at_ms)
      };
      StickerPackResponse {
        pack: PACK,
        version,
        name: name.to_owned(),
        maybe_updated_by: Some(self.user),
        updated_at: UnixMillis(0),
        signed: sign_sticker_pack(&self.device, &self.certificate, &document).unwrap(),
      }
    }
  }

  fn document(version: u64, name: &str, signed_at_ms: i64) -> StickerPackDocument {
    StickerPackDocument {
      pack: PACK,
      version,
      name: name.to_owned(),
      stickers: vec![StickerEntry {
        name: "wave".into(),
        alt: "Waving hand".into(),
        sha256: CustomEmojiHash::from_bytes([version as u8; 32]),
        format: CustomEmojiFormat::Png,
      }],
      signed_at_ms,
      maybe_roster_version: None,
    }
  }

  fn workspace(members: &[(&Signer, RosterRole)]) -> WorkspaceState {
    WorkspaceState {
      roster: RosterDocument {
        members: members
          .iter()
          .map(|(signer, role)| RosterMember {
            user_id: signer.user,
            identity_key: signer.key(),
            role: role.clone(),
          })
          .collect(),
        ..RosterDocument::default()
      },
      ..WorkspaceState::default()
    }
  }

  #[test]
  fn an_admins_pack_verifies_and_a_members_does_not() {
    let (admin, member) = (Signer::new(1), Signer::new(2));
    let state = workspace(&[(&admin, RosterRole::Admin), (&member, RosterRole::Unknown)]);
    assert!(verify(&admin.listed(1, "Team", 5), &state, &AssetLedger::default()).is_some());
    assert!(verify(&member.listed(1, "Fake", 5), &state, &AssetLedger::default()).is_none());
    // Somebody the roster doesn't know at all.
    assert!(
      verify(
        &Signer::new(3).listed(1, "Fake", 5),
        &state,
        &AssetLedger::default()
      )
      .is_none()
    );
  }

  mod rejection {
    use super::*;

    /// A document changed after signing (a sticker's picture swapped, the pack renamed) doesn't
    /// verify; the server's own `name` and `version` fields count for nothing.
    #[test]
    fn a_tampered_document_does_not_verify() {
      let admin = Signer::new(1);
      let state = workspace(&[(&admin, RosterRole::Owner)]);
      let mut listed = admin.listed(1, "Team", 5);
      listed.name = "renamed_by_server".into();
      listed.version = 9;
      let pack = verify(&listed, &state, &AssetLedger::default()).unwrap();
      assert_eq!((pack.name.as_str(), pack.version), ("Team", 1));

      let mut swapped = document(1, "Team", 5);
      swapped.stickers[0].sha256 = CustomEmojiHash::from_bytes([99; 32]);
      listed.signed.payload = Base64Bytes::new(serde_json::to_vec(&swapped).unwrap());
      assert!(verify(&listed, &state, &AssetLedger::default()).is_none());
    }

    #[test]
    fn an_unsigned_or_garbled_signature_does_not_verify() {
      let admin = Signer::new(1);
      let state = workspace(&[(&admin, RosterRole::Owner)]);
      let mut listed = admin.listed(1, "Team", 5);
      listed.signed.signature = Base64Bytes::new(Vec::new());
      assert!(verify(&listed, &state, &AssetLedger::default()).is_none());
      let mut flipped = admin.listed(1, "Team", 5);
      let mut bytes = flipped.signed.signature.as_slice().to_vec();
      let last = bytes.len() - 1;
      bytes[last] ^= 1;
      flipped.signed.signature = Base64Bytes::new(bytes);
      assert!(verify(&flipped, &state, &AssetLedger::default()).is_none());
    }

    /// Signed by the right person, but as a custom emoji, or with another version than the one it
    /// claims.
    #[test]
    fn a_signature_of_another_kind_or_version_does_not_verify() {
      let admin = Signer::new(1);
      let state = workspace(&[(&admin, RosterRole::Owner)]);
      let document = document(2, "Team", 5);
      let payload = serde_json::to_vec(&document).unwrap();
      for (log, version) in [(WorkspaceLog::CustomEmoji, 2), (WorkspaceLog::StickerPack, 1)] {
        let signature = sign_log_entry(&admin.device, &admin.certificate, log, version, &payload).unwrap();
        let listed = StickerPackResponse {
          signed: SignedStickerPack {
            payload: Base64Bytes::new(payload.clone()),
            signature: Base64Bytes::new(signature),
          },
          ..admin.listed(2, "Team", 5)
        };
        assert!(
          verify(&listed, &state, &AssetLedger::default()).is_none(),
          "{log:?} {version}"
        );
      }
    }

    /// An identity key other than the roster's for the signer: a forged certificate.
    #[test]
    fn a_wrongly_signed_pack_does_not_verify() {
      let admin = Signer::new(1);
      let impostor = Signer::new(1);
      let state = workspace(&[(&admin, RosterRole::Admin)]);
      assert!(verify(&impostor.listed(1, "Team", 5), &state, &AssetLedger::default()).is_none());
    }

    /// A pack that breaks the document's rules (two stickers of one name) is left out even when an
    /// admin signed it.
    #[test]
    fn a_pack_breaking_the_rules_does_not_verify() {
      let admin = Signer::new(1);
      let state = workspace(&[(&admin, RosterRole::Admin)]);
      let mut document = document(1, "Team", 5);
      document.stickers.push(document.stickers[0].clone());
      let listed = StickerPackResponse {
        signed: sign_sticker_pack(&admin.device, &admin.certificate, &document).unwrap(),
        ..admin.listed(1, "Team", 5)
      };
      assert!(verify(&listed, &state, &AssetLedger::default()).is_none());
    }
  }

  #[test]
  fn the_highest_verified_version_counts() {
    let (admin, member) = (Signer::new(1), Signer::new(2));
    let state = workspace(&[(&admin, RosterRole::Admin)]);
    let packs = verified_packs(
      &[
        admin.listed(2, "Second", 6),
        admin.listed(1, "First", 5),
        // A higher version nobody allowed signed is ignored.
        member.listed(3, "Forged", 7),
      ],
      &state,
      &mut AssetLedger::default(),
    );
    assert_eq!(packs.len(), 1);
    assert_eq!((packs[0].name.as_str(), packs[0].version), ("Second", 2));
  }

  #[test]
  fn a_former_admins_pack_counts_for_what_they_signed_while_admin() {
    let former = Signer::new(1);
    let mut state = workspace(&[]);
    state.admin_history = AdminHistory {
      is_complete: true,
      tenures: vec![AdminTenure {
        user_id: former.user,
        identity_key: former.key(),
        is_owner: false,
        maybe_from_ms: Some(100),
        maybe_from_version: Some(2),
        maybe_until: Some(TenureEnd {
          version: 3,
          maybe_at_ms: Some(200),
        }),
      }],
      maybe_replayed_at: None,
    };
    assert!(verify(&former.listed(1, "Early", 150), &state, &AssetLedger::default()).is_some());
    assert!(verify(&former.listed(2, "Late", 250), &state, &AssetLedger::default()).is_none());

    // Issue #2290: with the roster version named, the signing time no longer decides. Version 3
    // ended the tenure, so a document naming it (or later) is refused even with an early time,
    // and one before the tenure began is a version the signer never held as an admin.
    state.roster.version = 4;
    assert!(
      verify(
        &former.listed_under(3, "Early", 9_999, Some(2)),
        &state,
        &AssetLedger::default()
      )
      .is_some()
    );
    for version in [0, 1, 3, 4, 5] {
      assert!(
        verify(
          &former.listed_under(4, "Backdated", 150, Some(version)),
          &state,
          &AssetLedger::default()
        )
        .is_none(),
        "roster {version}"
      );
    }
  }

  /// Issue #2290 on a device that followed the demotion: the pack version it held counts, a new
  /// one the demoted admin signs (whatever time or roster version it names) doesn't.
  #[test]
  fn a_demoted_admins_new_pack_version_is_refused_where_the_demotion_was_followed() {
    let former = Signer::new(1);
    let mut before = workspace(&[(&former, RosterRole::Admin)]);
    before.roster.version = 2;
    let mut after = workspace(&[]);
    after.roster.version = 3;
    after.admin_history = AdminHistory {
      is_complete: true,
      tenures: vec![AdminTenure {
        user_id: former.user,
        identity_key: former.key(),
        is_owner: false,
        maybe_from_ms: Some(100),
        maybe_from_version: Some(1),
        maybe_until: Some(TenureEnd {
          version: 3,
          maybe_at_ms: Some(200),
        }),
      }],
      maybe_replayed_at: None,
    };
    let mut ledger = AssetLedger::default();
    ledger.follow(1);
    let held = former.listed_under(1, "Team", 150, Some(2));
    assert_eq!(
      verified_packs(std::slice::from_ref(&held), &before, &mut ledger).len(),
      1
    );
    let packs = verified_packs(
      &[held, former.listed_under(2, "Backdated", 150, Some(2))],
      &after,
      &mut ledger,
    );
    assert_eq!(packs.len(), 1);
    assert_eq!((packs[0].name.as_str(), packs[0].version), ("Team", 1));
  }

  #[test]
  fn pictures_are_kept_only_with_their_hash() {
    let store = MemoryStore::new();
    let bytes = b"sticker".to_vec();
    let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&bytes));
    let set = StickerSet::default();
    assert!(!keep_picture(&store, &set, CustomEmojiHash::from_bytes([1; 32]), bytes.clone()).unwrap());
    assert!(keep_picture(&store, &set, sha256, bytes.clone()).unwrap());
    assert_eq!(picture(&store, sha256).unwrap(), Some(bytes));
    // Kept apart from the custom emoji's pictures.
    assert_eq!(crate::custom_emoji::picture(&store, sha256).unwrap(), None);
  }
}
