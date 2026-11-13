//! Co-hosts and handing a meeting over (#1720; e2ee.md §4.6, "Meeting hosts"; docs/meetings.md).
//!
//! The organizer's identity key signs who hosts the meeting (`teamcraft_crypto::meeting_hosts`); a
//! list naming another organizer hands the meeting over, and from then on the new organizer's key
//! signs. A device follows the lists from the key it trusts: a guest from `v` of its link, a member
//! from the key of the device that founded the chat's group (the organizer when the group was
//! made). So a co-host's device lets guests in and a guest accepts its welcome without the
//! server's word, and a group founded by a former organizer stays the meeting's.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::{IdentityFingerprint, IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::meeting_hosts::{
  Host, HostsDraft, HostsExpectation, HostsHash, SignedMeetingHosts, VerifiedMeetingHosts, previous_for_next,
  verify_chain,
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
/// The fingerprint (hex) of the identity key that founded a meeting chat's anchor this device
/// pinned (#1747): only host lists verified from it count for the chat from then on.
const PINNED_FOUNDER_PREFIX: &str = "meeting-pinned-founder/";

/// At most this many keys' host records per meeting chat ([`KeptHosts`]): the pinned founder's,
/// a guest link's `v`, this device's own key, the organizers the server named before the device
/// had the group.
const MAX_KEPT_ANCHORS: usize = 8;

/// The host records of a meeting chat, one per key a chain was verified from (#1865): each with
/// its own sequence and handover for the rollback and fork checks of chains from that key, so a
/// guest's chain from `v` keeps its check once the device pinned a founder with another key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct KeptHosts {
  /// By the fingerprint (hex) of the key the chain was verified from.
  by_anchor: BTreeMap<String, HostsRecord>,
  /// The key of the record kept last: the one that counts for the chat where no founder is
  /// pinned ([`Client::hosts_record`]).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_latest: Option<String>,
}

/// The stored form: since #1865 [`KeptHosts`]; before, one [`HostsRecord`] for the chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredHosts {
  ByAnchor(KeptHosts),
  Single(HostsRecord),
}

/// The hosts of a meeting chat as this device verified them last from one key. A later check
/// from that key refuses a list older than `sequence` (the server bringing back a co-host the
/// organizer took off) or a chain without `maybe_handover` (a former organizer's fork, #1864) and
/// falls back on this.
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
  /// The fingerprint (hex) of the key the chain was verified from, in the one record of a chat
  /// from before #1865 (`None` in those from before #1858); the key of [`KeptHosts`] since.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_anchor: Option<String>,
  /// The newest handover counted from the key (#1864); `None` before one, or in records from
  /// before.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_handover: Option<HostsHash>,
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
    // Against the newest lists this device kept: a server can't replay a chain that ends at an
    // organizer the meeting was handed on from since, nor serve a former organizer's fork.
    let expectation = self.hosts_expectation(conversation, &anchor, None)?;
    match verify_chain(&statements, &expectation) {
      Ok(hosts) => {
        self.keep_hosts(conversation, &hosts, &anchor)?;
        Ok(hosts.is_organizer_key(&organizer_key))
      },
      Err(error) => {
        // Fails closed: the founder stays the organizer (availability only). A kept list proves
        // nothing here; it may come from another key.
        tracing::warn!(conversation_id = %conversation, %error, "the meeting's host lists don't verify; no handover counts");
        Ok(false)
      },
    }
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
    let expectation = self.hosts_expectation(conversation, anchor, meeting)?;
    match verify_chain(&statements, &expectation) {
      Ok(hosts) => {
        self.keep_hosts(conversation, &hosts, anchor)?;
        Ok((hosts.hosts_device(device), *hosts.organizer_fingerprint()))
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "the meeting's host lists don't verify; the kept ones count");
        let Some(record) = self.hosts_record_from(conversation, anchor)? else {
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
    let seen = self.newest_kept_hosts_sequence(conversation)?;
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
    let own_key = self.account.identity_public_key();
    // Links the list to the handover this user took the meeting over with (#1864).
    let maybe_previous = previous_for_next(
      &statements,
      &own_key,
      Some(convert::meeting(change.meeting)),
      convert::conversation(conversation),
    );
    let signed = HostsDraft {
      meeting_id: convert::meeting(change.meeting),
      conversation_id: convert::conversation(conversation),
      sequence,
      signer_user_id: convert::user(me),
      organizer,
      co_hosts,
      maybe_previous,
    }
    .sign(self.account.key_bag.identity())?;
    let request = PutMeetingHostsRequest {
      statement: Base64Bytes::new(signed.to_bytes()?),
    };
    self.api.put_meeting_hosts(conversation, &request).await?;
    // What this device signed is what it verifies from its own key, after the handovers the
    // server keeps: kept so, so that this key's own rollback and fork checks keep counting.
    let own = own_key.fingerprint();
    let mut chain: Vec<SignedMeetingHosts> = statements
      .into_iter()
      .filter(SignedMeetingHosts::is_handover)
      .collect();
    chain.push(signed);
    let expectation = self.hosts_expectation(conversation, &own, Some(change.meeting))?;
    match verify_chain(&chain, &expectation) {
      Ok(verified) => self.keep_hosts(conversation, &verified, &own)?,
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "the host list this device signed doesn't verify from its own key; not kept");
      },
    }
    // Where this device pinned another founder (a meeting handed to this user), the chain from
    // that key is the one that counts for the chat: read back.
    let is_founder = self
      .pinned_founder_fingerprint(conversation)?
      .is_none_or(|founder| founder == own.to_string());
    if !is_founder && let Err(error) = self.member_hosts(conversation, Some(change.meeting)).await {
      tracing::warn!(conversation_id = %conversation, %error, "could not read the meeting's host lists back");
    }
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
    let expectation = self.hosts_expectation(conversation, &anchor, meeting)?;
    match verify_chain(&statements, &expectation) {
      Ok(hosts) => {
        self.keep_hosts(conversation, &hosts, &anchor)?;
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

  /// The co-hosts of the host lists that count for the meeting chat ([`Self::hosts_record`]: the
  /// pinned founder's record where there is a pin, else the one kept last): they let
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

  /// The organizer the host lists that count for the chat name: after a handover the one the
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

  /// The host lists that count for the chat (the members' rule, [`Self::kept_co_hosts`]): those
  /// verified from the pinned founder's key where this device pinned the meeting chat's anchor (a
  /// list kept from another key, such as the server's organizer before the device entered the
  /// group or a guest link's `v`, counts for nothing there), else those kept last.
  fn hosts_record(&self, conversation: ConversationId) -> Result<Option<HostsRecord>, CoreError> {
    let mut kept = self.kept_hosts(conversation)?;
    let maybe_anchor = match self.pinned_founder_fingerprint(conversation)? {
      Some(founder) => Some(founder),
      None => kept.maybe_latest.clone(),
    };
    Ok(maybe_anchor.and_then(|anchor| kept.by_anchor.remove(&anchor)))
  }

  /// The host lists kept as verified from `anchor`.
  fn hosts_record_from(
    &self,
    conversation: ConversationId,
    anchor: &IdentityFingerprint,
  ) -> Result<Option<HostsRecord>, CoreError> {
    Ok(
      self
        .kept_hosts(conversation)?
        .by_anchor
        .remove(&anchor.to_string()),
    )
  }

  /// What a chain from `anchor` must match: the chat (and meeting, where known), and not older
  /// nor forked from what this device kept from the same key. A chain from another key neither
  /// raises nor blocks it.
  fn hosts_expectation<'a>(
    &self,
    conversation: ConversationId,
    anchor: &'a IdentityFingerprint,
    meeting: Option<MeetingId>,
  ) -> Result<HostsExpectation<'a>, CoreError> {
    let maybe_kept = self.hosts_record_from(conversation, anchor)?;
    Ok(HostsExpectation {
      anchor,
      maybe_meeting_id: meeting.map(convert::meeting),
      conversation_id: convert::conversation(conversation),
      maybe_seen_sequence: maybe_kept.as_ref().map(|kept| kept.sequence),
      maybe_seen_handover: maybe_kept.and_then(|kept| kept.maybe_handover),
    })
  }

  /// The newest sequence kept from any key: the organizer's next list goes above it.
  fn newest_kept_hosts_sequence(&self, conversation: ConversationId) -> Result<u64, CoreError> {
    Ok(
      self
        .kept_hosts(conversation)?
        .by_anchor
        .values()
        .map(|record| record.sequence)
        .max()
        .unwrap_or(0),
    )
  }

  /// Every host record of the chat, in the format from #1865 (a record of before reads as the
  /// one record kept).
  fn kept_hosts(&self, conversation: ConversationId) -> Result<KeptHosts, CoreError> {
    let maybe_stored: Option<StoredHosts> =
      records::get_json(&self.store, Table::SyncState, &hosts_key(conversation))?;
    Ok(match maybe_stored {
      None => KeptHosts::default(),
      Some(StoredHosts::ByAnchor(kept)) => kept,
      Some(StoredHosts::Single(mut record)) => {
        // Unanchored records (from before #1858) count for the chat until the next is kept, but
        // never for a rollback check: no key is the empty string.
        let anchor = record.maybe_anchor.take().unwrap_or_default();
        KeptHosts {
          by_anchor: BTreeMap::from([(anchor.clone(), record)]),
          maybe_latest: Some(anchor),
        }
      },
    })
  }

  fn pinned_founder_fingerprint(&self, conversation: ConversationId) -> Result<Option<String>, CoreError> {
    records::get_json(
      &self.store,
      Table::SyncState,
      &format!("{PINNED_FOUNDER_PREFIX}{conversation}"),
    )
  }

  /// This device pinned the meeting chat's anchor, founded by `founder` (#1747): from now on only
  /// host lists verified from that key count for the chat ([`Self::hosts_record`]). Those kept
  /// from other keys stay, for their own rollback and fork checks only (#1865).
  pub(crate) fn note_pinned_meeting_founder(
    &self,
    conversation: ConversationId,
    founder: &IdentityFingerprint,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &format!("{PINNED_FOUNDER_PREFIX}{conversation}"),
      &founder.to_string(),
    )
  }

  /// Forgets the meeting chat's kept host lists and pinned founder with its other records.
  pub(crate) fn forget_host_records(&self, conversation: ConversationId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &hosts_key(conversation))?;
    Ok(self.store.delete(
      Table::SyncState,
      &format!("{PINNED_FOUNDER_PREFIX}{conversation}"),
    )?)
  }

  /// Keeps hosts verified from `anchor` in that key's own record (#1865), unless hosts of a higher
  /// sequence from the same key are kept already. Other keys' records stay; beyond
  /// [`MAX_KEPT_ANCHORS`] the oldest of them goes, never the pinned founder's.
  fn keep_hosts(
    &self,
    conversation: ConversationId,
    verified: &VerifiedMeetingHosts,
    anchor: &IdentityFingerprint,
  ) -> Result<(), CoreError> {
    let anchor_hex = anchor.to_string();
    let mut kept = self.kept_hosts(conversation)?;
    if kept
      .by_anchor
      .get(&anchor_hex)
      .is_some_and(|record| record.sequence > verified.sequence())
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
      maybe_anchor: None,
      maybe_handover: verified.maybe_handover(),
    };
    kept.by_anchor.insert(anchor_hex.clone(), record);
    kept.maybe_latest = Some(anchor_hex.clone());
    let maybe_founder = self.pinned_founder_fingerprint(conversation)?;
    while kept.by_anchor.len() > MAX_KEPT_ANCHORS {
      let maybe_oldest = kept
        .by_anchor
        .iter()
        .filter(|(key, _)| **key != anchor_hex && Some(*key) != maybe_founder.as_ref())
        .min_by_key(|(_, record)| record.sequence)
        .map(|(key, _)| key.clone());
      let Some(oldest) = maybe_oldest else {
        break;
      };
      kept.by_anchor.remove(&oldest);
    }
    records::put_json(
      &self.store,
      Table::SyncState,
      &hosts_key(conversation),
      &StoredHosts::ByAnchor(kept),
    )
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

/// A guest's checks of the host lists over the fake server, with lists signed here by keys of the
/// test's own (the server never checks whose keys they are).
#[cfg(all(test, not(target_arch = "wasm32")))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#[allow(clippy::unwrap_used)]
mod tests {
  use std::sync::Arc;

  use rand_core::OsRng;
  use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair};
  use teamcraft_crypto::ids::DeviceId as SignedDeviceId;

  use super::*;
  use crate::ClientSettings;
  use crate::api::fake::{FakeServer, FakeServerApi};
  use crate::clock::ManualClock;
  use crate::store::MemoryStore;

  type TestClient = Client<FakeServerApi, Arc<MemoryStore>>;

  /// #1865: a guest with a link Bob made after Alice handed him the meeting (`v` is Bob's key),
  /// whose device pinned the group Alice founded. Bob takes Alice off as co-host; the server then
  /// serves the older chain from `v` that still lists her.
  #[tokio::test]
  async fn a_link_made_after_a_handover_keeps_its_rollback_check_once_the_founder_is_pinned() {
    let mut rig = Rig::new().await;
    let (alice, bob) = (rig.person("Alice"), rig.person("Bob"));
    let to_bob = rig.list(&alice, &bob, &[&alice], 10, None);
    let with_alice = rig.list(&bob, &bob, &[&alice], 11, Some(&to_bob));
    let without_alice = rig.list(&bob, &bob, &[], 12, Some(&to_bob));
    let link = bob.fingerprint();
    rig
      .client
      .note_pinned_meeting_founder(rig.chat, &alice.fingerprint())
      .unwrap();

    assert!(rig.admits(&link, &alice, &[&to_bob, &with_alice]).await);
    assert!(!rig.admits(&link, &alice, &[&to_bob, &without_alice]).await);

    // The older chain from `v` again: refused as a rollback, the kept hosts count.
    assert!(!rig.admits(&link, &alice, &[&to_bob, &with_alice]).await);
    assert!(rig.admits(&link, &bob, &[&to_bob, &with_alice]).await);
    let kept = rig.client.hosts_record_from(rig.chat, &link).unwrap().unwrap();
    assert_eq!(kept.sequence, 12);
    // The chat's own hosts are the pinned founder's chain only: none kept from it.
    assert!(rig.client.kept_co_hosts(rig.chat).unwrap().is_empty());
    assert_eq!(rig.client.kept_organizer(rig.chat).unwrap(), None);
  }

  /// #1864: Alice hands the meeting to Bob, Bob to Carol. Bob signs a list above Carol's naming
  /// himself organizer again, and the server withholds his handover to Carol.
  #[tokio::test]
  async fn a_former_organizers_fork_falls_back_on_the_kept_hosts() {
    let mut rig = Rig::new().await;
    let (alice, bob, carol) = (rig.person("Alice"), rig.person("Bob"), rig.person("Carol"));
    let to_bob = rig.list(&alice, &bob, &[], 10, None);
    let to_carol = rig.list(&bob, &carol, &[], 11, Some(&to_bob));
    let carols = rig.list(&carol, &carol, &[], 12, Some(&to_carol));
    let link = alice.fingerprint();
    assert!(rig.admits(&link, &carol, &[&to_bob, &to_carol, &carols]).await);
    assert!(!rig.admits(&link, &bob, &[&to_bob, &to_carol, &carols]).await);
    let kept = rig.client.hosts_record_from(rig.chat, &link).unwrap().unwrap();
    assert_eq!(kept.maybe_handover, Some(to_carol.hash().unwrap()));

    let fork = rig.list(&bob, &bob, &[], 13, Some(&to_bob));

    assert!(!rig.admits(&link, &bob, &[&to_bob, &fork]).await);
    assert!(
      rig.admits(&link, &carol, &[&to_bob, &fork]).await,
      "the kept hosts"
    );
    // With the handover served again, the fork is passed over and the chain verifies.
    assert!(
      rig
        .admits(&link, &carol, &[&to_bob, &to_carol, &carols, &fork])
        .await
    );
  }

  /// A record kept before #1865 (one per chat, with the key it was verified from) reads as the
  /// record of that key, and keeping another key's adds to it.
  #[tokio::test]
  async fn a_record_from_before_reads_as_its_keys() {
    let mut rig = Rig::new().await;
    let (alice, bob) = (rig.person("Alice"), rig.person("Bob"));
    let legacy = serde_json::json!({
      "sequence": 5,
      "organizer_fingerprint": alice.fingerprint().to_string(),
      "maybe_organizer": alice.user,
      "co_hosts": [[bob.user, bob.fingerprint().to_string()]],
      "maybe_anchor": alice.fingerprint().to_string(),
    });
    rig
      .client
      .store
      .put(
        Table::SyncState,
        &hosts_key(rig.chat),
        legacy.to_string().as_bytes(),
      )
      .unwrap();
    assert_eq!(rig.client.kept_co_hosts(rig.chat).unwrap(), vec![bob.user]);
    let older = rig.list(&alice, &alice, &[], 4, None);
    assert!(
      rig.admits(&alice.fingerprint(), &bob, &[&older]).await,
      "a rollback: the kept co-host counts"
    );

    let other = rig.list(&bob, &bob, &[], 7, None);
    assert!(rig.admits(&bob.fingerprint(), &bob, &[&other]).await);

    let kept = rig.client.kept_hosts(rig.chat).unwrap();
    assert_eq!(kept.by_anchor.len(), 2);
    assert_eq!(kept.by_anchor[&alice.fingerprint().to_string()].sequence, 5);
    assert_eq!(kept.maybe_latest, Some(bob.fingerprint().to_string()));
  }

  struct Rig {
    server: FakeServer,
    client: TestClient,
    chat: ConversationId,
    meeting: MeetingId,
    next_device: u64,
  }

  struct Person {
    user: UserId,
    identity: IdentityKeyPair,
    device: VerifiedDevice,
  }

  impl Person {
    fn fingerprint(&self) -> IdentityFingerprint {
      self.identity.public_key().fingerprint()
    }

    fn host(&self) -> Host {
      Host {
        user_id: convert::user(self.user),
        identity_key: self.identity.public_key(),
      }
    }
  }

  impl Rig {
    async fn new() -> Self {
      let server = FakeServer::new();
      let api = server.add_user("Guest");
      let organizer = server.add_user("Organizer");
      let chat = server.create_meeting_chat(organizer.user_id(), &[api.user_id()]);
      let meeting = server.meeting_of_chat(chat).unwrap();
      let options = crate::EnrollmentOptions {
        device_name: "test".into(),
        secret: crate::EnrollmentSecret::Passphrase(teamcraft_crypto::secret::SecretBytes::new(
          b"passphrase".to_vec(),
        )),
        argon2: teamcraft_crypto::backup::Argon2Settings {
          memory_kib: 64,
          iterations: 1,
          parallelism: 1,
        },
        maybe_attestation: None,
      };
      let client = crate::PendingEnrollment::new()
        .complete(
          api,
          Arc::new(MemoryStore::new()),
          Box::new(Arc::new(ManualClock::new(1_800_000_000_000))),
          ClientSettings::default(),
          options,
        )
        .await
        .unwrap();
      Self {
        server,
        client,
        chat,
        meeting,
        next_device: 1,
      }
    }

    fn person(&mut self, name: &str) -> Person {
      let user = self.server.add_user(name).user_id();
      let identity = IdentityKeyPair::generate(&mut OsRng);
      self.next_device += 1;
      let keys = DeviceKeyPair::generate(
        convert::user(user),
        SignedDeviceId::new(self.next_device),
        &mut OsRng,
      );
      let certificate = identity
        .certify_device(convert::user(user), &keys.public_keys())
        .unwrap();
      let device = certificate.verify(&identity.public_key()).unwrap();
      Person {
        user,
        identity,
        device,
      }
    }

    fn list(
      &self,
      signer: &Person,
      organizer: &Person,
      co_hosts: &[&Person],
      sequence: u64,
      maybe_handover: Option<&SignedMeetingHosts>,
    ) -> SignedMeetingHosts {
      HostsDraft {
        meeting_id: convert::meeting(self.meeting),
        conversation_id: convert::conversation(self.chat),
        sequence,
        signer_user_id: convert::user(signer.user),
        organizer: organizer.host(),
        co_hosts: co_hosts.iter().map(|person| person.host()).collect(),
        maybe_previous: maybe_handover.map(|handover| handover.hash().unwrap()),
      }
      .sign(&signer.identity)
      .unwrap()
    }

    /// Whether the guest's device takes a welcome from `sender`'s device by the hosts that follow
    /// from `link` while the server serves `lists`.
    async fn admits(
      &mut self,
      link: &IdentityFingerprint,
      sender: &Person,
      lists: &[&SignedMeetingHosts],
    ) -> bool {
      self.server.set_meeting_hosts(
        self.chat,
        lists.iter().map(|list| list.to_bytes().unwrap()).collect(),
      );
      self
        .client
        .guest_hosts_check(self.chat, Some(self.meeting), link, &sender.device)
        .await
        .unwrap()
        .0
    }
  }
}
