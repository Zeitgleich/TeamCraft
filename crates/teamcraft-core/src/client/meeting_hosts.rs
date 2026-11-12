//! Co-hosts and handing a meeting over (#1720; e2ee.md §4.6, "Meeting hosts"; docs/meetings.md).
//!
//! The organizer's identity key signs who hosts the meeting (`teamcraft_crypto::meeting_hosts`); a
//! list naming another organizer hands the meeting over, and from then on the new organizer's key
//! signs. A device follows the lists from the key it trusts: a guest from `v` of its link, a member
//! from the key of the device that founded the chat's group (the organizer when the group was
//! made). So a co-host's device lets guests in and a guest accepts its welcome without the
//! server's word, and a group founded by a former organizer stays the meeting's.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::{IdentityFingerprint, IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::meeting_hosts::{
  Host, HostsDraft, HostsExpectation, SignedMeetingHosts, VerifiedMeetingHosts, verify_chain,
};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::mls::{ConversationGroup, GroupMember};
use teamcraft_proto::{Base64Bytes, ConversationId, MeetingId, PutMeetingHostsRequest, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::trust::PinnedDirectory;

/// The futures this module hands to the sync, catch-up and command paths, boxed where they are
/// made: the callers' futures, and their frames while polled, hold a pointer instead of the
/// checks' state (the core suite's catch-up tests overflowed their stack on Windows otherwise).
type HostsFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The hosts of a meeting chat this device verified last ([`HostsRecord`]).
const HOSTS_PREFIX: &str = "meeting-hosts/";
/// How the last change of a meeting chat's hosts on this device went ([`HostsChangeRecord`]).
const HOSTS_CHANGE_PREFIX: &str = "meeting-hosts-change/";

/// The hosts of a meeting chat as this device verified them last. A later check refuses a list
/// older than `sequence` (the server bringing back a co-host the organizer took off) and falls
/// back on this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HostsRecord {
  sequence: u64,
  /// The current organizer's identity key fingerprint (hex).
  organizer_fingerprint: String,
  /// The current organizer, where a list named them (after a handover the server's naming may
  /// still be the old one here).
  #[serde(default)]
  maybe_organizer: Option<UserId>,
  /// The co-hosts with their identity key fingerprints (hex).
  co_hosts: Vec<(UserId, String)>,
}

/// How the last change of a meeting chat's hosts that this device made went, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostsChangeRecord {
  Saving,
  Saved,
  Failed { reason: String },
}

/// What the organizer's device changes about the hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostsChange {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  /// Everybody who hosts besides the organizer afterwards.
  pub co_hosts: Vec<UserId>,
  /// Hands the meeting to this member of its chat.
  pub maybe_new_organizer: Option<UserId>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Signs and publishes who hosts the meeting (the organizer's devices only): the co-hosts with
  /// the identity keys this device pinned for them, and either this user as organizer or the one
  /// the meeting is handed to. The sequence is above every list the server holds and the time of
  /// signing, so the newest of two organizer devices stays ahead. The outcome is kept for the UI
  /// ([`meeting_hosts_change`]).
  pub fn change_meeting_hosts<'a>(
    &'a mut self,
    change: &'a HostsChange,
  ) -> HostsFuture<'a, Result<(), CoreError>> {
    Box::pin(self.change_meeting_hosts_unboxed(change))
  }

  /// After a handover to this user, signs the hosts the handover named again with this user's key,
  /// so that links with this key (made from now on) find the co-hosts too. `Ok(true)` when it
  /// published a list; nothing to do otherwise.
  pub fn sync_meeting_hosts(
    &mut self,
    meeting: MeetingId,
    conversation: ConversationId,
  ) -> HostsFuture<'_, Result<bool, CoreError>> {
    Box::pin(self.sync_meeting_hosts_unboxed(meeting, conversation))
  }

  /// Whether this member's device hosts the meeting chat by the verified host lists (organizer or
  /// co-host with this device's own key): such a device may let guests in, whose pages accept its
  /// welcome by the same lists.
  pub(super) fn is_own_hosting(
    &mut self,
    conversation: ConversationId,
  ) -> HostsFuture<'_, Result<bool, CoreError>> {
    Box::pin(self.is_own_hosting_unboxed(conversation))
  }

  /// Whether a meeting chat's group whose first member is `founder` is the meeting's although the
  /// server names another organizer, `organizer`: the host lists hand the meeting over from the
  /// key that certified the founder's leaf, in a chain of lists each signed by the organizer
  /// before, to the organizer's key as this device pinned it.
  pub(super) fn is_handed_over_group<'a>(
    &'a mut self,
    conversation: ConversationId,
    group: &'a ConversationGroup,
    founder: GroupMember,
    directory: &'a PinnedDirectory,
    organizer: UserId,
  ) -> HostsFuture<'a, Result<bool, CoreError>> {
    Box::pin(async move {
      let Some(founder_device) = group.verified_member(founder, directory)? else {
        return Ok(false);
      };
      self
        .is_handed_over_from(conversation, founder_device.identity_key(), organizer)
        .await
    })
  }

  /// For a guest's device: whether the device that sent its welcome may let the guest in, by the
  /// key of the link (`anchor`, hex): a host by the lists that follow from that key (the current
  /// organizer or a co-host, #1720), else an admitter the current organizer's key signed for the
  /// meeting (#1103). A list older than one seen falls back on the hosts kept from before.
  pub(super) fn is_hosts_welcome<'a>(
    &'a mut self,
    conversation: ConversationId,
    anchor: &'a str,
    device: &'a VerifiedDevice,
  ) -> HostsFuture<'a, Result<bool, CoreError>> {
    Box::pin(async move {
      let Ok(anchor) = anchor.parse::<IdentityFingerprint>() else {
        return Ok(false);
      };
      let maybe_meeting = self.guest_meeting(conversation)?;
      let (is_hosts, organizer) = self
        .guest_hosts_check(conversation, maybe_meeting, &anchor, device)
        .await?;
      if is_hosts {
        return Ok(true);
      }
      self
        .is_signed_admitters_device(conversation, &organizer.to_string(), device)
        .await
    })
  }

  /// Before a commit by somebody the roster doesn't vouch for is judged or made in a meeting chat:
  /// reads the host lists again, so a co-host's add is judged by the newest list. A failure keeps
  /// what this device had.
  pub(super) fn refresh_meeting_hosts_for_commit(
    &mut self,
    conversation: ConversationId,
  ) -> HostsFuture<'_, ()> {
    Box::pin(self.refresh_meeting_hosts_for_commit_unboxed(conversation))
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The body of [`Self::change_meeting_hosts`].
  async fn change_meeting_hosts_unboxed(&mut self, change: &HostsChange) -> Result<(), CoreError> {
    self.note_hosts_change(change.conversation, &HostsChangeRecord::Saving)?;
    let result = self.sign_meeting_hosts(change).await;
    let record = match &result {
      Ok(()) => HostsChangeRecord::Saved,
      Err(error) => HostsChangeRecord::Failed {
        reason: error.to_string(),
      },
    };
    self.note_hosts_change(change.conversation, &record)?;
    result
  }

  /// The body of [`Self::sync_meeting_hosts`].
  async fn sync_meeting_hosts_unboxed(
    &mut self,
    meeting: MeetingId,
    conversation: ConversationId,
  ) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    if self.api.conversation(conversation).await?.maybe_organizer_user_id != Some(me) {
      return Ok(false);
    }
    let Some(verified) = self.member_hosts(conversation, Some(meeting)).await? else {
      return Ok(false);
    };
    let own = self.account.identity_public_key();
    if !verified.is_handover_pending() || !verified.is_organizer_key(&own) {
      return Ok(false);
    }
    let mut co_hosts = Vec::new();
    for host in verified.co_hosts() {
      co_hosts.push(convert::user_from_crypto(host.user_id)?);
    }
    self
      .sign_meeting_hosts(&HostsChange {
        meeting,
        conversation,
        co_hosts,
        maybe_new_organizer: None,
      })
      .await?;
    tracing::info!(meeting_id = %meeting, "signed the hosts of a meeting handed over to this user");
    Ok(true)
  }

  /// The body of [`Self::is_own_hosting`].
  async fn is_own_hosting_unboxed(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    let me = convert::user(self.account.user_id);
    let own = self.account.identity_public_key();
    Ok(
      self
        .member_hosts(conversation, None)
        .await?
        .is_some_and(|hosts| hosts.is_host(me, &own)),
    )
  }

  /// Whether the host lists hand the meeting from `founder_key` to `organizer`'s pinned key.
  pub(super) async fn is_handed_over_from(
    &mut self,
    conversation: ConversationId,
    founder_key: &IdentityPublicKey,
    organizer: UserId,
  ) -> Result<bool, CoreError> {
    let directory = self.pin_identities([organizer]).await?;
    let Some(organizer_key) = directory.pinned_identity(convert::user(organizer)) else {
      return Ok(false);
    };
    let statements = self.host_statements(conversation).await?;
    let anchor = founder_key.fingerprint();
    let expectation = HostsExpectation {
      anchor: &anchor,
      maybe_meeting_id: None,
      conversation_id: convert::conversation(conversation),
      maybe_seen_sequence: None,
    };
    Ok(verify_chain(&statements, &expectation).is_ok_and(|hosts| hosts.is_organizer_key(&organizer_key)))
  }

  /// Whether `device` hosts the meeting by the lists that follow from `anchor`, and the current
  /// organizer's fingerprint (what the admitters must be signed by).
  async fn guest_hosts_check(
    &mut self,
    conversation: ConversationId,
    meeting: Option<MeetingId>,
    anchor: &IdentityFingerprint,
    device: &VerifiedDevice,
  ) -> Result<(bool, IdentityFingerprint), CoreError> {
    let statements = self.host_statements(conversation).await?;
    let expectation = HostsExpectation {
      anchor,
      maybe_meeting_id: meeting.map(convert::meeting),
      conversation_id: convert::conversation(conversation),
      maybe_seen_sequence: self.hosts_record(conversation)?.map(|record| record.sequence),
    };
    match verify_chain(&statements, &expectation) {
      Ok(hosts) => {
        self.keep_hosts(conversation, &hosts)?;
        Ok((hosts.hosts_device(device), *hosts.organizer_fingerprint()))
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "the meeting's host lists don't verify; the kept ones count");
        let Some(record) = self.hosts_record(conversation)? else {
          return Ok((device.identity_key().fingerprint() == *anchor, *anchor));
        };
        let organizer = record
          .organizer_fingerprint
          .parse::<IdentityFingerprint>()
          .unwrap_or(*anchor);
        Ok((record.hosts_device(device), organizer))
      },
    }
  }

  async fn sign_meeting_hosts(&mut self, change: &HostsChange) -> Result<(), CoreError> {
    let me = self.account.user_id;
    let conversation = change.conversation;
    if self.api.conversation(conversation).await?.maybe_organizer_user_id != Some(me) {
      return Err(CoreError::NotMeetingOrganizer(conversation));
    }
    let statements = self.host_statements(conversation).await?;
    let newest_on_server = statements
      .iter()
      .map(SignedMeetingHosts::unverified_sequence)
      .max()
      .unwrap_or(0);
    let seen = self
      .hosts_record(conversation)?
      .map_or(0, |record| record.sequence);
    let now = u64::try_from(self.now_ms()).unwrap_or(0);
    // Time as the floor keeps a later list ahead of an earlier one of the organizer's other
    // devices, even of one this device never saw.
    let sequence = newest_on_server.max(seen).saturating_add(1).max(now);
    let named: Vec<UserId> = change
      .co_hosts
      .iter()
      .copied()
      .chain(change.maybe_new_organizer)
      .collect();
    let directory = self.pin_identities(named.iter().copied()).await?;
    let host_of = |user: UserId| -> Result<Host, CoreError> {
      let user_id = convert::user(user);
      let identity_key = directory
        .pinned_identity(user_id)
        .ok_or(CoreError::UnknownIdentity(user))?;
      Ok(Host {
        user_id,
        identity_key,
      })
    };
    let organizer = match change.maybe_new_organizer {
      Some(new_organizer) => host_of(new_organizer)?,
      None => Host {
        user_id: convert::user(me),
        identity_key: self.account.identity_public_key(),
      },
    };
    let co_hosts = change
      .co_hosts
      .iter()
      .copied()
      .map(host_of)
      .collect::<Result<Vec<_>, _>>()?;
    let signed = HostsDraft {
      meeting_id: convert::meeting(change.meeting),
      conversation_id: convert::conversation(conversation),
      sequence,
      signer_user_id: convert::user(me),
      organizer,
      co_hosts,
    }
    .sign(self.account.key_bag.identity())?;
    let request = PutMeetingHostsRequest {
      statement: Base64Bytes::new(signed.to_bytes()?),
    };
    self.api.put_meeting_hosts(conversation, &request).await?;
    // What this device signed is what it would verify from its own key.
    let own = self.account.identity_public_key().fingerprint();
    let verified = verify_chain(
      &[signed],
      &HostsExpectation {
        anchor: &own,
        maybe_meeting_id: Some(convert::meeting(change.meeting)),
        conversation_id: convert::conversation(conversation),
        maybe_seen_sequence: None,
      },
    )?;
    self.keep_hosts(conversation, &verified)?;
    tracing::info!(meeting_id = %change.meeting, sequence, is_handover = change.maybe_new_organizer.is_some(), "published the meeting's hosts");
    Ok(())
  }

  /// The hosts of the meeting chat as a member's device verifies them: from the key of the device
  /// that founded the chat's group (as this device verified its certificate), or, without a group
  /// here yet, from the organizer's key as pinned. `None` when there is neither.
  async fn member_hosts(
    &mut self,
    conversation: ConversationId,
    meeting: Option<MeetingId>,
  ) -> Result<Option<VerifiedMeetingHosts>, CoreError> {
    let Some(anchor) = self.member_anchor(conversation).await? else {
      return Ok(None);
    };
    let statements = self.host_statements(conversation).await?;
    let expectation = HostsExpectation {
      anchor: &anchor,
      maybe_meeting_id: meeting.map(convert::meeting),
      conversation_id: convert::conversation(conversation),
      maybe_seen_sequence: self.hosts_record(conversation)?.map(|record| record.sequence),
    };
    match verify_chain(&statements, &expectation) {
      Ok(hosts) => {
        self.keep_hosts(conversation, &hosts)?;
        Ok(Some(hosts))
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "the meeting's host lists don't verify; ignored");
        Ok(None)
      },
    }
  }

  async fn member_anchor(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<IdentityFingerprint>, CoreError> {
    let maybe_founder = match self.groups.get(&conversation) {
      Some(group) => group.members()?.into_iter().next(),
      None => None,
    };
    if let Some(founder) = maybe_founder {
      let directory = self
        .pin_identities([convert::user_from_crypto(founder.user_id)?])
        .await?;
      if let Some(group) = self.groups.get(&conversation)
        && let Some(device) = group.verified_member(founder, &directory)?
      {
        return Ok(Some(device.identity_key().fingerprint()));
      }
    }
    let Some(organizer) = self.api.conversation(conversation).await?.maybe_organizer_user_id else {
      return Ok(None);
    };
    let directory = self.pin_identities([organizer]).await?;
    Ok(
      directory
        .pinned_identity(convert::user(organizer))
        .map(|key| key.fingerprint()),
    )
  }

  /// The server's host lists of the meeting chat, those that decode; none before the first.
  async fn host_statements(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<SignedMeetingHosts>, CoreError> {
    let response = match self.api.meeting_hosts(conversation).await {
      Ok(response) => response,
      Err(error) if error.is_not_found() => return Ok(Vec::new()),
      Err(error) => return Err(error.into()),
    };
    Ok(
      response
        .statements
        .iter()
        .filter_map(|bytes| SignedMeetingHosts::from_bytes(bytes.as_slice()).ok())
        .collect(),
    )
  }

  /// The co-hosts of the host lists this device verified last for the meeting chat: they let
  /// guests in like the admitters the organizer signs, also where the roster doesn't vouch for
  /// them (the members' rule for commits, `group_authority`).
  pub(super) fn kept_co_hosts(&self, conversation: ConversationId) -> Result<Vec<UserId>, CoreError> {
    Ok(
      self
        .hosts_record(conversation)?
        .map(|record| record.co_hosts.into_iter().map(|(user, _)| user).collect())
        .unwrap_or_default(),
    )
  }

  /// The organizer the host lists this device verified last name: after a handover the one the
  /// meeting went to, whom the members' rule vouches for like the organizer the server names.
  pub(super) fn kept_organizer(&self, conversation: ConversationId) -> Result<Option<UserId>, CoreError> {
    Ok(
      self
        .hosts_record(conversation)?
        .and_then(|record| record.maybe_organizer),
    )
  }

  /// The body of [`Self::refresh_meeting_hosts_for_commit`].
  async fn refresh_meeting_hosts_for_commit_unboxed(&mut self, conversation: ConversationId) {
    let is_guest_device = self.is_guest_device().unwrap_or(false);
    let result = if is_guest_device {
      Ok(None)
    } else {
      self.member_hosts(conversation, None).await
    };
    if let Err(error) = result {
      tracing::warn!(conversation_id = %conversation, %error, "could not read the meeting's host lists before a commit");
    }
  }

  fn hosts_record(&self, conversation: ConversationId) -> Result<Option<HostsRecord>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &hosts_key(conversation))
  }

  /// Keeps verified hosts unless hosts of a higher sequence are kept already.
  fn keep_hosts(
    &self,
    conversation: ConversationId,
    verified: &VerifiedMeetingHosts,
  ) -> Result<(), CoreError> {
    if self
      .hosts_record(conversation)?
      .is_some_and(|kept| kept.sequence > verified.sequence())
    {
      return Ok(());
    }
    let mut co_hosts = Vec::new();
    for host in verified.co_hosts() {
      co_hosts.push((
        convert::user_from_crypto(host.user_id)?,
        host.identity_key.fingerprint().to_string(),
      ));
    }
    let maybe_organizer = verified
      .maybe_organizer()
      .map(|host| convert::user_from_crypto(host.user_id))
      .transpose()?;
    let record = HostsRecord {
      sequence: verified.sequence(),
      organizer_fingerprint: verified.organizer_fingerprint().to_string(),
      maybe_organizer,
      co_hosts,
    };
    records::put_json(&self.store, Table::SyncState, &hosts_key(conversation), &record)
  }

  fn note_hosts_change(
    &self,
    conversation: ConversationId,
    record: &HostsChangeRecord,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{HOSTS_CHANGE_PREFIX}{conversation}"),
      record,
    )
  }
}

impl HostsRecord {
  fn hosts_device(&self, device: &VerifiedDevice) -> bool {
    let fingerprint = device.identity_key().fingerprint().to_string();
    fingerprint == self.organizer_fingerprint
      || convert::user_from_crypto(device.user_id()).is_ok_and(|user| {
        self
          .co_hosts
          .iter()
          .any(|(co_host, key)| *co_host == user && *key == fingerprint)
      })
  }
}

/// How the last change of the meeting chat's hosts that this device made went
/// ([`Client::change_meeting_hosts`]); `None` before the first.
pub fn meeting_hosts_change<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
) -> Option<HostsChangeRecord> {
  records::get_json(
    store,
    Table::SyncState,
    &format!("{HOSTS_CHANGE_PREFIX}{conversation}"),
  )
  .ok()
  .flatten()
}

fn hosts_key(conversation: ConversationId) -> String {
  format!("{HOSTS_PREFIX}{conversation}")
}
