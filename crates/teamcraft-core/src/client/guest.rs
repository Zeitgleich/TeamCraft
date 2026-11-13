//! Meeting guests (e2ee.md §4.6, "Meeting guests", docs/meetings.md): the organizer's side that
//! lets a guest's own device into the meeting chat's group, and the guest's side that anchors the
//! organizer.
//!
//! A meeting link carries no secret (#987): `<base>/meeting/<token>#v=<fingerprint>`, the token
//! only lets its holder knock, the fragment is the organizer's identity key fingerprint. The guest's
//! page makes its own keys (an ephemeral device); an organizer's device adds that device to the
//! group, which seals the group's keys to it in the welcome (a new epoch). The guest accepts a
//! group only from a welcome sent by a device the organizer's key from the link certified, or by
//! a device of an admitter that key signed for the meeting (#1103): the organizer's devices publish
//! that list, the other members' devices check they are on it before they let a guest in.

use std::collections::{BTreeMap, BTreeSet};
use teamcraft_crypto::backup::Argon2Settings;

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::guest_link::key_code;
use teamcraft_crypto::identity::{IdentityFingerprint, IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::meeting_admitters::{
  Admitter, AdmittersDraft, AdmittersExpectation, LinkAdmission, SignedMeetingAdmitters,
  VerifiedMeetingAdmitters,
};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{Base64Bytes, ConversationId, MeetingId, PutMeetingAdmittersRequest, UserId};
use zeroize::Zeroizing;

use super::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::convert;
use crate::error::{CoreError, GuestRefusal};
use crate::meeting_invitees::{self, ChosenInvitees, InviteeFacts, InviteeList, SignedInvitees};
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
/// The meeting a guest's page joined, per meeting chat: the guest checks that the admitters are
/// for it.
const GUEST_MEETING_PREFIX: &str = "guest-meeting/";
/// The newest organizer-signed admitters of a meeting chat this device verified
/// ([`AdmittersRecord`]).
const ADMITTERS_PREFIX: &str = "meeting-admitters/";
/// Key prefix of the invitees the organizer chose on this device, per meeting chat (#1726).
const CHOSEN_INVITEES_PREFIX: &str = "meeting-chosen-invitees/";
/// Key prefix of the users a verified commit took out of a conversation's group since one let them
/// in, per conversation, oldest first (#1726).
const REMOVED_INVITEES_PREFIX: &str = "meeting-removed-invitees/";
/// At most this many removed users are kept per conversation; the oldest go first.
const MAX_REMOVED_INVITEES: usize = teamcraft_crypto::meeting_admitters::MAX_INVITEES;

/// The newest organizer-signed admitters of a meeting chat this device verified (#1103, e2ee.md
/// §4.6, "Meeting guests"). An older statement is refused afterwards (a rollback by the server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AdmittersRecord {
  sequence: u64,
  /// When the list stops counting (milliseconds).
  expires_at_ms: i64,
  /// The organizer signed "no lobby": admitters let people who came with the link in by themselves.
  admits_by_link: bool,
  /// The listed users (the organizer, who signed, aside).
  admitters: Vec<UserId>,
  /// The list names this device's user with this device's identity key (or it is the organizer's).
  is_own: bool,
  /// The invitees of the newest verified statement that names them (version 2, #1726); a newer
  /// version 1 statement leaves them as they are. `None` before any did.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_invitees: Option<InviteesRecord>,
}

/// The invitees of a verified statement, with its sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct InviteesRecord {
  sequence: u64,
  invitees: Vec<UserId>,
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

  /// Lets an admitted guest's device into the meeting chat's group: the organizer's devices, and
  /// those of the admitters the organizer signed for the chat (#1103). On a host's "admit", or by
  /// itself for a guest of the meeting link in a meeting without a lobby. The identity key the
  /// server holds for the guest is pinned (a different one on a second look is refused), the guest
  /// is noted as a guest of this chat until `access_ends_at_ms` (end plus grace, as the host's app
  /// shows it), and the device is added with a signed add, always with history from the join on:
  /// a new epoch whose welcome is sealed to the guest's key package.
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
    // The guest accepts a welcome only from the hosts by the lists that follow from `v` in the
    // link (the organizer and the co-hosts, #1720) and from the admitters the organizer signed:
    // anybody else's would fail on the guest's side.
    if organizer != Some(self.account.user_id)
      && !self.is_own_hosting(conversation).await?
      && self.own_admission(conversation).await?.is_none()
    {
      return Err(CoreError::GuestRefused(GuestRefusal::NotAnAdmitter));
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

  /// Remembers the organizer's identity key fingerprint from the guest link, and the meeting the
  /// guest joined: the meeting chat's group is only joined from a welcome of a device this key
  /// certified, or of an admitter's device the key signed for `meeting` (#1103).
  pub fn set_guest_organizer_anchor(
    &self,
    conversation: ConversationId,
    meeting: MeetingId,
    fingerprint: &str,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{GUEST_MEETING_PREFIX}{conversation}"),
      &meeting,
    )?;
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

  /// The meeting a guest's page joined for this chat (`set_guest_organizer_anchor`).
  pub(super) fn guest_meeting(&self, conversation: ConversationId) -> Result<Option<MeetingId>, CoreError> {
    records::get_json(
      &self.store,
      Table::SyncState,
      &format!("{GUEST_MEETING_PREFIX}{conversation}"),
    )
  }

  /// Whether a guest's welcome came from a device of an admitter the organizer signed for this
  /// meeting chat (#1103): the statement must verify against `anchor` (`v` of the link), name the
  /// meeting the guest joined and its chat, be in force by this device's clock and not be older
  /// than one seen before, and list the device's user with exactly the identity key that certified
  /// the device. A failed fetch other than "none published" is an error the caller retries; a
  /// statement that doesn't verify is no admission.
  pub(super) async fn is_signed_admitters_device(
    &mut self,
    conversation: ConversationId,
    anchor: &str,
    device: &VerifiedDevice,
  ) -> Result<bool, CoreError> {
    let maybe_meeting: Option<MeetingId> = records::get_json(
      &self.store,
      Table::SyncState,
      &format!("{GUEST_MEETING_PREFIX}{conversation}"),
    )?;
    let (Some(meeting), Ok(organizer)) = (maybe_meeting, anchor.parse::<IdentityFingerprint>()) else {
      return Ok(false);
    };
    let now_ms = self.now_ms();
    let maybe_admitters = self
      .verified_admitters(conversation, Some(meeting), &organizer, now_ms)
      .await?;
    Ok(maybe_admitters.is_some_and(|admitters| admitters.admits_device(device)))
  }

  /// Signs and publishes who may let guests into `meeting` (the organizer's devices only, #1103):
  /// the users with a device in the meeting chat's group as this device sees it, guests left out,
  /// with the identity keys this device pinned for them, and whether they let in people of the
  /// link by themselves (the organizer's "no lobby", as recorded on this device or, without a
  /// record, as the organizer's key signed it before). Signs anew only when that differs from the
  /// server's newest statement or the statement ends before `expires_at_ms`; `Ok(true)` when it
  /// published one. Another device of the organizer publishing at the same time is no error: the
  /// next call compares again.
  pub async fn sync_meeting_admitters(
    &mut self,
    meeting: MeetingId,
    conversation: ConversationId,
    expires_at_ms: i64,
  ) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let is_organizer = self.api.conversation(conversation).await?.maybe_organizer_user_id == Some(me);
    if !is_organizer || !self.has_group(conversation)? {
      return Ok(false);
    }
    // A meeting handed over to this user: its hosts are signed with this user's key first (#1720).
    if let Err(error) = self.sync_meeting_hosts(meeting, conversation).await {
      tracing::warn!(meeting_id = %meeting, %error, "could not sign the hosts of a meeting handed over");
    }
    let admitters = self.admitter_candidates(conversation).await?;
    let own = self.account.identity_public_key().fingerprint();
    // The organizer's own statement sets the next sequence even once it ended: no end of
    // validity counts here (`now_ms` 0).
    let maybe_current = self
      .verified_admitters(conversation, Some(meeting), &own, 0)
      .await?;
    // Without a record of its own (a meeting handed to this user, #1720) the device keeps what the
    // organizer before signed, as this device verified it.
    let maybe_kept_admission = self.admitters_record(conversation)?.map(|record| {
      if record.admits_by_link {
        LinkAdmission::ByThemselves
      } else {
        LinkAdmission::Lobby
      }
    });
    let link_admission = match records::get_json::<bool>(
      &self.store,
      Table::SyncState,
      &format!("{LOBBY_RECORD_PREFIX}{meeting}"),
    )? {
      Some(false) => LinkAdmission::ByThemselves,
      Some(true) => LinkAdmission::Lobby,
      None => maybe_current
        .as_ref()
        .map(VerifiedMeetingAdmitters::link_admission)
        .or(maybe_kept_admission)
        .unwrap_or(LinkAdmission::Lobby),
    };
    let maybe_invitees: Option<Vec<teamcraft_crypto::ids::UserId>> = self
      .invitees_to_sign(conversation)
      .await?
      .map(|invitees| invitees.into_iter().map(convert::user).collect());
    let wanted_until = u64::try_from(expires_at_ms).unwrap_or(0);
    if let Some(current) = &maybe_current
      && current.has_same_content(&admitters, link_admission, maybe_invitees.as_deref())
      && current.expires_at_ms() >= wanted_until
    {
      if maybe_invitees.is_some() {
        self.forget_chosen_invitees(conversation)?;
      }
      return Ok(false);
    }
    let now = u64::try_from(self.now_ms()).unwrap_or(0);
    let seen = self
      .admitters_record(conversation)?
      .map_or(0, |record| record.sequence);
    let current_sequence = maybe_current
      .as_ref()
      .map_or(0, VerifiedMeetingAdmitters::sequence);
    // Time as the floor keeps a later list ahead of an earlier one of the organizer's other
    // devices, even of one this device never saw.
    let sequence = seen.max(current_sequence).saturating_add(1).max(now);
    let expires_at_ms = maybe_current
      .as_ref()
      .map_or(wanted_until, |current| current.expires_at_ms().max(wanted_until));
    let signed = AdmittersDraft {
      meeting_id: convert::meeting(meeting),
      conversation_id: convert::conversation(conversation),
      sequence,
      expires_at_ms,
      link_admission,
      admitters,
      maybe_invitees,
    }
    .sign(self.account.key_bag.identity())?;
    let request = PutMeetingAdmittersRequest {
      statement: Base64Bytes::new(signed.to_bytes()?),
    };
    match self.api.put_meeting_admitters(conversation, &request).await {
      Ok(()) => {},
      Err(error) if error.is_conflict() => {
        tracing::info!(meeting_id = %meeting, "another device of the organizer published the admitters first");
        return Ok(false);
      },
      Err(error) => return Err(error.into()),
    }
    // What this device signed is what it would verify: kept like a verified statement.
    let expectation = AdmittersExpectation {
      organizer: &own,
      maybe_meeting_id: Some(convert::meeting(meeting)),
      conversation_id: convert::conversation(conversation),
      now_ms: 0,
      maybe_seen_sequence: None,
    };
    let verified = signed.verify(&expectation)?;
    self.keep_admitters(conversation, &verified)?;
    if verified.maybe_invitees().is_some() {
      self.forget_chosen_invitees(conversation)?;
    }
    tracing::info!(meeting_id = %meeting, sequence, "published the meeting's admitters");
    Ok(true)
  }

  /// Which users of `list` this host's device adds to the meeting chat's group (#1726, e2ee.md
  /// §4.6.5, "Meetings (D10)"): users the organizer added count as they are, users from the
  /// server's list only where the organizer's newest signed list names them. A decision of the
  /// organizer's is kept as the new list, for the next signed statement. Anybody left out knocks
  /// at the lobby.
  pub(super) async fn meeting_invitees_to_add(
    &mut self,
    conversation: ConversationId,
    list: &InviteeList,
  ) -> Result<Vec<UserId>, CoreError> {
    let from_server = match list {
      InviteeList::Chosen { kept, .. } => kept.len(),
      InviteeList::Listed(listed) => listed.len(),
    };
    if from_server > 0 {
      // The newest statement, whatever its end: an ended list is still the organizer's newest
      // word on who is invited. A failure keeps what this device verified before.
      if let Err(error) = self.organizers_admitters_at(conversation, 0).await {
        tracing::warn!(conversation_id = %conversation, %error, "could not read the meeting's signed invitees");
      }
    }
    // Decides whether a choice is kept and how the server's list counts.
    self.refresh_minimum_client_version().await;
    let signed = self.kept_signed_invitees(conversation)?;
    let signs = meeting_invitees::signs_invitees(self.highest_minimum_client_version()?);
    let in_group = self.members_in_group(conversation)?;
    let removed = self.removed_invitees(conversation)?;
    let facts = InviteeFacts {
      signed: &signed,
      signs_invitees: signs,
      in_group: &in_group,
      removed: &removed,
    };
    let to_add = meeting_invitees::invitees_to_add(list, &facts);
    if let InviteeList::Chosen { added, .. } = list {
      // The organizer invites them again: an earlier removal no longer keeps them out.
      for user in added {
        self.note_invitee_added(conversation, *user)?;
      }
      // Before every app reads signed invitees, a choice carries users over unchecked: it is
      // neither kept nor signed.
      if signs {
        let chosen = ChosenInvitees {
          invitees: to_add.clone(),
          maybe_base_sequence: self.signed_invitees_sequence(conversation)?,
        };
        records::put_json(
          &self.store,
          Table::SyncState,
          &format!("{CHOSEN_INVITEES_PREFIX}{conversation}"),
          &chosen,
        )?;
        tracing::debug!(conversation_id = %conversation, added = added.len(), "kept the organizer's invitees");
      }
    }
    let vouched = to_add.len();
    let offered = from_server
      + match list {
        InviteeList::Chosen { added, .. } => added.len(),
        InviteeList::Listed(_) => 0,
      };
    if vouched < offered {
      tracing::info!(
        conversation_id = %conversation,
        left_out = offered - vouched,
        "listed attendees the organizer's signed list doesn't name are not added; they knock at the lobby"
      );
    }
    Ok(to_add)
  }

  /// The invitees the organizer's next statement names: the newest list this device knows, once
  /// the apps all read it ([`meeting_invitees::SIGNED_INVITEES_RELEASE`]); `None` before, or
  /// without a list (a meeting from before signed invitees: its statement stays version 1).
  async fn invitees_to_sign(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<Vec<UserId>>, CoreError> {
    self.refresh_minimum_client_version().await;
    if !meeting_invitees::signs_invitees(self.highest_minimum_client_version()?) {
      return Ok(None);
    }
    Ok(match self.kept_signed_invitees(conversation)? {
      SignedInvitees::Known(invitees) => Some(invitees),
      SignedInvitees::Unknown => None,
    })
  }

  /// The users with a device in the meeting chat's group on this device, guests left out (they
  /// came in through signed adds); none without a group.
  fn members_in_group(&mut self, conversation: ConversationId) -> Result<Vec<UserId>, CoreError> {
    if !self.has_group(conversation)? {
      return Ok(Vec::new());
    }
    let in_group = self.group_users(conversation)?;
    let guests = self.guest_users(in_group.iter().copied())?;
    Ok(
      in_group
        .into_iter()
        .filter(|user| !guests.contains(user))
        .collect(),
    )
  }

  /// The sequence of the newest verified statement that named invitees.
  fn signed_invitees_sequence(&self, conversation: ConversationId) -> Result<Option<u64>, CoreError> {
    Ok(
      self
        .admitters_record(conversation)?
        .and_then(|record| record.maybe_invitees)
        .map(|invitees| invitees.sequence),
    )
  }

  /// A verified commit took `user` out of the conversation's group (#1726): neither the server's
  /// list nor any statement brings the user back into a meeting chat, only a verified commit that
  /// adds them again or this device's own choice. Noted whatever kind the server says the
  /// conversation is, so it can't suppress the note.
  pub(super) fn note_invitee_removed(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    let mut removed = self.removed_list(conversation)?;
    removed.retain(|known| *known != user);
    removed.push(user);
    if removed.len() > MAX_REMOVED_INVITEES {
      let excess = removed.len() - MAX_REMOVED_INVITEES;
      removed.drain(..excess);
    }
    self.save_removed_invitees(conversation, &removed)
  }

  /// `user` went into the group again, by a verified commit or the organizer's own choice.
  pub(super) fn note_invitee_added(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    let mut removed = self.removed_list(conversation)?;
    let before = removed.len();
    removed.retain(|known| *known != user);
    if removed.len() != before {
      self.save_removed_invitees(conversation, &removed)?;
    }
    Ok(())
  }

  /// Forgets the conversation's invitee records (the pending choice, the removals) with its other
  /// per-conversation state.
  pub(crate) fn forget_invitee_records(&self, conversation: ConversationId) -> Result<(), CoreError> {
    self.forget_chosen_invitees(conversation)?;
    Ok(self.store.delete(
      Table::SyncState,
      &format!("{REMOVED_INVITEES_PREFIX}{conversation}"),
    )?)
  }

  fn removed_invitees(&self, conversation: ConversationId) -> Result<BTreeSet<UserId>, CoreError> {
    Ok(self.removed_list(conversation)?.into_iter().collect())
  }

  fn removed_list(&self, conversation: ConversationId) -> Result<Vec<UserId>, CoreError> {
    Ok(
      records::get_json(
        &self.store,
        Table::SyncState,
        &format!("{REMOVED_INVITEES_PREFIX}{conversation}"),
      )?
      .unwrap_or_default(),
    )
  }

  fn save_removed_invitees(&self, conversation: ConversationId, removed: &[UserId]) -> Result<(), CoreError> {
    if removed.is_empty() {
      return Ok(self.store.delete(
        Table::SyncState,
        &format!("{REMOVED_INVITEES_PREFIX}{conversation}"),
      )?);
    }
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{REMOVED_INVITEES_PREFIX}{conversation}"),
      &removed,
    )
  }

  /// The organizer's choice on this device not signed yet, else the newest verified statement's
  /// invitees, as kept; nothing is fetched.
  fn kept_signed_invitees(&self, conversation: ConversationId) -> Result<SignedInvitees, CoreError> {
    let maybe_chosen: Option<ChosenInvitees> = records::get_json(
      &self.store,
      Table::SyncState,
      &format!("{CHOSEN_INVITEES_PREFIX}{conversation}"),
    )?;
    let maybe_signed = self
      .admitters_record(conversation)?
      .and_then(|record| record.maybe_invitees);
    Ok(meeting_invitees::newest_signed_invitees(
      maybe_chosen.as_ref(),
      maybe_signed
        .as_ref()
        .map(|signed| (signed.sequence, signed.invitees.as_slice())),
      &self.removed_invitees(conversation)?,
    ))
  }

  /// The organizer's choice on this device is in a statement now: from here on the statements
  /// speak for it, a newer one of another device included.
  fn forget_chosen_invitees(&self, conversation: ConversationId) -> Result<(), CoreError> {
    Ok(self.store.delete(
      Table::SyncState,
      &format!("{CHOSEN_INVITEES_PREFIX}{conversation}"),
    )?)
  }

  /// Checks the organizer-signed admitters of the meeting chat against the organizer's key as this
  /// device pinned it, so the server's word adds nothing, and keeps them: for
  /// [`admits_link_arrivals`] and for judging the admitters' commits.
  pub async fn check_meeting_admission(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.organizers_admitters(conversation).await.map(drop)
  }

  /// Before a commit in a meeting chat is judged or made: reads the organizer-signed admitters
  /// again when the change's signer is neither in the signed roster nor the organizer, so an
  /// admitter's add (#1103) is judged by the newest list. A failure keeps what this device had.
  pub(super) async fn refresh_meeting_admitters_for_commit(
    &mut self,
    conversation: ConversationId,
    signer: UserId,
  ) {
    let maybe_response = self
      .cached_conversations()
      .ok()
      .and_then(|all| all.get(&conversation).cloned());
    let Some(response) =
      maybe_response.filter(|response| response.kind == teamcraft_proto::ConversationKind::Meeting)
    else {
      return;
    };
    let is_vouched = response.maybe_organizer_user_id == Some(signer)
      || self.workspace().is_ok_and(|workspace| {
        workspace
          .roster
          .members
          .iter()
          .any(|member| member.user_id == signer)
      });
    if is_vouched {
      return;
    }
    if let Err(error) = self.organizers_admitters(conversation).await {
      tracing::warn!(conversation_id = %conversation, %error, "could not read the meeting's admitters before a commit");
    }
    self.refresh_meeting_hosts_for_commit(conversation).await;
  }

  /// The admitters of the meeting chat that count for a commit at the server's time `at_ms`: the
  /// newest list this device verified, while it is in force.
  pub(super) fn meeting_admitters_at(
    &self,
    conversation: ConversationId,
    at_ms: i64,
  ) -> Result<Vec<UserId>, CoreError> {
    Ok(
      self
        .admitters_record(conversation)?
        .filter(|record| at_ms <= record.expires_at_ms)
        .map(|record| record.admitters)
        .unwrap_or_default(),
    )
  }

  /// The verified admitters of the meeting chat if they name this device's user with its own key
  /// (the organizer's own device always counts); `None` otherwise.
  async fn own_admission(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<VerifiedMeetingAdmitters>, CoreError> {
    let me = self.account.user_id;
    let own_key = self.account.identity_public_key();
    Ok(
      self
        .organizers_admitters(conversation)
        .await?
        .filter(|admitters| admitters.is_admitter(convert::user(me), &own_key)),
    )
  }

  /// The server's newest admitters of the meeting chat, verified against the organizer's key as
  /// this device pinned it, by this device's clock.
  async fn organizers_admitters(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<VerifiedMeetingAdmitters>, CoreError> {
    let now_ms = self.now_ms();
    self.organizers_admitters_at(conversation, now_ms).await
  }

  /// [`Self::organizers_admitters`] by `now_ms` (0: whatever the end).
  async fn organizers_admitters_at(
    &mut self,
    conversation: ConversationId,
    now_ms: i64,
  ) -> Result<Option<VerifiedMeetingAdmitters>, CoreError> {
    let Some(organizer) = self.api.conversation(conversation).await?.maybe_organizer_user_id else {
      return Ok(None);
    };
    let directory = self.pin_identities([organizer]).await?;
    let organizer_key = directory
      .pinned_identity(convert::user(organizer))
      .ok_or(CoreError::UnknownIdentity(organizer))?;
    self
      .verified_admitters(conversation, None, &organizer_key.fingerprint(), now_ms)
      .await
  }

  /// The server's newest admitters of the meeting chat, verified against `organizer`, the chat
  /// and, where known, the meeting; by `now_ms` (0: whatever the end) and against the newest
  /// sequence this device saw, and kept. `None` when none is published or it doesn't verify
  /// (logged).
  async fn verified_admitters(
    &mut self,
    conversation: ConversationId,
    maybe_meeting: Option<MeetingId>,
    organizer: &IdentityFingerprint,
    now_ms: i64,
  ) -> Result<Option<VerifiedMeetingAdmitters>, CoreError> {
    let response = match self.api.meeting_admitters(conversation).await {
      Ok(response) => response,
      Err(error) if error.is_not_found() => return Ok(None),
      Err(error) => return Err(error.into()),
    };
    let expectation = AdmittersExpectation {
      organizer,
      maybe_meeting_id: maybe_meeting.map(convert::meeting),
      conversation_id: convert::conversation(conversation),
      now_ms: u64::try_from(now_ms).unwrap_or(0),
      maybe_seen_sequence: self.admitters_record(conversation)?.map(|record| record.sequence),
    };
    let verified = SignedMeetingAdmitters::from_bytes(response.statement.as_slice())
      .and_then(|signed| signed.verify(&expectation));
    match verified {
      Ok(admitters) if !has_known_ids(&admitters) => {
        tracing::warn!(conversation_id = %conversation, "the meeting's admitters name an ID this device can't use; ignored");
        Ok(None)
      },
      Ok(admitters) => {
        self.keep_admitters(conversation, &admitters)?;
        Ok(Some(admitters))
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "the meeting's admitters don't verify; ignored");
        Ok(None)
      },
    }
  }

  /// The users this organizer's device names as admitters: everybody with a device in the meeting
  /// chat's group but the organizer and guests, with the identity keys this device pinned.
  async fn admitter_candidates(&mut self, conversation: ConversationId) -> Result<Vec<Admitter>, CoreError> {
    let me = self.account.user_id;
    let in_group: Vec<UserId> = self
      .group_users(conversation)?
      .into_iter()
      .filter(|user| *user != me)
      .collect();
    let guests = self.guest_users(in_group.iter().copied())?;
    let members: Vec<UserId> = in_group
      .into_iter()
      .filter(|user| !guests.contains(user))
      .collect();
    let directory = self.pin_identities(members.iter().copied()).await?;
    Ok(
      members
        .into_iter()
        .filter_map(|user| {
          let user_id = convert::user(user);
          directory.pinned_identity(user_id).map(|identity_key| Admitter {
            user_id,
            identity_key,
          })
        })
        .collect(),
    )
  }

  fn admitters_record(&self, conversation: ConversationId) -> Result<Option<AdmittersRecord>, CoreError> {
    records::get_json(
      &self.store,
      Table::SyncState,
      &format!("{ADMITTERS_PREFIX}{conversation}"),
    )
  }

  /// Keeps a verified statement unless one with a higher sequence is kept already.
  fn keep_admitters(
    &self,
    conversation: ConversationId,
    verified: &VerifiedMeetingAdmitters,
  ) -> Result<(), CoreError> {
    let maybe_kept = self.admitters_record(conversation)?;
    if maybe_kept
      .as_ref()
      .is_some_and(|kept| kept.sequence > verified.sequence())
    {
      return Ok(());
    }
    let maybe_invitees = match verified.maybe_invitees() {
      Some(invitees) => Some(InviteesRecord {
        sequence: verified.sequence(),
        invitees: invitees
          .iter()
          .map(|user| convert::user_from_crypto(*user))
          .collect::<Result<_, _>>()?,
      }),
      None => maybe_kept.and_then(|kept| kept.maybe_invitees),
    };
    let me = convert::user(self.account.user_id);
    let mut admitters = Vec::new();
    for admitter in verified.admitters() {
      admitters.push(convert::user_from_crypto(admitter.user_id)?);
    }
    let record = AdmittersRecord {
      sequence: verified.sequence(),
      expires_at_ms: i64::try_from(verified.expires_at_ms()).unwrap_or(i64::MAX),
      admits_by_link: verified.link_admission() == LinkAdmission::ByThemselves,
      admitters,
      is_own: verified.is_admitter(me, &self.account.identity_public_key()),
      maybe_invitees,
    };
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{ADMITTERS_PREFIX}{conversation}"),
      &record,
    )?;
    // A pending choice made on an older statement is replaced by this newer decision.
    if verified.maybe_invitees().is_some()
      && let Some(chosen) = records::get_json::<ChosenInvitees>(
        &self.store,
        Table::SyncState,
        &format!("{CHOSEN_INVITEES_PREFIX}{conversation}"),
      )?
      && chosen
        .maybe_base_sequence
        .is_none_or(|base| verified.sequence() > base)
    {
      self.forget_chosen_invitees(conversation)?;
    }
    Ok(())
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

/// Whether this member's device lets people who came with the meeting link in by itself, by the
/// organizer-signed admitters of the meeting chat it verified last
/// ([`Client::check_meeting_admission`]): they name this user, the organizer signed "no lobby", and
/// they are still in force at `now_ms` (#1103). Unknown means no.
pub fn admits_link_arrivals<S: LocalStore>(store: &S, conversation: ConversationId, now_ms: i64) -> bool {
  records::get_json::<AdmittersRecord>(
    store,
    Table::SyncState,
    &format!("{ADMITTERS_PREFIX}{conversation}"),
  )
  .ok()
  .flatten()
  .is_some_and(|record| record.is_own && record.admits_by_link && now_ms <= record.expires_at_ms)
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

/// Whether every user ID a verified statement names is one this device can use; a statement that
/// names another counts as not verifying.
fn has_known_ids(verified: &VerifiedMeetingAdmitters) -> bool {
  let admitters = verified.admitters().iter().map(|admitter| admitter.user_id);
  let invitees = verified.maybe_invitees().unwrap_or_default().iter().copied();
  admitters
    .chain(invitees)
    .all(|user| convert::user_from_crypto(user).is_ok())
}
