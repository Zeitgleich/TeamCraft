//! Meeting guests (e2ee.md §4.6, "Meeting guests", docs/meetings.md): the host side that checks a
//! guest before their device joins the meeting chat's group, and the guest side that proves the
//! link and anchors the organizer.
//!
//! A guest link carries two values in its `#fragment`, which no server sees: a random secret made by
//! the host's device that makes the link, and the organizer's identity key fingerprint. The guest
//! proves the secret with a MAC over their user, identity key and name; the host's device checks it
//! before it adds the guest, so the server can neither invent a guest nor swap a real guest's key.
//! The guest accepts a group only if its founder carries the organizer's key from the link.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};
use std::collections::BTreeMap;
use teamcraft_crypto::backup::Argon2Settings;

use teamcraft_crypto::guest_link::{GuestLinkSecret, key_code, key_code_matches};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, Id, MeetingId, UserId};
use zeroize::Zeroizing;

use super::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::{CoreError, GuestRefusal};
use crate::records;
use crate::store::{LocalStore, Table};

const LINK_SECRET_PREFIX: &str = "guest-link-secret/";
const ORGANIZER_ANCHOR_PREFIX: &str = "guest-organizer-anchor/";
/// Marks the store of a guest's ephemeral device.
const GUEST_DEVICE_KEY: &str = "guest-device";
/// The guests this device let into meeting chats (a local record: the server's word about a user's
/// kind is not the only thing that says somebody is a guest).
const ADMITTED_GUESTS_KEY: &str = "admitted-guests";
/// When each admitted guest's access ends, per meeting chat: from this device's own clock and the
/// end the host's app showed it. A guest still in the group by then is removed whatever the server
/// lists (the server's removal event can be withheld).
const GUEST_DEADLINES_PREFIX: &str = "guest-deadlines/";

/// What a host's device puts into the fragment of a guest link next to the server's token.
#[derive(Clone)]
pub struct GuestLinkMaterial {
  /// The link secret, base64url. Wiped on drop; never printed.
  pub secret: Zeroizing<String>,
  /// The organizer's identity key fingerprint (hex) as this device has it pinned.
  pub organizer_fingerprint: String,
}

impl std::fmt::Debug for GuestLinkMaterial {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GuestLinkMaterial")
      .field("secret", &"[redacted]")
      .field("organizer_fingerprint", &self.organizer_fingerprint)
      .finish()
  }
}

/// What lets a host's device add an admitted guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestAdmission {
  /// The proof of the link secret the guest sent, checked with the secret this device holds.
  Proof(Vec<u8>),
  /// The host compared the guest's key code with the guest's screen and confirmed it. For a device
  /// that did not make the link.
  KeyCodeConfirmed(String),
}

impl PendingEnrollment {
  /// Enrolls the ephemeral device of a meeting guest: the normal enrollment (identity key, device
  /// certificate, key packages, device list), with the key bag the server requires wrapped by a
  /// random passphrase nobody ever sees. There is no backup and no recovery; a guest who loses
  /// the device is a new guest (e2ee.md §4.6, "Meeting guests").
  pub async fn complete_as_guest<A: ServerApi, S: LocalStore>(
    api: A,
    store: S,
    clock: Box<dyn Clock>,
    settings: ClientSettings,
    device_name: String,
    argon2: Argon2Settings,
  ) -> Result<Client<A, S>, CoreError> {
    let mut passphrase = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(passphrase.as_mut());
    let options = EnrollmentOptions {
      device_name,
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.to_vec())),
      argon2,
      maybe_attestation: None,
    };
    let client = Self::new().complete(api, store, clock, settings, options).await?;
    // A guest's device accepts only a group anchored in the link (`is_founded_by_organizer`).
    records::put_json(&client.store, Table::SyncState, GUEST_DEVICE_KEY, &true)?;
    Ok(client)
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The secret and organizer fingerprint for a guest link to `meeting`. The secret is made on the
  /// first call and kept for the meeting, so all guest links of this device check alike.
  pub async fn guest_link_material(
    &mut self,
    conversation: ConversationId,
    meeting: MeetingId,
  ) -> Result<GuestLinkMaterial, CoreError> {
    let response = self.api.conversation(conversation).await?;
    let organizer = response
      .maybe_organizer_user_id
      .ok_or(CoreError::NotAMeetingChat(conversation))?;
    // Guests accept a welcome only from the organizer's devices, so only they make guest links.
    if organizer != self.account.user_id {
      return Err(CoreError::GuestRefused(GuestRefusal::NotOrganizer));
    }
    let directory = self.pin_identities([organizer]).await?;
    let key = directory
      .pinned_identity(convert::user(organizer))
      .ok_or(CoreError::UnknownIdentity(organizer))?;
    let secret = match self.link_secret(meeting)? {
      Some(secret) => secret,
      None => {
        let secret = GuestLinkSecret::generate(&mut OsRng);
        let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.expose_secret()));
        records::put_json(
          &self.store,
          Table::SyncState,
          &link_secret_key(meeting),
          &*encoded,
        )?;
        secret
      },
    };
    Ok(GuestLinkMaterial {
      secret: Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.expose_secret())),
      organizer_fingerprint: key.fingerprint().to_string(),
    })
  }

  /// Lets an admitted guest's devices into the meeting chat's group, after the guest passed the
  /// check of the link: the proof verifies against the identity key the server holds for them (and
  /// that key is then pinned; a different one on the second look is refused), or the host confirmed
  /// the key code. Always with history from the join on. `access_ends_at_ms` is when the guest's
  /// access to the meeting ends (end plus grace, as the host's app shows it).
  pub async fn let_guest_into_meeting_chat(
    &mut self,
    conversation: ConversationId,
    meeting: MeetingId,
    guest: UserId,
    display_name: &str,
    admission: &GuestAdmission,
    access_ends_at_ms: i64,
  ) -> Result<(), CoreError> {
    let organizer = self.api.conversation(conversation).await?.maybe_organizer_user_id;
    if organizer != Some(self.account.user_id) {
      return Err(CoreError::GuestRefused(GuestRefusal::NotOrganizer));
    }
    let identity = self
      .api
      .identity_key(guest)
      .await?
      .ok_or(CoreError::GuestRefused(GuestRefusal::NotEnrolled))?;
    let bytes = <[u8; 32]>::try_from(identity.public_key.as_slice())
      .map_err(|_| CoreError::Corrupt(format!("identity key of {guest} has the wrong size")))?;
    let key = IdentityPublicKey::from_bytes(bytes);
    self.check_admission(meeting, guest, display_name, &key, admission)?;
    // The key that was checked is the key that gets pinned.
    let directory = self.pin_identities([guest]).await?;
    if directory.pinned_identity(convert::user(guest)) != Some(key) {
      return Err(CoreError::GuestRefused(GuestRefusal::KeyChanged));
    }
    // Noted before the add: a guest is a guest here even if the server's user kind says otherwise,
    // and the guest is removed by `access_ends_at_ms` at the latest (e2ee.md §4.6).
    self.note_admitted_guest(conversation, guest, access_ends_at_ms)?;
    self.let_into_meeting_chat(conversation, guest).await
  }

  /// The proof a guest sends the hosts: a MAC over their user, identity key and display name under
  /// the link's secret (`secret` as in the link fragment).
  pub fn guest_link_proof(
    &self,
    secret: &str,
    meeting: MeetingId,
    display_name: &str,
  ) -> Result<Vec<u8>, CoreError> {
    let secret = decode_secret(secret)?;
    let proof = secret.prove(
      meeting_number(meeting),
      convert::user(self.account.user_id),
      &self.account.identity_public_key(),
      display_name,
    );
    Ok(proof.as_bytes().to_vec())
  }

  /// This guest's key code, for the host to compare when it can't check the proof.
  pub fn own_key_code(&self) -> String {
    key_code(&self.account.identity_public_key())
  }

  /// Remembers the organizer's identity key fingerprint from the guest link: the meeting chat's
  /// group is only joined if its founder has exactly this identity key.
  pub fn set_guest_organizer_anchor(
    &self,
    conversation: ConversationId,
    fingerprint: &str,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &organizer_anchor_key(conversation),
      &fingerprint.to_ascii_lowercase(),
    )
  }

  pub(super) fn guest_organizer_anchor(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<String>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &organizer_anchor_key(conversation))
  }

  /// Whether this is a guest's ephemeral device.
  pub fn is_guest_device(&self) -> Result<bool, CoreError> {
    Ok(records::get_json::<bool>(&self.store, Table::SyncState, GUEST_DEVICE_KEY)?.unwrap_or(false))
  }

  fn note_admitted_guest(
    &self,
    conversation: ConversationId,
    guest: UserId,
    access_ends_at_ms: i64,
  ) -> Result<(), CoreError> {
    let mut admitted: Vec<UserId> =
      records::get_json(&self.store, Table::SyncState, ADMITTED_GUESTS_KEY)?.unwrap_or_default();
    if !admitted.contains(&guest) {
      admitted.push(guest);
      records::put_json(&self.store, Table::SyncState, ADMITTED_GUESTS_KEY, &admitted)?;
    }
    let key = deadlines_key(conversation);
    let mut deadlines: BTreeMap<UserId, i64> =
      records::get_json(&self.store, Table::SyncState, &key)?.unwrap_or_default();
    deadlines.insert(guest, access_ends_at_ms);
    records::put_json(&self.store, Table::SyncState, &key, &deadlines)
  }

  /// Users this device itself let into a meeting chat as guests.
  pub(super) fn admitted_guests(&self) -> Result<Vec<UserId>, CoreError> {
    Ok(records::get_json(&self.store, Table::SyncState, ADMITTED_GUESTS_KEY)?.unwrap_or_default())
  }

  /// Queues the removal of guests this device admitted whose access has ended by its own clock,
  /// whether or not the server lists them still; the removal itself is the signed commit of
  /// `apply_meeting_removals`. Run on every sync pass (`repair_dm_setups`).
  pub(super) fn queue_ended_guests(&mut self) -> Result<(), CoreError> {
    let now = self.now_ms();
    let all: Vec<(String, BTreeMap<UserId, i64>)> =
      records::list_json_with_prefix(&self.store, Table::SyncState, GUEST_DEADLINES_PREFIX)?;
    for (key, deadlines) in all {
      let Some(conversation) = key
        .strip_prefix(GUEST_DEADLINES_PREFIX)
        .and_then(|id| id.parse::<ConversationId>().ok())
      else {
        continue;
      };
      let (ended, running): (BTreeMap<_, _>, BTreeMap<_, _>) =
        deadlines.into_iter().partition(|(_, ends_at)| *ends_at <= now);
      if ended.is_empty() {
        continue;
      }
      let mut pending = self.pending_meeting_removals(conversation)?;
      for guest in ended.keys() {
        if !pending.contains(guest) {
          pending.push(*guest);
        }
      }
      self.save_meeting_removals(conversation, &pending)?;
      if running.is_empty() {
        self.store.delete(Table::SyncState, &key)?;
      } else {
        records::put_json(&self.store, Table::SyncState, &key, &running)?;
      }
    }
    Ok(())
  }

  /// Queues the signed removal of meeting guests who are still in the group although the server
  /// no longer lists them (they left, were turned away, or the meeting's access ended); the
  /// removal is committed on the next sync (`repair_dm_setups`) or right away by the caller. Only
  /// guests the signed roster doesn't vouch for count: the server's word that somebody is a guest
  /// never gets a member removed. A removal is only ever the guest's own leaves (e2ee.md §4.6).
  pub(super) fn queue_guest_removals(
    &mut self,
    conversation: ConversationId,
    leftovers: &[UserId],
  ) -> Result<(), CoreError> {
    let is_meeting_chat = self
      .cached_conversations()?
      .get(&conversation)
      .is_some_and(|response| response.kind == teamcraft_proto::ConversationKind::Meeting);
    if !is_meeting_chat {
      return Ok(());
    }
    let guests = self.guest_users(leftovers.iter().copied())?;
    let workspace = self.workspace()?;
    let unvouched: Vec<UserId> = guests
      .into_iter()
      .filter(|guest| {
        !workspace
          .roster
          .members
          .iter()
          .any(|member| member.user_id == *guest)
      })
      .collect();
    if unvouched.is_empty() {
      return Ok(());
    }
    let mut pending = self.pending_meeting_removals(conversation)?;
    let before = pending.len();
    for guest in unvouched {
      if !pending.contains(&guest) {
        pending.push(guest);
      }
    }
    if pending.len() != before {
      self.save_meeting_removals(conversation, &pending)?;
    }
    Ok(())
  }

  fn check_admission(
    &self,
    meeting: MeetingId,
    guest: UserId,
    display_name: &str,
    key: &IdentityPublicKey,
    admission: &GuestAdmission,
  ) -> Result<(), CoreError> {
    match admission {
      GuestAdmission::Proof(proof) => {
        if proof.is_empty() {
          return Err(CoreError::GuestRefused(GuestRefusal::NoProof));
        }
        let secret = self
          .link_secret(meeting)?
          .ok_or(CoreError::GuestRefused(GuestRefusal::NoLinkSecret))?;
        secret
          .verify(
            meeting_number(meeting),
            convert::user(guest),
            key,
            display_name,
            proof,
          )
          .map_err(|_| CoreError::GuestRefused(GuestRefusal::BadProof))
      },
      GuestAdmission::KeyCodeConfirmed(code) => {
        if key_code_matches(key, code) {
          Ok(())
        } else {
          Err(CoreError::GuestRefused(GuestRefusal::KeyCodeMismatch))
        }
      },
    }
  }

  fn link_secret(&self, meeting: MeetingId) -> Result<Option<GuestLinkSecret>, CoreError> {
    let maybe_encoded: Option<String> =
      records::get_json(&self.store, Table::SyncState, &link_secret_key(meeting))?;
    maybe_encoded.map(|encoded| decode_secret(&encoded)).transpose()
  }
}

fn decode_secret(encoded: &str) -> Result<GuestLinkSecret, CoreError> {
  let bytes = URL_SAFE_NO_PAD
    .decode(encoded)
    .map_err(|_| CoreError::GuestRefused(GuestRefusal::NoLinkSecret))?;
  GuestLinkSecret::from_bytes(&bytes).map_err(|_| CoreError::GuestRefused(GuestRefusal::NoLinkSecret))
}

fn meeting_number(meeting: MeetingId) -> u64 {
  // Snowflake IDs are never negative (`Id` refuses them).
  u64::try_from(meeting.snowflake().get()).unwrap_or_default()
}

fn deadlines_key(conversation: ConversationId) -> String {
  format!("{GUEST_DEADLINES_PREFIX}{conversation}")
}

fn link_secret_key(meeting: MeetingId) -> String {
  format!("{LINK_SECRET_PREFIX}{meeting}")
}

fn organizer_anchor_key(conversation: ConversationId) -> String {
  format!("{ORGANIZER_ANCHOR_PREFIX}{conversation}")
}
