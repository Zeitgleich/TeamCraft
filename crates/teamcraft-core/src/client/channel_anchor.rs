//! The channel anchor on this device (e2ee.md §4.6.5): founding new channels anchored, checking
//! the anchor against the pin and the floor whenever the device enters a group state or a commit
//! changes it, and signing moves and renames.
//!
//! The pure decisions are `crate::anchoring`'s; this module reads the device's state for them and
//! records the outcome in the account data. Meeting chats are anchored too (D10, #1747): the
//! organizer's device founds with the meeting's ID, and a member checks the anchor against the
//! meeting and organizer the server names.

use teamcraft_crypto::channel_anchor::{
  AnchorKind, ChannelAnchor, ChannelProperties, DEFAULT_MAX_ANCHOR_BYTES, FloorError, Founding,
  NameAuthority, PropertiesChangeError, PropertiesTarget, VerifiedAnchor, name_hash,
};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::mls::{ConversationGroup, MlsError};
use teamcraft_proto::{ConversationId, ConversationKind, ConversationResponse, MeetingId, SpaceId, UserId};

use super::Client;
use crate::anchoring::{
  self, AnchorMarks, AnchorOutcome, AnchorVerdict, HeldName, MeetingCheck, MeetingExpectation, ReleaseVersion,
};
use crate::api::ServerApi;
use crate::channel_roles::RoleFounding;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::signed_deletion::AnchorFacts;
use crate::store::{LocalStore, Table};
use crate::trust::{PinnedDirectory, WorkspaceAuthority};

/// Where the highest `minimum_client_version` this device has seen is kept (D2: monotonic).
const MINIMUM_VERSION_KEY: &str = "highest_minimum_client_version";

/// That the pin or floor refused the group state this device was offered last (e2ee.md §4.6.5,
/// "Stripping and replacing"): it stays out, and the channel shows as unreachable.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AnchorRefusal {
  at_ms: i64,
  reason: String,
}

/// The channel's anchor as this device holds it now, for decisions that rest on it.
pub(crate) enum HeldAnchor {
  Unanchored,
  /// Pinned, but the anchor doesn't verify on this device right now.
  Unverifiable,
  Anchored {
    anchor: Box<VerifiedAnchor>,
    floor_grants: bool,
    /// The anchor this device's group holds is below the floor its user's devices recorded: a
    /// move or rename another device saw that this one hasn't. Deletions take the retention.
    is_below_floor: bool,
  },
}

/// A move or rename to sign after the server took it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PropertiesWish<'a> {
  /// `None`: the space stays as the anchor has it (a move always names a space).
  pub(crate) maybe_space: Option<SpaceId>,
  /// `None`: the name stays as the anchor has it (the listed name must match it).
  pub(crate) maybe_name: Option<&'a str>,
}

impl HeldAnchor {
  pub(crate) fn facts<'a>(&'a self, maybe_listed_name: Option<&'a str>) -> AnchorFacts<'a> {
    match self {
      Self::Unanchored => AnchorFacts::Unanchored,
      Self::Unverifiable => AnchorFacts::Unverifiable,
      Self::Anchored {
        anchor,
        floor_grants,
        is_below_floor,
      } => AnchorFacts::Anchored {
        anchor,
        floor_grants: *floor_grants && !*is_below_floor,
        maybe_listed_name,
      },
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Notes the server's `minimum_client_version`; only a higher one than seen before counts, so a
  /// server can't turn anchoring off again for this device (D2). The apps may also pass what they
  /// read from `GET /client-compatibility` or the server info.
  pub fn note_minimum_client_version(&mut self, version: &str) -> Result<(), CoreError> {
    let Some(seen) = ReleaseVersion::parse(version) else {
      tracing::debug!(version, "ignored a minimum client version that doesn't parse");
      return Ok(());
    };
    if self
      .highest_minimum_client_version()?
      .is_some_and(|highest| highest >= seen)
    {
      return Ok(());
    }
    records::put_json(&self.store, Table::SyncState, MINIMUM_VERSION_KEY, &seen)
  }

  pub(crate) fn highest_minimum_client_version(&self) -> Result<Option<ReleaseVersion>, CoreError> {
    records::get_json(&self.store, Table::SyncState, MINIMUM_VERSION_KEY)
  }

  /// Asks the server for its `minimum_client_version` and notes it. A failure keeps the highest
  /// one seen before, which is what decisions then rest on.
  pub(crate) async fn refresh_minimum_client_version(&mut self) {
    match self.api.client_compatibility().await {
      Ok(compatibility) => {
        if let Err(error) = self.note_minimum_client_version(&compatibility.minimum_client_version) {
          tracing::warn!(%error, "could not keep the minimum client version");
        }
      },
      Err(error) => tracing::debug!(%error, "deciding on the minimum client version seen before"),
    }
  }

  /// Whether a channel this device is about to create is founded anchored, and if so the version
  /// of its own signed device list the founding names. Asked **before** the server creates the
  /// channel: once D2 is reached, a channel is never founded unanchored silently. If this device's
  /// own list can't be found even after fetching it again, the create fails with
  /// [`CoreError::OwnDeviceListMissing`] and nothing is created.
  pub(crate) async fn prepare_anchored_founding(&mut self) -> Result<Option<u64>, CoreError> {
    self.refresh_minimum_client_version().await;
    if !anchoring::founds_anchored(self.highest_minimum_client_version()?) {
      return Ok(None);
    }
    let me = self.user_id();
    let identity = self.account.identity_public_key();
    let device = self.account.certificate.verify(&identity)?;
    if let Some(list) = self.stored_device_list(me, &identity)?
      && list.is_active(&device)
    {
      return Ok(Some(list.sequence()));
    }
    // Fetched again, twice: a list published a moment ago may not be read back at once.
    for _ in 0..2 {
      if let Some(list) = self.fetch_device_list(me, &identity).await?
        && list.is_active(&device)
      {
        return Ok(Some(list.sequence()));
      }
    }
    Err(CoreError::OwnDeviceListMissing)
  }

  /// Signs a new channel's anchor with what this device asked the server for (e2ee.md §4.6.5,
  /// "Founding"), naming its own device list at `device_list_version`.
  pub(crate) fn sign_founding(
    &self,
    conversation: ConversationId,
    space: SpaceId,
    name: &str,
    device_list_version: u64,
  ) -> Result<ChannelAnchor, CoreError> {
    self.sign_anchor_founding(&Founding {
      conversation_id: convert::conversation(conversation),
      kind: AnchorKind::Channel,
      device_list_version,
      signed_at_ms: u64::try_from(self.now_ms()).unwrap_or(0),
      maybe_space_id: Some(convert::space(space)),
      name,
    })
  }

  /// Signs the anchor of a meeting chat this organizer's device founds (D10, #1747): kind
  /// `meeting_chat` with `meeting`, as the app handed it over, outside any space and without a name
  /// (a meeting chat is never renamed through its anchor, and no deletion purges by its name).
  pub(crate) fn sign_meeting_founding(
    &self,
    conversation: ConversationId,
    meeting: MeetingId,
    device_list_version: u64,
  ) -> Result<ChannelAnchor, CoreError> {
    self.sign_anchor_founding(&Founding {
      conversation_id: convert::conversation(conversation),
      kind: AnchorKind::MeetingChat {
        meeting_id: convert::meeting(meeting),
      },
      device_list_version,
      signed_at_ms: u64::try_from(self.now_ms()).unwrap_or(0),
      maybe_space_id: None,
      name: "",
    })
  }

  fn sign_anchor_founding(&self, founding: &Founding<'_>) -> Result<ChannelAnchor, CoreError> {
    ChannelAnchor::found(&self.account.device, &self.account.certificate, founding)
      .map_err(|error| CoreError::Mls(MlsError::from(error)))
  }

  /// Check 6 for a group state this device enters (a welcome, an add-back, an external commit, its
  /// own founding): records the pin and floor it accepts. `Err` with the reason if refused; the
  /// caller then drops the group (deletes it before anything is sent) and stays out.
  pub(crate) async fn admit_anchor_entering(
    &mut self,
    conversation: ConversationId,
    group: &ConversationGroup,
  ) -> Result<Result<(), FloorError>, CoreError> {
    let maybe_floor = self.account_data().anchor_floor(conversation);
    if maybe_floor.is_none() && group.anchor_bytes().is_none() {
      return Ok(Ok(()));
    }
    let tree_users = users_of(group)?;
    let directory = self.anchor_directory(group.anchor_bytes(), tree_users).await?;
    let maybe_meeting = match self
      .meeting_expectation_entering(conversation, group, &directory)
      .await?
    {
      MeetingCheck::NotAMeeting => None,
      MeetingCheck::Expect(expected) => Some(expected),
      MeetingCheck::Unknown => {
        tracing::warn!(conversation_id = %conversation, "the server names no meeting or organizer for the meeting chat; its anchor grants nothing");
        let verdict =
          anchoring::judge_unknown_meeting(maybe_floor.as_ref(), group.anchor_bytes(), group.epoch());
        return self.apply_anchor_verdict(conversation, verdict);
      },
    };
    let Some(expected) = anchoring::anchor_expectation(
      convert::conversation(conversation),
      maybe_floor.is_some(),
      group.anchor_bytes().is_some(),
      maybe_meeting,
    ) else {
      return Ok(Ok(()));
    };
    let authority = self.anchor_authority()?;
    let result = group.verified_anchor(&expected, &directory, &authority);
    if anchoring::refuses_meeting_anchor(&expected, &result) {
      return self.apply_anchor_verdict(conversation, AnchorVerdict::Refuse(FloorError::NotThisMeeting));
    }
    let outcome = AnchorOutcome::of(result, group.anchor_bytes());
    let maybe_meeting_founder = maybe_meeting
      .and(outcome.verified())
      .and_then(VerifiedAnchor::founder)
      .map(|(_, key)| key.fingerprint());
    let verdict = anchoring::judge_entering(maybe_floor.as_ref(), &outcome, group.epoch());
    let admitted = self.apply_anchor_verdict(conversation, verdict)?;
    match &admitted {
      Ok(()) => {
        self.note_held_name(conversation, outcome.verified())?;
        self
          .store
          .delete(Table::AppliedChanges, &anchor_refused_key(conversation))?;
      },
      Err(refusal) => records::put_json(
        &self.store,
        Table::AppliedChanges,
        &anchor_refused_key(conversation),
        &AnchorRefusal {
          at_ms: self.now_ms(),
          reason: refusal.to_string(),
        },
      )?,
    }
    // The meeting's host lists count only from the founder's key now (#1747).
    if admitted.is_ok()
      && let Some(founder) = maybe_meeting_founder
    {
      self.note_pinned_meeting_founder(conversation, &founder)?;
    }
    Ok(admitted)
  }

  /// The founder of the meeting chat's anchor this device pinned (D10): who organized the meeting
  /// when its group was founded, whatever the server names now. `None` without a pin or a meeting
  /// chat's anchor.
  pub(crate) fn pinned_meeting_founder(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Option<UserId>, CoreError> {
    if self.account_data().anchor_floor(conversation).is_none() {
      return Ok(None);
    }
    let Some(group) = self.group(conversation)? else {
      return Ok(None);
    };
    group
      .anchor_bytes()
      .and_then(anchoring::meeting_named_by)
      .map(|named| convert::user_from_crypto(named.organizer))
      .transpose()
  }

  /// What a meeting chat's anchor must name when this device enters its group (D10, #1747): the
  /// meeting and the organizer the server names for the chat. After a handover (#1720) the group
  /// keeps the anchor its first organizer signed: then the founder counts if the signed host lists
  /// hand the meeting from the founder's key (as its leaf verified) to the organizer named now.
  /// Any other conversation's anchor must be a channel's. The anchor never vouches for itself: a
  /// server that names no organizer, or no meeting while the anchor names one, gets
  /// [`MeetingCheck::Unknown`] (nothing granted or pinned); a server too old to name the meeting
  /// has the anchor's meeting held to the organizer it names.
  async fn meeting_expectation_entering(
    &mut self,
    conversation: ConversationId,
    group: &ConversationGroup,
    directory: &PinnedDirectory,
  ) -> Result<MeetingCheck, CoreError> {
    let maybe_named = group.anchor_bytes().and_then(anchoring::meeting_named_by);
    let is_listed_meeting = self
      .cached_conversations()?
      .get(&conversation)
      .is_some_and(|response| response.kind == ConversationKind::Meeting);
    if maybe_named.is_none() && !is_listed_meeting {
      return Ok(MeetingCheck::NotAMeeting);
    }
    let response = self.api.conversation(conversation).await?;
    if response.kind != ConversationKind::Meeting {
      return Ok(MeetingCheck::NotAMeeting);
    }
    let Some(organizer) = response.maybe_organizer_user_id else {
      return Ok(MeetingCheck::Unknown);
    };
    let meeting_id = match (response.maybe_meeting_id, maybe_named) {
      (Some(meeting), _) => convert::meeting(meeting),
      (None, Some(named)) => named.meeting_id,
      (None, None) => return Ok(MeetingCheck::Unknown),
    };
    let mut expected = MeetingExpectation {
      meeting_id,
      organizer: convert::user(organizer),
    };
    if let Some(named) = maybe_named
      && named.organizer != expected.organizer
      && let Some(founder) = group.members()?.into_iter().next()
      && founder.user_id == named.organizer
      && self
        .is_handed_over_group(conversation, group, founder, directory, organizer)
        .await?
    {
      expected.organizer = named.organizer;
    }
    Ok(MeetingCheck::Expect(expected))
  }

  /// Pins the anchor of a channel this device just founded (its own founding is entered like any
  /// group state). A failure only leaves the channel unpinned here; the members pin it.
  pub(crate) async fn pin_own_founding(&mut self, conversation: ConversationId) {
    let Some(group) = self.groups.remove(&conversation) else {
      return;
    };
    let admitted = Box::pin(self.admit_anchor_entering(conversation, &group)).await;
    self.groups.insert(conversation, group);
    match admitted {
      Ok(Ok(())) => {},
      Ok(Err(refusal)) => {
        tracing::warn!(conversation_id = %conversation, %refusal, "this device's own founding doesn't match the pin");
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not pin the new channel's anchor");
      },
    }
  }

  /// Check 6 at a commit inside a group state this device holds that changed the anchor (a move or
  /// rename, its own included): the pinned founding, and the floor only rises. `directory` is the
  /// one the commit was verified with.
  pub(crate) fn note_anchor_inside(
    &mut self,
    conversation: ConversationId,
    directory: &PinnedDirectory,
  ) -> Result<(), CoreError> {
    let maybe_floor = self.account_data().anchor_floor(conversation);
    let authority = self.anchor_authority()?;
    let group = self.group_or_error(conversation)?;
    // The founding can't change inside a group state, and was checked against the server's meeting
    // and organizer when this device entered it: a meeting chat's anchor names its own.
    let Some(expected) = anchoring::anchor_expectation(
      convert::conversation(conversation),
      maybe_floor.is_some(),
      group.anchor_bytes().is_some(),
      group.anchor_bytes().and_then(anchoring::meeting_named_by),
    ) else {
      return Ok(());
    };
    let outcome = AnchorOutcome::of(
      group.verified_anchor(&expected, directory, &authority),
      group.anchor_bytes(),
    );
    let epoch = group.epoch();
    let verdict = anchoring::judge_inside(maybe_floor.as_ref(), &outcome, epoch);
    self.note_held_name(conversation, outcome.verified())?;
    if let Err(error) = self.apply_anchor_verdict(conversation, verdict)? {
      // The commit is merged already, and the crypto layer refuses any commit that touches the
      // founding: another device of the user pinned another founding. Granting nothing is all
      // that is left; the floor shows the fork.
      tracing::warn!(conversation_id = %conversation, %error, "the channel's anchor differs from the one the user's devices pinned");
    }
    Ok(())
  }

  /// The channel's anchor as this device holds it, verified now (for a deletion, D3).
  pub(crate) async fn held_anchor(&mut self, conversation: ConversationId) -> Result<HeldAnchor, CoreError> {
    let maybe_floor = self.account_data().anchor_floor(conversation);
    let Some(group) = self.group(conversation)? else {
      return Ok(HeldAnchor::Unanchored);
    };
    let maybe_bytes = group.anchor_bytes().map(<[u8]>::to_vec);
    // As inside a group state: checked against the server's word when this device entered it.
    let Some(expected) = anchoring::anchor_expectation(
      convert::conversation(conversation),
      maybe_floor.is_some(),
      maybe_bytes.is_some(),
      maybe_bytes.as_deref().and_then(anchoring::meeting_named_by),
    ) else {
      return Ok(HeldAnchor::Unanchored);
    };
    let tree_users = users_of(self.group_or_error(conversation)?)?;
    let directory = self.anchor_directory(maybe_bytes.as_deref(), tree_users).await?;
    let authority = self.anchor_authority()?;
    let group = self.group_or_error(conversation)?;
    let epoch = group.epoch();
    let outcome = AnchorOutcome::of(
      group.verified_anchor(&expected, &directory, &authority),
      maybe_bytes.as_deref(),
    );
    self.note_held_name(conversation, outcome.verified())?;
    Ok(match (outcome, maybe_floor) {
      (AnchorOutcome::Verified(anchor), maybe_floor) => {
        // A floor of another founding is a fork: the anchor grants nothing.
        let floor_grants =
          maybe_floor.is_none_or(|floor| floor.grants() && floor.founding_hash == anchor.founding_hash());
        // The floor as on entering a group state: a lower properties version, the same under
        // another hash, or an older epoch than the user's devices saw (#1717).
        let is_below_floor = maybe_floor.is_some_and(|floor| floor.check_entering(&anchor, epoch).is_err());
        HeldAnchor::Anchored {
          anchor,
          floor_grants,
          is_below_floor,
        }
      },
      (_, Some(_)) => HeldAnchor::Unverifiable,
      (_, None) => HeldAnchor::Unanchored,
    })
  }

  /// Whether the channel is anchored on this device: its group carries an anchor, or the user's
  /// devices pinned it. Nobody founds such a channel's roles at first sight (e2ee.md §4.6.5); an
  /// anchor that doesn't verify counts here too, which only ever withholds a signature.
  pub(crate) fn is_anchored_channel(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    if self.account_data().anchor_floor(conversation).is_some() {
      return Ok(true);
    }
    Ok(
      self
        .group(conversation)?
        .is_some_and(|group| group.anchor_bytes().is_some()),
    )
  }

  /// Who may found the channel's roles (e2ee.md §4.6.5, "What the founding decides"): the verified
  /// anchor's founder while the floor grants; nobody for a channel whose group carries an anchor or
  /// which is pinned, when the anchor doesn't verify here or shows a fork (a server that refuses the
  /// founder's device list mustn't turn the channel back to trust on first use); the first entry's
  /// claim, as before anchors, only for a channel with neither anchor nor pin.
  pub(crate) async fn role_founding(
    &mut self,
    conversation: ConversationId,
  ) -> Result<RoleFounding, CoreError> {
    let is_anchored = self.is_anchored_channel(conversation)?;
    Ok(match Box::pin(self.held_anchor(conversation)).await? {
      HeldAnchor::Anchored {
        anchor, floor_grants, ..
      } => anchoring::role_founding(is_anchored, Some(&anchor), floor_grants),
      HeldAnchor::Unverifiable | HeldAnchor::Unanchored => anchoring::role_founding(is_anchored, None, false),
    })
  }

  /// Whether the anchor the group carries claims `user` as its founder, unverified: only a cheap
  /// filter for whose upkeep tries the founding, which [`Self::role_founding`] then decides.
  pub(crate) fn anchor_claims_founder(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<bool, CoreError> {
    let Some(group) = self.group(conversation)? else {
      return Ok(false);
    };
    Ok(anchor_signers(group.anchor_bytes()).first() == Some(&user))
  }

  /// Signs and commits a move or rename the server took (e2ee.md §4.6.5, D4, D5), so members' apps
  /// hold it in the anchor: as a workspace signer if this user is an admin of a fresh roster, else
  /// a rename as a member. A move by anybody else, or a change this device can't sign, leaves the
  /// property stale (the server and the anchor differ), which only costs a deletion's speed.
  pub(crate) async fn sign_channel_properties(
    &mut self,
    conversation: ConversationId,
    wish: PropertiesWish<'_>,
  ) -> Result<(), CoreError> {
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      if !self.has_group(conversation)? || self.group_or_error(conversation)?.anchor_bytes().is_none() {
        return Ok(());
      }
      self.resolve_pending_commit(conversation).await?;
      let HeldAnchor::Anchored { anchor, .. } = Box::pin(self.held_anchor(conversation)).await? else {
        tracing::info!(conversation_id = %conversation, "the channel's anchor doesn't verify here; the change stays the server's word");
        return Ok(());
      };
      let Some(listed_name) = self.listed_name(conversation)? else {
        return Ok(());
      };
      let current = *anchor.properties();
      let Some(target) = target_of(&current, wish, &listed_name) else {
        tracing::info!(conversation_id = %conversation, "the listed name isn't the anchor's; a move leaves the anchor as it is");
        return Ok(());
      };
      let new_hash = name_hash(target.name);
      if target.maybe_space_id == current.maybe_space_id && new_hash == current.name_hash {
        return Ok(());
      }
      let is_move = target.maybe_space_id != current.maybe_space_id;
      let (class, roster_version) = self.properties_signer_class()?;
      if is_move && class != NameAuthority::Workspace {
        tracing::info!(conversation_id = %conversation, "only a workspace signer moves an anchored channel; the space stays stale");
        return Ok(());
      }
      let tree_users = users_of(self.group_or_error(conversation)?)?;
      let maybe_bytes = self
        .group_or_error(conversation)?
        .anchor_bytes()
        .map(<[u8]>::to_vec);
      let directory = self.anchor_directory(maybe_bytes.as_deref(), tree_users).await?;
      let authority = self.group_authority(conversation, self.now_ms())?;
      let epoch = self.group_or_error(conversation)?.epoch();
      let signed = anchor
        .anchor()
        .sign_change(
          &self.account.device,
          &self.account.certificate,
          class,
          &target,
          epoch,
          roster_version,
        )
        .map_err(MlsError::from)?;
      let group = self
        .groups
        .get_mut(&conversation)
        .ok_or(CoreError::NoGroup(conversation))?;
      let output = match group.commit_properties_change(
        &self.provider,
        &self.account.device,
        &signed,
        &directory,
        &authority,
      ) {
        Ok(output) => output,
        Err(MlsError::PropertiesChange(error @ PropertiesChangeError::NotAllowed(_))) => {
          tracing::info!(conversation_id = %conversation, %error, "the members wouldn't take this change; it stays the server's word");
          return Ok(());
        },
        Err(error) => return Err(error.into()),
      };
      if self.post_commit(conversation, output, &[]).await?.is_some() {
        // The committer raises its floor like every other member.
        return self.note_anchor_inside(conversation, &directory);
      }
      self.sync_mls(conversation).await?;
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "other commits kept winning the epoch".into(),
    })
  }

  /// Reads the channel's anchor again for the marks the apps show (the channel or its settings
  /// opened): what [`channel_anchor_marks`] reads. Only the UI adapter asks for it.
  #[cfg(feature = "ui")]
  pub(crate) async fn refresh_channel_anchor(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    if !self.has_group(conversation)? {
      return Ok(());
    }
    Box::pin(self.held_anchor(conversation)).await.map(|_| ())
  }

  /// Keeps what this device verified of the anchor's name, or forgets it when nothing verifies:
  /// a name is shown as stale only against a verified anchor.
  fn note_held_name(
    &self,
    conversation: ConversationId,
    maybe_verified: Option<&VerifiedAnchor>,
  ) -> Result<(), CoreError> {
    let key = held_name_key(conversation);
    match maybe_verified.and_then(HeldName::of) {
      Some(held) => {
        let maybe_known: Option<HeldName> = records::get_json(&self.store, Table::AppliedChanges, &key)?;
        if maybe_known != Some(held) {
          records::put_json(&self.store, Table::AppliedChanges, &key, &held)?;
        }
        Ok(())
      },
      None => Ok(self.store.delete(Table::AppliedChanges, &key)?),
    }
  }

  fn listed_name(&self, conversation: ConversationId) -> Result<Option<String>, CoreError> {
    let maybe_known: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    Ok(maybe_known.and_then(|known| known.maybe_name))
  }

  /// The directory an anchor is verified with: the group's users, plus the founder and the last
  /// change's signer with their newest device lists fetched (check 3; a list never fetched grants
  /// nothing). Only those two lists are read, never every member's.
  async fn anchor_directory(
    &mut self,
    maybe_bytes: Option<&[u8]>,
    mut users: Vec<UserId>,
  ) -> Result<PinnedDirectory, CoreError> {
    let signers = anchor_signers(maybe_bytes);
    users.extend(signers.iter().copied());
    let directory = self.pin_identities(users.iter().copied()).await?;
    for signer in signers {
      let Some(identity) = directory.pinned_identity(convert::user(signer)) else {
        continue;
      };
      if let Err(error) = self.fetch_device_list(signer, &identity).await {
        if error.is_transient() {
          return Err(error);
        }
        tracing::warn!(user_id = %signer, %error, "refused a device list of an anchor's signer");
      }
    }
    self.pin_identities(users).await
  }

  /// Who is a workspace signer of an anchor: the admins of the fresh verified roster (check 4, D4).
  fn anchor_authority(&self) -> Result<WorkspaceAuthority, CoreError> {
    let workspace = self.workspace()?;
    let (admins, roster) = self.fresh_admins(&workspace, self.now_ms());
    Ok(
      WorkspaceAuthority::new(
        std::iter::empty(),
        admins,
        workspace.policy.admin_self_join.clone(),
      )
      .with_roster(roster),
    )
  }

  /// This user's role for a move or rename: a workspace signer with the roster's version, else a
  /// member.
  fn properties_signer_class(&self) -> Result<(NameAuthority, u64), CoreError> {
    let workspace = self.workspace()?;
    let (admins, roster) = self.fresh_admins(&workspace, self.now_ms());
    Ok(if admins.contains(&self.user_id()) {
      (NameAuthority::Workspace, roster.version)
    } else {
      (NameAuthority::Member, 0)
    })
  }

  /// Records an accepted pin and floor; returns the reason of a refusal.
  fn apply_anchor_verdict(
    &mut self,
    conversation: ConversationId,
    verdict: AnchorVerdict,
  ) -> Result<Result<(), FloorError>, CoreError> {
    match verdict {
      AnchorVerdict::Unanchored | AnchorVerdict::KeepPin => Ok(Ok(())),
      AnchorVerdict::Record(floor) => {
        self.update_account_data(|data, _| data.note_anchor_floor(conversation, &floor))?;
        Ok(Ok(()))
      },
      AnchorVerdict::Refuse(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "refused a group state against the channel's anchor pin; staying out");
        Ok(Err(error))
      },
    }
  }
}

/// What the apps show of a channel's anchor on this device (e2ee.md §4.6.5): a listed name the
/// anchor doesn't hold, who signed the anchor's name, and a fork the user's devices saw. `None` for
/// a channel that isn't anchored here.
pub fn channel_anchor_marks(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<AnchorMarks>, CoreError> {
  let maybe_held: Option<HeldName> =
    records::get_json(store, Table::AppliedChanges, &held_name_key(conversation))?;
  let maybe_floor = super::stored_account_data(store)?.anchor_floor(conversation);
  let maybe_known: Option<ConversationResponse> =
    records::get_json(store, Table::Conversations, &conversation.to_string())?;
  let maybe_listed_name = maybe_known.as_ref().and_then(|known| known.maybe_name.as_deref());
  Ok(anchoring::anchor_marks(
    maybe_held.as_ref(),
    maybe_floor.as_ref(),
    maybe_listed_name,
  ))
}

/// Whether the channel's pin refused the group state this device was offered last: it stays out
/// and the channel is unreachable here, rather than waiting silently.
pub fn is_anchor_refused(store: &impl LocalStore, conversation: ConversationId) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::AppliedChanges, &anchor_refused_key(conversation))?
      .is_some(),
  )
}

pub(crate) fn held_name_key(conversation: ConversationId) -> String {
  format!("anchor_held_name/{conversation}")
}

pub(crate) fn anchor_refused_key(conversation: ConversationId) -> String {
  format!("anchor_refused/{conversation}")
}

/// The users with a leaf in the group.
fn users_of(group: &ConversationGroup) -> Result<Vec<UserId>, CoreError> {
  group
    .members()?
    .into_iter()
    .map(|member| convert::user_from_crypto(member.user_id))
    .collect()
}

/// The founder (or reviewer) and the last change's claimed signer of an anchor, unverified: whose
/// device lists to fetch before verifying it.
fn anchor_signers(maybe_bytes: Option<&[u8]>) -> Vec<UserId> {
  let Some(anchor) =
    maybe_bytes.and_then(|bytes| ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES).ok())
  else {
    return Vec::new();
  };
  let founder = anchor.founding().origin.signer().0;
  let maybe_changer = anchor
    .last_change()
    .and_then(|change| change.claimed_signer().ok())
    .filter(|changer| *changer != founder);
  [Some(founder), maybe_changer]
    .into_iter()
    .flatten()
    .filter_map(|user| convert::user_from_crypto(user).ok())
    .collect()
}

/// The full new properties of a wish: what it names, the rest as the anchor has it. `None` if the
/// name stays but the listed name isn't the anchor's (signing it would rename the channel).
fn target_of<'a>(
  current: &ChannelProperties,
  wish: PropertiesWish<'a>,
  listed_name: &'a str,
) -> Option<PropertiesTarget<'a>> {
  let maybe_space_id = match wish.maybe_space {
    Some(space) => Some(convert::space(space)),
    None => current.maybe_space_id,
  };
  let name = match wish.maybe_name {
    Some(name) => name,
    None if name_hash(listed_name) == current.name_hash => listed_name,
    None => return None,
  };
  Some(PropertiesTarget { maybe_space_id, name })
}
