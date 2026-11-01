//! Meeting guests (e2ee.md §4.6, "Meeting guests", docs/meetings.md): the organizer's side that
//! lets a guest's own device into the meeting chat's group, and the guest's side that anchors the
//! organizer.
//!
//! A meeting link carries no secret (#987): `<base>/meeting/<token>#v=<fingerprint>`, the token
//! only lets its holder knock, the fragment is the organizer's identity key fingerprint. The guest's
//! page makes its own keys (an ephemeral device); an organizer's device adds that device to the
//! group, which seals the group's keys to it in the welcome (a new epoch). The guest accepts a
//! group only from a welcome sent by a device the organizer's key from the link certified.

use std::collections::BTreeMap;
use teamcraft_crypto::backup::Argon2Settings;

use rand_core::{OsRng, RngCore};
use teamcraft_crypto::guest_link::key_code;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, MeetingId, UserId};
use zeroize::Zeroizing;

use super::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::{CoreError, GuestRefusal};
use crate::records;
use crate::store::{LocalStore, Table};

/// What a member's app appends to a meeting link's `#fragment` (`v=<fingerprint>`), kept for the
/// UI to read once [`Client::prepare_guest_link`] made it.
const LINK_FRAGMENT_PREFIX: &str = "guest-link-fragment/";
const ORGANIZER_ANCHOR_PREFIX: &str = "guest-organizer-anchor/";
/// The key code of a waiting guest's key as this device pinned it, for the lobby entry.
const GUEST_KEY_CODE_PREFIX: &str = "guest-key-code/";
/// The lobby setting of a meeting as the organizer chose it on this device (#987 review): only this
/// record, never the server's `has_lobby`, lets the device admit guests of the link by itself.
const LOBBY_RECORD_PREFIX: &str = "meeting-lobby-record/";
/// The name of the organizer's fingerprint in a meeting link's fragment, and the name links of
/// older apps used for it.
pub const VERIFIER_KEY: &str = "v";
pub const LEGACY_ORGANIZER_KEY: &str = "o";
/// Marks the store of a guest's ephemeral device.
const GUEST_DEVICE_KEY: &str = "guest-device";
/// The guests this device let into meeting chats (a local record: the server's word about a user's
/// kind is not the only thing that says somebody is a guest).
const ADMITTED_GUESTS_KEY: &str = "admitted-guests";
/// When each admitted guest's access ends, per meeting chat: from this device's own clock and the
/// end the host's app showed it. A guest still in the group by then is removed whatever the server
/// lists (the server's removal event can be withheld).
const GUEST_DEADLINES_PREFIX: &str = "guest-deadlines/";

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
  /// The fingerprint of the organizer's identity key as this device has it pinned: the `v` of the
  /// meeting chat's link. Any member's device can give it.
  pub async fn organizer_fingerprint(&mut self, conversation: ConversationId) -> Result<String, CoreError> {
    let organizer = self
      .api
      .conversation(conversation)
      .await?
      .maybe_organizer_user_id
      .ok_or(CoreError::NotAMeetingChat(conversation))?;
    let directory = self.pin_identities([organizer]).await?;
    let key = directory
      .pinned_identity(convert::user(organizer))
      .ok_or(CoreError::UnknownIdentity(organizer))?;
    Ok(key.fingerprint().to_string())
  }

  /// Makes the fragment of the meeting link (`v=<fingerprint>`) and keeps it where the UI reads it
  /// ([`guest_link_fragment`]).
  pub async fn prepare_guest_link(
    &mut self,
    conversation: ConversationId,
    meeting: MeetingId,
  ) -> Result<(), CoreError> {
    let fingerprint = self.organizer_fingerprint(conversation).await?;
    records::put_json(
      &self.store,
      Table::SyncState,
      &link_fragment_key(meeting),
      &format!("{VERIFIER_KEY}={fingerprint}"),
    )
  }

  /// Lets an admitted guest's device into the meeting chat's group (organizer's devices only):
  /// on the host's "admit", or by itself for a guest of the meeting link in a meeting without a
  /// lobby. The identity key the server holds for the guest is pinned (a different one on a
  /// second look is refused), the guest is noted as a guest of this chat until
  /// `access_ends_at_ms` (end plus grace, as the host's app shows it), and the device is added
  /// with a signed add, always with history from the join on: a new epoch whose welcome is sealed
  /// to the guest's key package.
  ///
  /// `knock_fingerprint` is the fingerprint the guest's page reported with its device; the key the
  /// server presents must have it. That binds the key to the knock, but the server relays both:
  /// against a hostile server only comparing the key code with the guest's screen helps
  /// ([`Self::prepare_guest_key_code`], e2ee.md §4.6).
  pub async fn let_guest_into_meeting_chat(
    &mut self,
    conversation: ConversationId,
    guest: UserId,
    knock_fingerprint: &str,
    access_ends_at_ms: i64,
  ) -> Result<(), CoreError> {
    let organizer = self.api.conversation(conversation).await?.maybe_organizer_user_id;
    // The guest accepts a welcome only from the organizer's devices (`v` in the link).
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
    let directory = self.pin_identities([guest]).await?;
    if directory.pinned_identity(convert::user(guest)) != Some(key) {
      return Err(CoreError::GuestRefused(GuestRefusal::KeyChanged));
    }
    if !key
      .fingerprint()
      .to_string()
      .eq_ignore_ascii_case(knock_fingerprint)
    {
      return Err(CoreError::GuestRefused(GuestRefusal::NotTheKnockingKey));
    }
    // Noted before the add: a guest is a guest here even if the server's user kind says otherwise,
    // and the guest is removed by `access_ends_at_ms` at the latest (e2ee.md §4.6).
    self.note_admitted_guest(conversation, guest, access_ends_at_ms)?;
    self.let_into_meeting_chat(conversation, guest).await
  }

  /// Pins a waiting guest's identity key as the server presents it and keeps its key code for the
  /// lobby entry ([`guest_key_code`]): the host compares it with the guest's screen. Admitting later
  /// adds only a guest with this very key (a swap is refused as a changed key).
  pub async fn prepare_guest_key_code(&mut self, guest: UserId) -> Result<(), CoreError> {
    let directory = self.pin_identities([guest]).await?;
    let key = directory
      .pinned_identity(convert::user(guest))
      .ok_or(CoreError::GuestRefused(GuestRefusal::NotEnrolled))?;
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{GUEST_KEY_CODE_PREFIX}{guest}"),
      &key_code(&key),
    )
  }

  /// Keeps the lobby setting the organizer chose for `meeting` on this device. Only this record
  /// lets the device admit guests of the link by itself (when it says there is no lobby).
  pub fn note_meeting_lobby(&self, meeting: MeetingId, has_lobby: bool) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{LOBBY_RECORD_PREFIX}{meeting}"),
      &has_lobby,
    )
  }

  /// This device's identity key fingerprint (hex): what a guest's page reports with its device.
  pub fn own_fingerprint(&self) -> String {
    self.account.identity_public_key().fingerprint().to_string()
  }

  /// This guest's key code, shown on the guest's page for the host to compare.
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
}

/// The key code of a waiting guest's key as this device pinned it, once
/// [`Client::prepare_guest_key_code`] made it.
pub fn guest_key_code<S: LocalStore>(store: &S, guest: UserId) -> Option<String> {
  records::get_json(
    store,
    Table::SyncState,
    &format!("{GUEST_KEY_CODE_PREFIX}{guest}"),
  )
  .ok()
  .flatten()
}

/// Whether the organizer chose "no lobby" for `meeting` on this device: the only case in which the
/// device admits guests of the link by itself. Unknown means no.
pub fn is_open_by_own_record<S: LocalStore>(store: &S, meeting: MeetingId) -> bool {
  records::get_json::<bool>(
    store,
    Table::SyncState,
    &format!("{LOBBY_RECORD_PREFIX}{meeting}"),
  )
  .ok()
  .flatten()
    == Some(false)
}

/// The fragment a member's app appends to the meeting link, if [`Client::prepare_guest_link`] made it.
pub fn guest_link_fragment<S: LocalStore>(store: &S, meeting: MeetingId) -> Option<String> {
  records::get_json(store, Table::SyncState, &link_fragment_key(meeting))
    .ok()
    .flatten()
}

fn link_fragment_key(meeting: MeetingId) -> String {
  format!("{LINK_FRAGMENT_PREFIX}{meeting}")
}

fn deadlines_key(conversation: ConversationId) -> String {
  format!("{GUEST_DEADLINES_PREFIX}{conversation}")
}

fn organizer_anchor_key(conversation: ConversationId) -> String {
  format!("{ORGANIZER_ANCHOR_PREFIX}{conversation}")
}
