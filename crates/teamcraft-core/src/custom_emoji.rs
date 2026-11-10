//! Custom emoji of the workspace (issue #207, e2ee.md §4.13): signing an emoji's document on an
//! admin's device, verifying the server's list against this device's own verified roster, and
//! keeping the verified set and the pictures on the device.
//!
//! An emoji counts when its document is signed by a device of somebody who was an owner or admin
//! of the verified roster when it was signed: on the roster now, or in a tenure of the admin
//! history that covers the signing time. The server's word (its list, its `shortcode` field) counts
//! for nothing; a picture is kept only when its SHA-256 is the one the document names. Pictures stay
//! on the device, by hash, after their emoji left the set, so earlier messages keep showing them:
//! those of the set, and up to [`MAX_KEPT_REMOVED_PICTURES`] more.

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

use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::{RosterRole, WorkspaceState};

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
pub fn verified_set(listed: &[CustomEmojiResponse], workspace: &WorkspaceState) -> Vec<CustomEmoji> {
  let mut by_name: BTreeMap<String, CustomEmoji> = BTreeMap::new();
  for response in listed {
    let Some(emoji) = verify(response, workspace) else {
      tracing::debug!(id = %response.id, "a custom emoji's signature doesn't verify; left out");
      continue;
    };
    match by_name.get(&emoji.shortcode) {
      Some(known) if known.signed_at >= emoji.signed_at => {},
      _ => {
        by_name.insert(emoji.shortcode.clone(), emoji);
      },
    }
  }
  by_name.into_values().collect()
}

/// One listed emoji, if an owner or admin of the verified roster signed its document.
pub fn verify(response: &CustomEmojiResponse, workspace: &WorkspaceState) -> Option<CustomEmoji> {
  let document: CustomEmojiDocument = serde_json::from_slice(response.signed.payload.as_slice()).ok()?;
  check_custom_emoji_shortcode(&document.shortcode).ok()?;
  if !matches!(document.format, CustomEmojiFormat::Png | CustomEmojiFormat::Webp) {
    return None;
  }
  let signer = claimed_signer(response.signed.signature.as_slice()).ok()?;
  let signer_user = convert::user_from_crypto(signer).ok()?;
  let verified = admin_keys(workspace, signer_user, document.signed_at_ms)
    .into_iter()
    .any(|key| {
      verify_log_entry(
        response.signed.signature.as_slice(),
        WorkspaceLog::CustomEmoji,
        version_of(&document),
        response.signed.payload.as_slice(),
        &OneKey(signer, key),
      )
      .is_ok()
    });
  verified.then_some(CustomEmoji {
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
  let maybe_picture: Option<Base64Bytes> = records::get_json(store, Table::Workspace, &picture_key(sha256))?;
  Ok(maybe_picture.map(Base64Bytes::into_vec))
}

pub(crate) fn has_picture(store: &impl LocalStore, sha256: CustomEmojiHash) -> Result<bool, CoreError> {
  Ok(store.get(Table::Workspace, &picture_key(sha256))?.is_some())
}

/// Keeps a downloaded picture if it is the one `sha256` names, and forgets the pictures of emoji
/// gone from `set` beyond [`MAX_KEPT_REMOVED_PICTURES`], oldest first. Returns whether it was kept.
pub(crate) fn keep_picture(
  store: &impl LocalStore,
  set: &CustomEmojiSet,
  sha256: CustomEmojiHash,
  bytes: Vec<u8>,
) -> Result<bool, CoreError> {
  let actual = CustomEmojiHash::from_bytes(teamcraft_crypto::attachment::content_sha256(&bytes));
  if actual != sha256 {
    return Ok(false);
  }
  let mut held: Vec<CustomEmojiHash> =
    records::get_json(store, Table::Workspace, PICTURES_KEY)?.unwrap_or_default();
  held.retain(|known| *known != sha256);
  held.push(sha256);
  let removed = |hash: &CustomEmojiHash| set.by_hash(*hash).is_none();
  while held.iter().filter(|hash| removed(hash)).count() > MAX_KEPT_REMOVED_PICTURES {
    let Some(at) = held.iter().position(removed) else {
      break;
    };
    let oldest = held.remove(at);
    store.delete(Table::Workspace, &picture_key(oldest))?;
  }
  records::put_json(
    store,
    Table::Workspace,
    &picture_key(sha256),
    &Base64Bytes::new(bytes),
  )?;
  records::put_json(store, Table::Workspace, PICTURES_KEY, &held)?;
  Ok(true)
}

/// The signature's "version": when the document was signed.
fn version_of(document: &CustomEmojiDocument) -> u64 {
  u64::try_from(document.signed_at_ms).unwrap_or(0)
}

/// The identity keys `user` held as an owner or admin of the verified roster at `at_ms`: the one
/// on the roster now, and those of tenures that cover that time.
fn admin_keys(
  workspace: &WorkspaceState,
  user: teamcraft_proto::UserId,
  at_ms: i64,
) -> Vec<IdentityPublicKey> {
  let mut keys: Vec<Vec<u8>> = workspace
    .roster
    .members
    .iter()
    .filter(|member| member.user_id == user && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .map(|member| member.identity_key.as_slice().to_vec())
    .collect();
  for tenure in workspace
    .admin_history
    .tenures
    .iter()
    .filter(|tenure| tenure.user_id == user)
  {
    let has_begun = tenure.maybe_from_ms.is_none_or(|from| from <= at_ms);
    let has_ended = tenure
      .maybe_until
      .as_ref()
      .is_some_and(|until| until.maybe_at_ms.is_none_or(|end| end <= at_ms));
    if has_begun && !has_ended {
      keys.push(tenure.identity_key.as_slice().to_vec());
    }
  }
  keys.sort();
  keys.dedup();
  keys
    .into_iter()
    .filter_map(|key| {
      Some(IdentityPublicKey::from_bytes(
        <[u8; 32]>::try_from(key.as_slice()).ok()?,
      ))
    })
    .collect()
}

fn picture_key(sha256: CustomEmojiHash) -> String {
  format!("{PICTURE_PREFIX}{}", sha256.to_hex())
}

/// One identity key, for one verification.
struct OneKey(crypto::UserId, IdentityPublicKey);

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

    fn listed(&self, id: i64, shortcode: &str, signed_at_ms: i64) -> CustomEmojiResponse {
      let document = CustomEmojiDocument {
        shortcode: shortcode.to_owned(),
        sha256: CustomEmojiHash::from_bytes([id as u8; 32]),
        format: CustomEmojiFormat::Png,
        signed_at_ms,
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
    let emoji = verify(&listed, &state).unwrap();
    assert_eq!(emoji.shortcode, "ship_it");
    assert_eq!(emoji.sha256, CustomEmojiHash::from_bytes([10; 32]));
    // A document changed after signing doesn't verify.
    let mut document: CustomEmojiDocument = serde_json::from_slice(listed.signed.payload.as_slice()).unwrap();
    document.sha256 = CustomEmojiHash::from_bytes([99; 32]);
    listed.signed.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
    assert!(verify(&listed, &state).is_none());
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
        maybe_until: Some(TenureEnd {
          version: 3,
          maybe_at_ms: Some(200),
        }),
      }],
      maybe_replayed_at: None,
    };
    assert!(verify(&former.listed(10, "early", 150), &state).is_some());
    assert!(verify(&former.listed(11, "late", 250), &state).is_none());
    assert!(verify(&former.listed(12, "before", 50), &state).is_none());
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
