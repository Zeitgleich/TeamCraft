//! Custom emoji of the workspace (issue #207, e2ee.md §4.13): signing an emoji's document on an
//! admin's device, verifying the server's list against this device's own verified roster, and
//! keeping the verified set and the pictures on the device.
//!
//! An emoji counts when its document is signed by a device of somebody who was an owner or admin
//! of the verified roster when it was signed: on the roster now, or in a tenure of the admin
//! history that covers the roster version the document names (before the demotion that ended it;
//! issue #2290), or, for a document from before that field, its signing time. A former admin's
//! document counts only if the [`AssetLedger`] admits it: held before a demotion this device
//! followed (both the time and the version are the signer's word). The server's word
//! (its list, its `shortcode` field) counts for nothing; a picture is kept only when its SHA-256 is
//! the one the document names. Pictures stay on the device, by hash, after their emoji left the
//! set, so earlier messages keep showing them: those of the set, and up to
//! [`MAX_KEPT_REMOVED_PICTURES`] more.

use std::collections::BTreeMap;

use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate};
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, sign_log_entry, verify_log_entry};
use teamcraft_proto::custom_emoji_set::{CustomEmoji, CustomEmojiSet};
use teamcraft_proto::{
  Base64Bytes, CustomEmojiDocument, CustomEmojiFormat, CustomEmojiHash, CustomEmojiResponse,
  SignedCustomEmoji, check_custom_emoji_shortcode,
};

use crate::asset_ledger::{AssetLedger, AssetSigner};
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::{AdminTenure, RosterRole, WorkspaceState};

/// The verified set, in [`Table::Workspace`].
const SET_KEY: &str = "custom_emoji";
/// The hashes of the pictures held, oldest use first.
const PICTURES_KEY: &str = "custom_emoji_pictures";
const PICTURE_PREFIX: &str = "custom_emoji_picture/";
/// Pictures of emoji no longer in the set that stay for earlier messages.
pub const MAX_KEPT_REMOVED_PICTURES: usize = 200;

/// Signs an emoji's document for the server to keep and the members' devices to verify.
pub(crate) fn sign_custom_emoji(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  document: &CustomEmojiDocument,
) -> Result<SignedCustomEmoji, CoreError> {
  let payload = serde_json::to_vec(document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  let signature = sign_log_entry(
    device,
    certificate,
    WorkspaceLog::CustomEmoji,
    version_of(document),
    &payload,
  )?;
  Ok(SignedCustomEmoji {
    payload: Base64Bytes::new(payload),
    signature: Base64Bytes::new(signature),
  })
}

/// The emoji of the server's list whose documents verify against `workspace`, by shortcode. Of two
/// with one name (a server mixing old and new documents), the later signed wins.
/// Every emoji accepted is noted in `ledger`.
pub fn verified_set(
  listed: &[CustomEmojiResponse],
  workspace: &WorkspaceState,
  ledger: &mut AssetLedger,
) -> Vec<CustomEmoji> {
  let mut by_name: BTreeMap<String, CustomEmoji> = BTreeMap::new();
  for response in listed {
    let Some(emoji) = verify(response, workspace, ledger) else {
      tracing::debug!(id = %response.id, "a custom emoji's signature doesn't verify; left out");
      continue;
    };
    ledger.hold(response.signed.payload.as_slice());
    match by_name.get(&emoji.shortcode) {
      Some(known) if known.signed_at >= emoji.signed_at => {},
      _ => {
        by_name.insert(emoji.shortcode.clone(), emoji);
      },
    }
  }
  by_name.into_values().collect()
}

/// One listed emoji, if an owner or admin of the verified roster signed its document, or a former
/// one and `ledger` admits it.
pub fn verify(
  response: &CustomEmojiResponse,
  workspace: &WorkspaceState,
  ledger: &AssetLedger,
) -> Option<CustomEmoji> {
  let document: CustomEmojiDocument = serde_json::from_slice(response.signed.payload.as_slice()).ok()?;
  check_custom_emoji_shortcode(&document.shortcode).ok()?;
  if !matches!(document.format, CustomEmojiFormat::Png | CustomEmojiFormat::Webp) {
    return None;
  }
  let signer = claimed_signer(response.signed.signature.as_slice()).ok()?;
  let signer_user = convert::user_from_crypto(signer).ok()?;
  let under = SignedUnder::of(document.maybe_roster_version, document.signed_at_ms);
  let signed_by = asset_signer(workspace, signer_user, under, |key| {
    verify_log_entry(
      response.signed.signature.as_slice(),
      WorkspaceLog::CustomEmoji,
      version_of(&document),
      response.signed.payload.as_slice(),
      &OneKey(signer, key),
    )
    .is_ok()
  })?;
  ledger
    .admits(response.signed.payload.as_slice(), signed_by)
    .then_some(CustomEmoji {
      id: response.id,
      shortcode: document.shortcode,
      sha256: document.sha256,
      format: document.format,
      signed_by: signer_user,
      signed_at: document.signed_at_ms,
    })
}

/// The set this device verified last; empty before the first read.
pub fn stored_set(store: &impl LocalStore) -> Result<CustomEmojiSet, CoreError> {
  Ok(records::get_json(store, Table::Workspace, SET_KEY)?.unwrap_or_default())
}

pub(crate) fn store_set(store: &impl LocalStore, set: &CustomEmojiSet) -> Result<(), CoreError> {
  records::put_json(store, Table::Workspace, SET_KEY, set)
}

/// A picture this device holds, checked against its hash when it was kept.
pub fn picture(store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<Option<Vec<u8>>, CoreError> {
  EMOJI_PICTURES.get(store, sha256)
}

pub(crate) fn has_picture(store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<bool, CoreError> {
  EMOJI_PICTURES.has(store, sha256)
}

/// Keeps a downloaded picture if it is the one `sha256` names, and forgets the pictures of emoji
/// gone from `set` beyond [`MAX_KEPT_REMOVED_PICTURES`], oldest first. Returns whether it was kept.
pub(crate) fn keep_picture(
  store: &impl LocalStore,
  set: &CustomEmojiSet,
  sha256: CustomEmojiHash,
  bytes: Vec<u8>,
) -> Result<bool, CoreError> {
  EMOJI_PICTURES.keep(store, |hash| set.by_hash(hash).is_some(), sha256, bytes)
}

/// Pictures of workspace assets kept by hash in [`Table::Workspace`]: custom emoji, sticker packs
/// (issue #2132). Those no longer in their set stay for earlier messages, up to
/// [`MAX_KEPT_REMOVED_PICTURES`].
pub(crate) struct PictureCache {
  /// The hashes held, oldest use first.
  list_key: &'static str,
  /// Each picture's key: this and its hash in hex.
  prefix: &'static str,
}

const EMOJI_PICTURES: PictureCache = PictureCache {
  list_key: PICTURES_KEY,
  prefix: PICTURE_PREFIX,
};

impl PictureCache {
  pub(crate) const fn new(list_key: &'static str, prefix: &'static str) -> Self {
    Self { list_key, prefix }
  }

  pub(crate) fn get(
    &self,
    store: &impl LocalStore,
    sha256: CustomEmojiHash,
  ) -> Result<Option<Vec<u8>>, CoreError> {
    let maybe_picture: Option<Base64Bytes> = records::get_json(store, Table::Workspace, &self.key(sha256))?;
    Ok(maybe_picture.map(Base64Bytes::into_vec))
  }

  pub(crate) fn has(&self, store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<bool, CoreError> {
    Ok(store.get(Table::Workspace, &self.key(sha256))?.is_some())
  }

  /// Keeps a downloaded picture if it is the one `sha256` names, and forgets pictures for which
  /// `is_current` is false beyond [`MAX_KEPT_REMOVED_PICTURES`], oldest first. Returns whether it
  /// was kept.
  pub(crate) fn keep(
    &self,
    store: &impl LocalStore,
    is_current: impl Fn(CustomEmojiHash) -> bool,
    sha256: CustomEmojiHash,
    bytes: Vec<u8>,
  ) -> Result<bool, CoreError> {
    let actual = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&bytes));
    if actual != sha256 {
      return Ok(false);
    }
    let mut held: Vec<CustomEmojiHash> =
      records::get_json(store, Table::Workspace, self.list_key)?.unwrap_or_default();
    held.retain(|known| *known != sha256);
    held.push(sha256);
    let removed = |hash: &CustomEmojiHash| !is_current(*hash);
    while held.iter().filter(|hash| removed(hash)).count() > MAX_KEPT_REMOVED_PICTURES {
      let Some(at) = held.iter().position(removed) else {
        break;
      };
      let oldest = held.remove(at);
      store.delete(Table::Workspace, &self.key(oldest))?;
    }
    records::put_json(
      store,
      Table::Workspace,
      &self.key(sha256),
      &Base64Bytes::new(bytes),
    )?;
    records::put_json(store, Table::Workspace, self.list_key, &held)?;
    Ok(true)
  }

  fn key(&self, sha256: CustomEmojiHash) -> String {
    format!("{}{}", self.prefix, sha256.to_hex())
  }
}

/// The signature's "version": when the document was signed.
fn version_of(document: &CustomEmojiDocument) -> u64 {
  u64::try_from(document.signed_at_ms).unwrap_or(0)
}

/// What a workspace asset's document (custom emoji, sticker pack) says it was signed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignedUnder {
  /// The version of the verified roster the signing device held (issue #2290): bound to the
  /// owners' signed log, not to the signer's clock.
  Roster(u64),
  /// Only the signer's clock: documents from before the roster version (e2ee.md §4.13,
  /// "Transition").
  Clock(i64),
}

impl SignedUnder {
  pub(crate) fn of(maybe_roster_version: Option<u64>, signed_at_ms: i64) -> Self {
    maybe_roster_version.map_or(Self::Clock(signed_at_ms), Self::Roster)
  }

  /// Whether `tenure` covers this: for a roster version, the tenure began at or before it and the
  /// roster that ended it (the demotion) comes after it. A version the history can't place in a
  /// tenure, before it began or from the demotion on, is one the signer never held as an admin.
  fn is_within(self, tenure: &AdminTenure) -> bool {
    match self {
      Self::Roster(version) => {
        let has_begun = tenure.maybe_from_version.is_none_or(|from| from <= version);
        let has_ended = tenure
          .maybe_until
          .as_ref()
          .is_some_and(|until| until.version <= version);
        has_begun && !has_ended
      },
      Self::Clock(at_ms) => {
        let has_begun = tenure.maybe_from_ms.is_none_or(|from| from <= at_ms);
        let has_ended = tenure
          .maybe_until
          .as_ref()
          .is_some_and(|until| until.maybe_at_ms.is_none_or(|end| end <= at_ms));
        has_begun && !has_ended
      },
    }
  }
}

/// Who signed a document, if a key `user` held as an owner or admin of the verified roster makes
/// `verifies` true: the one on the roster now first, then those of tenures that cover `under`.
/// Whether a former admin's document counts is then the [`AssetLedger`]'s call.
pub(crate) fn asset_signer(
  workspace: &WorkspaceState,
  user: teamcraft_proto::UserId,
  under: SignedUnder,
  verifies: impl Fn(IdentityPublicKey) -> bool,
) -> Option<AssetSigner> {
  let is_admin = workspace
    .roster
    .members
    .iter()
    .filter(|member| member.user_id == user && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .filter_map(|member| identity_key(&member.identity_key))
    .any(&verifies);
  if is_admin {
    return Some(AssetSigner::Admin);
  }
  let mut tenures = workspace
    .admin_history
    .tenures
    .iter()
    .filter(|tenure| tenure.user_id == user && under.is_within(tenure))
    .filter(|tenure| identity_key(&tenure.identity_key).is_some_and(&verifies))
    .peekable();
  tenures.peek()?;
  // Of several tenures under one key, the latest end decides: the demotion this device most likely
  // followed. One without an end has none to compare with.
  let maybe_demoted_at = tenures
    .map(|tenure| tenure.maybe_until.as_ref().map(|until| until.version))
    .reduce(|left, right| left.zip(right).map(|(left, right)| left.max(right)))
    .flatten();
  Some(AssetSigner::FormerAdmin { maybe_demoted_at })
}

fn identity_key(bytes: &Base64Bytes) -> Option<IdentityPublicKey> {
  Some(IdentityPublicKey::from_bytes(
    <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
  ))
}

/// One identity key, for one verification.
pub(crate) struct OneKey(pub(crate) crypto::UserId, pub(crate) IdentityPublicKey);

impl IdentityDirectory for OneKey {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    (user_id == self.0).then_some(self.1)
  }
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_proto::{CustomEmojiId, Id, Snowflake, UnixMillis, UserId};

  use super::*;
  use crate::store::MemoryStore;
  use crate::workspace::{AdminHistory, AdminTenure, RosterDocument, RosterMember, TenureEnd};

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
    fn listed(&self, id: i64, shortcode: &str, signed_at_ms: i64) -> CustomEmojiResponse {
      self.listed_under(id, shortcode, signed_at_ms, None)
    }

    fn listed_under(
      &self,
      id: i64,
      shortcode: &str,
      signed_at_ms: i64,
      maybe_roster_version: Option<u64>,
    ) -> CustomEmojiResponse {
      let document = CustomEmojiDocument {
        shortcode: shortcode.to_owned(),
        sha256: CustomEmojiHash::from_bytes([id as u8; 32]),
        format: CustomEmojiFormat::Png,
        signed_at_ms,
        maybe_roster_version,
      };
      CustomEmojiResponse {
        id: CustomEmojiId::from_snowflake(Snowflake::new(id).unwrap()),
        shortcode: shortcode.to_owned(),
        sha256: document.sha256,
        format: CustomEmojiFormat::Png,
        size_bytes: 10,
        width: 16,
        height: 16,
        maybe_created_by: Some(self.user),
        created_at: UnixMillis(0),
        signed: sign_custom_emoji(&self.device, &self.certificate, &document).unwrap(),
      }
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
  fn an_admins_emoji_verifies_and_a_members_does_not() {
    let (admin, member) = (Signer::new(1), Signer::new(2));
    let state = workspace(&[(&admin, RosterRole::Admin)]);
    let set = verified_set(
      &[admin.listed(10, "ship_it", 5), member.listed(11, "fake", 5)],
      &state,
      &mut AssetLedger::default(),
    );
    assert_eq!(set.len(), 1);
    assert_eq!(set[0].shortcode, "ship_it");
    assert_eq!(set[0].signed_by, admin.user);
  }

  #[test]
  fn the_signed_name_counts_not_the_servers() {
    let admin = Signer::new(1);
    let state = workspace(&[(&admin, RosterRole::Owner)]);
    let mut listed = admin.listed(10, "ship_it", 5);
    listed.shortcode = "renamed_by_server".into();
    listed.sha256 = CustomEmojiHash::from_bytes([99; 32]);
    let emoji = verify(&listed, &state, &AssetLedger::default()).unwrap();
    assert_eq!(emoji.shortcode, "ship_it");
    assert_eq!(emoji.sha256, CustomEmojiHash::from_bytes([10; 32]));
    // A document changed after signing doesn't verify.
    let mut document: CustomEmojiDocument = serde_json::from_slice(listed.signed.payload.as_slice()).unwrap();
    document.sha256 = CustomEmojiHash::from_bytes([99; 32]);
    listed.signed.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
    assert!(verify(&listed, &state, &AssetLedger::default()).is_none());
  }

  #[test]
  fn a_former_admins_emoji_counts_for_what_they_signed_while_admin() {
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
    assert!(verify(&former.listed(10, "early", 150), &state, &AssetLedger::default()).is_some());
    assert!(verify(&former.listed(11, "late", 250), &state, &AssetLedger::default()).is_none());
    assert!(verify(&former.listed(12, "before", 50), &state, &AssetLedger::default()).is_none());
  }

  /// Issue #2290, on a device without a ledger (one that didn't follow the demotion): a document
  /// that names a roster version is judged by it, not by the signer's clock. The version is the
  /// signer's word; [`former_admins`] is what a device that followed the demotion relies on.
  mod roster_version {
    use super::*;

    /// Admin from roster version 2 (signed at 100) until version 4 demoted them (signed at 200);
    /// the device has verified up to version 5.
    pub(super) fn demoted(former: &Signer) -> WorkspaceState {
      let mut state = workspace(&[]);
      state.roster.version = 5;
      state.admin_history = AdminHistory {
        is_complete: true,
        tenures: vec![AdminTenure {
          user_id: former.user,
          identity_key: former.key(),
          is_owner: false,
          maybe_from_ms: Some(100),
          maybe_from_version: Some(2),
          maybe_until: Some(TenureEnd {
            version: 4,
            maybe_at_ms: Some(200),
          }),
        }],
        maybe_replayed_at: None,
      };
      state
    }

    /// An unmodified app names the roster it holds, which no longer lists them as an admin: a
    /// signing time set back into their tenure doesn't help.
    #[test]
    fn a_backdated_document_of_a_demoted_admin_is_refused() {
      let former = Signer::new(1);
      let state = demoted(&former);
      assert!(
        verify(
          &former.listed_under(10, "backdated", 150, Some(4)),
          &state,
          &AssetLedger::default()
        )
        .is_none()
      );
      assert!(
        verify(
          &former.listed_under(11, "backdated", 150, Some(5)),
          &state,
          &AssetLedger::default()
        )
        .is_none()
      );
    }

    #[test]
    fn a_document_signed_before_the_demotion_still_counts() {
      let former = Signer::new(1);
      let state = demoted(&former);
      for version in [2, 3] {
        let emoji = verify(
          &former.listed_under(10, "early", 150, Some(version)),
          &state,
          &AssetLedger::default(),
        );
        assert_eq!(emoji.map(|emoji| emoji.signed_at), Some(150), "roster {version}");
      }
      // The signing time is for display and ordering only: one outside the tenure doesn't matter.
      assert!(
        verify(
          &former.listed_under(11, "odd_clock", 9_999, Some(3)),
          &state,
          &AssetLedger::default()
        )
        .is_some()
      );
    }

    /// A version before the tenure began, or one this device never verified (the server can't
    /// make up a future roster either).
    #[test]
    fn a_roster_version_the_signer_never_held_is_refused() {
      let former = Signer::new(1);
      let state = demoted(&former);
      for version in [0, 1, 6, u64::MAX] {
        assert!(
          verify(
            &former.listed_under(10, "forged", 150, Some(version)),
            &state,
            &AssetLedger::default()
          )
          .is_none(),
          "roster {version}"
        );
      }
    }

    /// An admin now could sign the document again, so any version they name counts.
    #[test]
    fn a_current_admins_document_counts_under_any_version() {
      let admin = Signer::new(1);
      let mut state = workspace(&[(&admin, RosterRole::Admin)]);
      state.roster.version = 5;
      for version in [1, 5, 6] {
        assert!(
          verify(
            &admin.listed_under(10, "ship_it", 150, Some(version)),
            &state,
            &AssetLedger::default()
          )
          .is_some()
        );
      }
    }

    /// A tenure recorded before its first version was kept has no lower bound; its end still
    /// holds.
    #[test]
    fn a_tenure_without_its_first_version_is_bounded_by_its_end() {
      let former = Signer::new(1);
      let mut state = demoted(&former);
      state.admin_history.tenures[0].maybe_from_version = None;
      assert!(
        verify(
          &former.listed_under(10, "early", 150, Some(1)),
          &state,
          &AssetLedger::default()
        )
        .is_some()
      );
      assert!(
        verify(
          &former.listed_under(11, "late", 150, Some(4)),
          &state,
          &AssetLedger::default()
        )
        .is_none()
      );
    }

    #[test]
    fn the_field_is_left_out_of_documents_without_it() {
      let document = CustomEmojiDocument {
        shortcode: "ship_it".into(),
        sha256: CustomEmojiHash::from_bytes([1; 32]),
        format: CustomEmojiFormat::Png,
        signed_at_ms: 5,
        maybe_roster_version: None,
      };
      let json = serde_json::to_value(&document).unwrap();
      assert!(json.get("roster_version").is_none());
      let named = CustomEmojiDocument {
        maybe_roster_version: Some(7),
        ..document
      };
      assert_eq!(serde_json::to_value(&named).unwrap()["roster_version"], 7);
    }
  }

  /// Issue #2290: what a device that followed a demotion accepts of the demoted admin. Their
  /// time and roster version are their own word; what this device held before is not.
  mod former_admins {
    use super::*;

    /// The admin of roster versions 2 to 3, as a device verified them (it kept a ledger from
    /// version 2), then demoted in version 4.
    fn followed(former: &Signer) -> (WorkspaceState, WorkspaceState, AssetLedger) {
      let mut before = workspace(&[(former, RosterRole::Admin)]);
      before.roster.version = 3;
      let mut after = roster_version::demoted(former);
      after.roster.version = 4;
      let mut ledger = AssetLedger::default();
      ledger.follow(2);
      (before, after, ledger)
    }

    #[test]
    fn a_document_held_before_the_demotion_still_counts() {
      let former = Signer::new(1);
      let (before, after, mut ledger) = followed(&former);
      let early = former.listed_under(10, "early", 150, Some(3));
      assert_eq!(
        verified_set(std::slice::from_ref(&early), &before, &mut ledger).len(),
        1
      );
      let set = verified_set(&[early], &after, &mut ledger);
      assert_eq!(set.len(), 1);
      assert_eq!(set[0].signed_by, former.user);
    }

    /// Signed after the demotion, naming a roster version or (an old document's form) a time inside
    /// the tenure: first seen after the demotion, so refused.
    #[test]
    fn a_demoted_admins_new_document_is_refused() {
      let former = Signer::new(1);
      let (before, after, mut ledger) = followed(&former);
      verified_set(
        &[former.listed_under(10, "early", 150, Some(3))],
        &before,
        &mut ledger,
      );
      let backdated = [
        former.listed_under(11, "by_version", 150, Some(3)),
        former.listed(12, "by_clock", 150),
      ];
      assert!(verified_set(&backdated, &after, &mut ledger).is_empty());
      // Refused documents aren't held: asking again changes nothing.
      assert!(verified_set(&backdated, &after, &mut ledger).is_empty());
    }

    /// A current admin signs the former admin's emoji again (a rename does): it counts.
    #[test]
    fn a_current_admins_new_signature_counts() {
      let (former, admin) = (Signer::new(1), Signer::new(2));
      let (_, mut after, mut ledger) = followed(&former);
      after.roster.members.push(RosterMember {
        user_id: admin.user,
        identity_key: admin.key(),
        role: RosterRole::Admin,
      });
      let set = verified_set(
        &[
          former.listed_under(11, "new_picture", 150, Some(3)),
          admin.listed_under(11, "new_picture", 300, Some(4)),
        ],
        &after,
        &mut ledger,
      );
      assert_eq!(set.len(), 1);
      assert_eq!(set[0].signed_by, admin.user);
    }

    /// A device whose ledger began after the demotion has nothing to compare with: it judges by
    /// the version, as before (e2ee.md §4.13, "What it does not stop").
    #[test]
    fn a_device_that_did_not_follow_the_demotion_judges_by_the_version() {
      let former = Signer::new(1);
      let (_, after, _) = followed(&former);
      let mut late = AssetLedger::default();
      late.follow(4);
      let listed = [former.listed_under(11, "unknown", 150, Some(3))];
      assert_eq!(verified_set(&listed, &after, &mut late).len(), 1);
    }
  }

  #[test]
  fn pictures_are_kept_only_with_their_hash_and_removed_ones_are_bounded() {
    let store = MemoryStore::new();
    let bytes = b"picture".to_vec();
    let sha256 = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&bytes));
    let set = CustomEmojiSet::default();
    assert!(!keep_picture(&store, &set, CustomEmojiHash::from_bytes([1; 32]), bytes.clone()).unwrap());
    assert!(keep_picture(&store, &set, sha256, bytes.clone()).unwrap());
    assert_eq!(picture(&store, sha256).unwrap(), Some(bytes));
    for index in 0..=MAX_KEPT_REMOVED_PICTURES {
      let other = format!("other {index}").into_bytes();
      let hash = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&other));
      keep_picture(&store, &set, hash, other).unwrap();
    }
    // The oldest picture of no emoji in the set went.
    assert_eq!(picture(&store, sha256).unwrap(), None);
  }
}
