//! Full state sync over REST, key package top-up and key bag upload.

use std::collections::{BTreeSet, HashMap, HashSet};

use teamcraft_crypto::mls::{create_key_package, create_last_resort_key_package};
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, ConversationResponse, ListConversationMembersResponse,
  ListThreadsQuery, SpaceId, UploadKeyPackagesRequest, UserId, UserSummary,
};

use super::Client;
use super::catch_up::FirstPages;
use crate::api::{ApiError, ServerApi};
use crate::clock::{Clock, SystemClock};
use crate::error::CoreError;
use crate::store::{LocalStore, Table};
use crate::{read_state, records};

/// Conversations whose content an earlier sync skipped, retried by the upkeep and the next full
/// sync.
const NEEDS_RESYNC_KEY: &str = "needs_resync";
/// Conversations whose member list an earlier sync couldn't refresh; tracked apart from the
/// content, so a content sync that works doesn't clear a member refresh that failed.
const NEEDS_MEMBER_RESYNC_KEY: &str = "needs_member_resync";
/// Most people a full sync asks the server about because no list mentions them any more (issue
/// #654): one request each, so a sync after a long absence stays short; the rest follow next time.
const MAX_UNLISTED_RECHECKS: usize = 25;
/// When each unlisted person was last asked about, so a person who stays unlisted (only in a DM's
/// participants, say) is asked once a day, not at every sync.
const UNLISTED_RECHECKED_KEY: &str = "unlisted_rechecked";
const UNLISTED_RECHECK_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// The part of a conversation's sync that failed and waits for another try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResyncPart {
  Members,
  Content,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Brings the store up to date after a fresh gateway session (or at start): the last steps of an
  /// interrupted enrollment, spaces, conversations and members, welcomes, MLS backlogs and new
  /// messages (the conversation the user looks at first, several at once, issue #247), then the
  /// outbox, key packages and the key bag.
  pub async fn full_sync(&mut self) -> Result<(), CoreError> {
    let started_ms = self.now_ms();
    // Welcomes may have come in while this device was away: no spent key package goes before they
    // are read again (issue #395).
    self.are_welcomes_synced = false;
    // Removal records of the user's devices count before anything is decrypted, and this device's
    // own (also from older versions) reach the others.
    self.apply_shared_removals()?;
    self.share_removed_senders()?;
    self.finish_enrollment().await?;
    self.sync_workspace().await?;
    let listed = self.refresh_spaces_listing().await?;
    self.refresh_own_summary().await;
    if let Err(error) = self.recheck_unlisted_people(&listed).await {
      // Not fatal: names stay as they were until the next sync.
      tracing::warn!(%error, "could not recheck people no list mentions");
    }
    let listing = self.api.list_conversations().await?;
    // Direct messages deleted with a deleted account while this device was away: they go with
    // their local history, as on the gateway's `conversation_delete`.
    for deleted in &listing.deleted_conversation_ids {
      if self
        .store
        .get(Table::Conversations, &deleted.to_string())?
        .is_some()
      {
        self.purge_conversation(*deleted)?;
      }
    }
    let conversations = listing.conversations;
    let known: Vec<ConversationId> = self.cached_conversations()?.into_keys().collect();
    for stale in known
      .iter()
      .filter(|id| !conversations.iter().any(|conversation| conversation.id == **id))
    {
      self.store.delete(Table::Conversations, &stale.to_string())?;
      self.unmark_resync(*stale, ResyncPart::Members)?;
      self.unmark_resync(*stale, ResyncPart::Content)?;
    }
    // The whole list first, members after: a UI reading the store meanwhile shows every
    // conversation at once instead of one per member request (issue #247).
    for conversation in &conversations {
      records::put_json(
        &self.store,
        Table::Conversations,
        &conversation.id.to_string(),
        conversation,
      )?;
    }
    tracing::info!(
      conversations = conversations.len(),
      ms = self.now_ms().saturating_sub(started_ms),
      "full sync: conversation list stored"
    );
    let ids: Vec<ConversationId> = conversations.iter().map(|conversation| conversation.id).collect();
    self
      .catch_up_conversations(&ids, &members_order(&conversations))
      .await?;
    // With the queues read, the newest server time is known (issues #186, #391).
    self.prune_removed_senders()?;
    self.prune_readded_records()?;
    self.refresh_read_state().await?;
    self.refresh_notification_settings().await?;
    // A `device_add` missed while offline beyond the resume window would leave them waiting.
    self.recheck_waiting_additions().await?;
    // A revocation the revoking device couldn't finish while this one was away (#232). A list
    // that doesn't verify is the server's problem, not a reason to stop syncing.
    if let Err(error) = self.finish_listed_revocations().await {
      if error.is_transient() {
        return Err(error);
      }
      tracing::warn!(%error, "could not check the signed device list for unfinished revocations");
    }
    self.apply_pending_changes().await?;
    self.flush_outbox().await?;
    // Read positions, acknowledgements and pins taken offline (issue #159).
    self.flush_pending_actions().await?;
    self.top_up_key_packages().await?;
    // Also picks up saved items and drafts other devices changed while this one was away.
    self.sync_account_data().await
  }

  /// Catches one conversation up for a push that arrived while the sync loop wasn't running
  /// (notifications.md, "Mobile push delivery"): the three steps below in order. Only for an app
  /// that owns this client; a notification extension next to the app must not advance its state
  /// and uses [`crate::push_preview`] alone. An app with a time budget runs the steps itself and
  /// may cut only the cancel-safe ones short.
  pub async fn catch_up_for_push(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.push_refresh_conversation(conversation).await?;
    self.push_follow_groups(conversation).await?;
    self.push_catch_up_messages(conversation).await
  }

  /// Step 1, the conversation's record and members (for the names). Cancel-safe: every write
  /// follows the request it stores, so a future dropped at any await leaves nothing half done.
  pub async fn push_refresh_conversation(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.refresh_conversation(conversation).await
  }

  /// Step 2, welcomes (a conversation this device was just added to) and the conversation's MLS
  /// backlog. **Not cancel-safe:** opening a welcome spends this device's key package before the
  /// group is persisted, with requests in between (`join_welcome_group`); dropped there, the
  /// welcome could never be opened again and the device would silently miss the group. Run it to
  /// completion (its requests time out on their own) or not at all.
  pub async fn push_follow_groups(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.sync_welcomes().await?;
    if self.has_group(conversation)? {
      self.sync_mls(conversation).await?;
    }
    Ok(())
  }

  /// Step 3, new messages, parked ones decrypted again, unknown senders' keys. Cancel-safe: the
  /// message cursor is derived from the store, each message is stored by itself (a repeat is
  /// skipped), and keys are stored only after the lists that vouch for them were fetched.
  pub async fn push_catch_up_messages(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.catch_up_messages(conversation).await?;
    self.retry_undecrypted(conversation)?;
    self.learn_sender_keys(conversation).await
  }

  /// Retries the conversations an earlier sync had to skip (see [`Self::isolate`]); the upkeep
  /// calls this with the account sync. Returns how many caught up.
  pub async fn resync_marked_conversations(&mut self) -> Result<usize, CoreError> {
    let members = self.marked_for_resync(ResyncPart::Members)?;
    let content = self.marked_for_resync(ResyncPart::Content)?;
    let mut caught_up = 0;
    for conversation in members.union(&content).copied() {
      let mut is_caught_up = true;
      if members.contains(&conversation) {
        let result = self.refresh_members(conversation).await;
        let is_done = self.isolate(conversation, ResyncPart::Members, result)?;
        if is_done {
          self.unmark_resync(conversation, ResyncPart::Members)?;
        }
        is_caught_up &= is_done;
      }
      if content.contains(&conversation) {
        let result = self.sync_conversation_content(conversation).await;
        let is_done = self.isolate(conversation, ResyncPart::Content, result)?;
        if is_done {
          self.unmark_resync(conversation, ResyncPart::Content)?;
        }
        is_caught_up &= is_done;
      }
      caught_up += usize::from(is_caught_up);
    }
    Ok(caught_up)
  }

  /// The conversations an earlier sync skipped in part (members, content or both).
  pub fn conversations_to_resync(&self) -> Result<BTreeSet<ConversationId>, CoreError> {
    let mut marked = self.marked_for_resync(ResyncPart::Members)?;
    marked.extend(self.marked_for_resync(ResyncPart::Content)?);
    Ok(marked)
  }

  /// The conversations whose member list an earlier sync couldn't refresh.
  pub fn conversations_to_resync_members(&self) -> Result<BTreeSet<ConversationId>, CoreError> {
    self.marked_for_resync(ResyncPart::Members)
  }

  /// One conversation's part of the sync: its MLS backlog, new messages, messages waiting for keys
  /// and the senders' keys.
  async fn sync_conversation_content(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .catch_up_content_from(conversation, FirstPages::default())
      .await
  }

  /// Keeps one conversation's failure from failing the whole sync (issue #159): a 403 or 404 right
  /// after a removal, or a group in a bad state, marks the conversation for another try and the
  /// sync goes on with the others. Being offline, server trouble, an expired token or a broken
  /// store would fail every other conversation too: that stops the sync. Returns whether the
  /// conversation went through.
  pub(super) fn isolate(
    &self,
    conversation: ConversationId,
    part: ResyncPart,
    result: Result<(), CoreError>,
  ) -> Result<bool, CoreError> {
    match result {
      Ok(()) => Ok(true),
      Err(error) if error.is_retry_later() || matches!(error, CoreError::Store(_)) => Err(error),
      Err(error) => {
        tracing::warn!(%error, conversation_id = %conversation, ?part, "conversation skipped by the sync; retried later");
        let mut marked = self.marked_for_resync(part)?;
        if marked.insert(conversation) {
          records::put_json(&self.store, Table::SyncState, part.key(), &marked)?;
        }
        Ok(false)
      },
    }
  }

  pub(super) fn unmark_resync(
    &self,
    conversation: ConversationId,
    part: ResyncPart,
  ) -> Result<(), CoreError> {
    let mut marked = self.marked_for_resync(part)?;
    if marked.remove(&conversation) {
      records::put_json(&self.store, Table::SyncState, part.key(), &marked)?;
    }
    Ok(())
  }

  fn marked_for_resync(&self, part: ResyncPart) -> Result<BTreeSet<ConversationId>, CoreError> {
    Ok(records::get_json(&self.store, Table::SyncState, part.key())?.unwrap_or_default())
  }

  /// Followed threads and unread counts (after `thread_update` and on fresh sessions). The threads'
  /// last replies are decrypted into the message store like any message.
  pub async fn refresh_read_state(&mut self) -> Result<(), CoreError> {
    let query = ListThreadsQuery {
      maybe_limit: Some(self.settings.history_page_size),
      ..ListThreadsQuery::default()
    };
    let threads = self.api.list_threads(&query).await?.threads;
    for reply in threads
      .iter()
      .filter_map(|thread| thread.maybe_last_reply.clone())
    {
      self.receive_message(reply)?;
    }
    read_state::save_threads(&self.store, &threads)?;
    let unread = self.api.unread().await?;
    read_state::save_unread(&self.store, &unread)
  }

  /// Checks the key packages on the server once [`ClientSettings::key_package_check_interval`]
  /// has passed since the last check (the runner calls this on its ticks), so packages are renewed
  /// on a long-running device too, not only after a full sync. Returns whether it checked.
  ///
  /// The welcomes are read first: the tick may come right after the device woke from a long sleep,
  /// before the gateway noticed, and a welcome that came meanwhile may wait for a package the
  /// cleanup would delete (issue #395). One request per check. A welcome sync that fails doesn't
  /// stop the renewal (the device must stay addable, and the check must not repeat on every tick);
  /// it only leaves the cleanup for later. Re-add records past their cooldown go too, for a device
  /// that only ever resumes its gateway session (issue #391).
  ///
  /// [`ClientSettings::key_package_check_interval`]: super::ClientSettings::key_package_check_interval
  pub async fn renew_key_packages_if_due(&mut self) -> Result<bool, CoreError> {
    let interval_ms = i64::try_from(self.settings.key_package_check_interval.as_millis()).unwrap_or(i64::MAX);
    if self.now_ms().saturating_sub(self.last_key_package_check_ms) < interval_ms {
      return Ok(false);
    }
    if let Err(error) = self.sync_welcomes().await {
      tracing::warn!(%error, "could not read the welcomes before the key package check");
    }
    self.top_up_key_packages().await?;
    self.prune_readded_records()?;
    Ok(true)
  }

  /// Keeps enough key packages on the server so others can add this device (e2ee.md §4.3), plus a
  /// last-resort one for when they run out. The server counts packages that expire within its
  /// renewal window as "expiring soon", not as available, and an expiring last-resort package as
  /// missing, so this also replaces packages before they expire.
  pub async fn top_up_key_packages(&mut self) -> Result<(), CoreError> {
    let status = self.api.key_package_status(self.account.device_id).await?;
    self.last_key_package_check_ms = self.now_ms();
    let missing = if status.available < self.settings.key_package_low_water {
      self.settings.key_package_target.saturating_sub(status.available)
    } else {
      0
    };
    // openmls checks lifetimes against the system clock, so they are made from it too (not from
    // the injectable clock, which tests move freely), and so is the cleanup that compares them.
    let now_unix_secs = u64::try_from(SystemClock.now_ms().div_euclid(1000)).unwrap_or(0);
    if missing == 0 && status.has_last_resort {
      self.clean_up_key_packages(None, now_unix_secs);
      return Ok(());
    }
    let mut key_packages = Vec::new();
    for _ in 0..missing {
      key_packages.push(Base64Bytes::new(create_key_package(
        &self.provider,
        &self.account.device,
        &self.account.certificate,
        now_unix_secs,
      )?));
    }
    // A last-resort package close to expiry is reported as missing: it is rotated, the server
    // replaces the old one.
    let maybe_last_resort = if status.has_last_resort {
      None
    } else {
      Some(Base64Bytes::new(create_last_resort_key_package(
        &self.provider,
        &self.account.device,
        &self.account.certificate,
        now_unix_secs,
      )?))
    };
    // The private parts must be stored before the public parts become claimable.
    records::save_mls_storage(&self.store, &self.provider)?;
    let request = UploadKeyPackagesRequest {
      key_packages,
      maybe_last_resort,
    };
    self
      .api
      .upload_key_packages(self.account.device_id, &request)
      .await?;
    self.clean_up_key_packages(request.maybe_last_resort.as_ref(), now_unix_secs);
    tracing::info!(
      uploaded = missing,
      expiring_soon = status.expiring_soon,
      is_last_resort_rotated = !status.has_last_resort,
      "key packages renewed"
    );
    Ok(())
  }

  /// The user's own name and kind. Other people's summaries come with the member lists, but someone
  /// who is in no conversation yet would otherwise have no name in the app. Fetched at every full
  /// sync, so a name changed at the identity provider shows (issue #654).
  pub(crate) async fn refresh_own_summary(&mut self) {
    let me = self.user_id();
    let key = me.to_string();
    match self.api.user(me).await {
      Ok(summary) => {
        if let Err(error) = records::put_json(&self.store, Table::Users, &key, &summary) {
          tracing::debug!(%error, "could not store the own user summary");
        }
      },
      // Not fatal: the name arrives with the first member list.
      Err(error) => tracing::debug!(%error, "could not load the own user summary"),
    }
  }

  /// The user's spaces, and their members as the people the user can find and message (a new
  /// member shares no conversation with anyone yet).
  pub(crate) async fn refresh_spaces(&mut self) -> Result<(), CoreError> {
    self.refresh_spaces_listing().await.map(drop)
  }

  /// [`Self::refresh_spaces`], returning the people the space member lists named.
  async fn refresh_spaces_listing(&mut self) -> Result<HashSet<UserId>, CoreError> {
    let mut listed = HashSet::new();
    for space in self.api.list_spaces().await?.spaces {
      records::put_json(&self.store, Table::Spaces, &space.id.to_string(), &space)?;
      if space.maybe_my_role.is_some() {
        listed.extend(self.refresh_space_members(space.id).await?);
      }
    }
    Ok(listed)
  }

  async fn refresh_space_members(&mut self, space: SpaceId) -> Result<Vec<UserId>, CoreError> {
    match self.api.list_space_members(space).await {
      Ok(members) => members
        .members
        .iter()
        .map(|member| self.remember_user(&member.user).map(|()| member.user.id))
        .collect(),
      // Guests may not list a space's members; they find the people of their conversations.
      Err(error) if error.is_forbidden() || error.is_not_found() => Ok(Vec::new()),
      Err(error) => Err(error.into()),
    }
  }

  /// Fetches a person again, after a profile change or because no list names them any more: a new
  /// name replaces the cached one. One the server no longer shows (deactivated, or a guest's
  /// contact who left every shared conversation) stays cached for the names of old messages, but
  /// marked deactivated, so it is offered nowhere (issue #654).
  pub(crate) async fn refresh_user(&mut self, user: UserId) -> Result<(), CoreError> {
    match self.api.user(user).await {
      Ok(summary) => self.remember_user(&summary),
      Err(error) if error.is_not_found() => {
        let key = user.to_string();
        match records::get_json::<UserSummary>(&self.store, Table::Users, &key)? {
          Some(mut summary) if !summary.is_deactivated => {
            summary.is_deactivated = true;
            records::put_json(&self.store, Table::Users, &key, &summary)
          },
          _ => Ok(()),
        }
      },
      Err(error) => Err(error.into()),
    }
  }

  /// Cached people that neither a space member list (`listed`) nor a stored conversation member
  /// list names are asked about one by one, at most [`MAX_UNLISTED_RECHECKS`] per sync and each
  /// at most once per [`UNLISTED_RECHECK_INTERVAL_MS`]: a deactivated account drops out of every
  /// list, and without this its old name stayed in the people lists forever (issue #654). A
  /// deactivated account is still asked about: it may be deleted later and then shows as "Deleted
  /// user" (issue #630). Deleted ones are not asked again.
  async fn recheck_unlisted_people(&mut self, listed: &HashSet<UserId>) -> Result<(), CoreError> {
    let me = self.user_id();
    let now = self.now_ms();
    let mut rechecked: HashMap<UserId, i64> =
      records::get_json(&self.store, Table::SyncState, UNLISTED_RECHECKED_KEY)?.unwrap_or_default();
    let member_lists: Vec<ListConversationMembersResponse> = records::list_json(&self.store, Table::Members)?;
    let in_conversations: HashSet<UserId> = member_lists
      .iter()
      .flat_map(|list| list.members.iter().map(|member| member.user.id))
      .collect();
    let cached: Vec<UserSummary> = records::list_json(&self.store, Table::Users)?;
    let unlisted: Vec<UserId> = cached
      .iter()
      .filter(|user| !user.is_deleted && user.id != me)
      .map(|user| user.id)
      .filter(|user| !listed.contains(user) && !in_conversations.contains(user))
      .filter(|user| {
        rechecked
          .get(user)
          .is_none_or(|at| now.saturating_sub(*at) >= UNLISTED_RECHECK_INTERVAL_MS)
      })
      .take(MAX_UNLISTED_RECHECKS)
      .collect();
    // Only people still cached are remembered, so the record stays as small as the cache.
    rechecked.retain(|user, _| cached.iter().any(|known| known.id == *user));
    for user in unlisted {
      self.refresh_user(user).await?;
      rechecked.insert(user, now);
    }
    records::put_json(&self.store, Table::SyncState, UNLISTED_RECHECKED_KEY, &rechecked)
  }

  pub(crate) async fn refresh_conversation(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    match self.api.conversation(conversation).await {
      Ok(response) => {
        records::put_json(
          &self.store,
          Table::Conversations,
          &conversation.to_string(),
          &response,
        )?;
        self.refresh_members(conversation).await
      },
      Err(error) if error.is_not_found() => {
        self
          .store
          .delete(Table::Conversations, &conversation.to_string())?;
        Ok(())
      },
      Err(error) => Err(error.into()),
    }
  }

  pub(crate) async fn refresh_members(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let read = self.api.list_members(conversation).await;
    self.store_members(conversation, read)
  }

  /// Keeps a member list the server answered, and its people.
  pub(crate) fn store_members(
    &mut self,
    conversation: ConversationId,
    read: Result<ListConversationMembersResponse, ApiError>,
  ) -> Result<(), CoreError> {
    let members = match read {
      Ok(members) => members,
      // No longer a member (an event from before the removal arrived late): nothing to refresh.
      Err(error) if error.is_not_found() || error.is_forbidden() => return Ok(()),
      Err(error) => return Err(error.into()),
    };
    for member in &members.members {
      self.remember_user(&member.user)?;
    }
    records::put_json(&self.store, Table::Members, &conversation.to_string(), &members)
  }

  fn remember_user(&self, user: &UserSummary) -> Result<(), CoreError> {
    records::put_json(&self.store, Table::Users, &user.id.to_string(), user)
  }
}

impl ResyncPart {
  /// Where the conversations marked for this part are stored.
  fn key(self) -> &'static str {
    match self {
      Self::Members => NEEDS_MEMBER_RESYNC_KEY,
      Self::Content => NEEDS_RESYNC_KEY,
    }
  }
}

/// The order member lists are read in: direct and group messages first (they are named after
/// their members), then the rest in the server's order.
fn members_order(conversations: &[ConversationResponse]) -> Vec<ConversationId> {
  let is_direct = |conversation: &&ConversationResponse| {
    matches!(conversation.kind, ConversationKind::Dm | ConversationKind::Group)
  };
  conversations
    .iter()
    .filter(is_direct)
    .chain(
      conversations
        .iter()
        .filter(|conversation| !is_direct(conversation)),
    )
    .map(|conversation| conversation.id)
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationVisibility, HistoryPolicy, Id, Snowflake};

  use super::*;

  #[test]
  fn direct_conversations_get_their_members_first() {
    let conversations = [
      conversation(1, ConversationKind::Channel),
      conversation(2, ConversationKind::Dm),
      conversation(3, ConversationKind::Channel),
      conversation(4, ConversationKind::Group),
    ];
    let order: Vec<i64> = members_order(&conversations)
      .into_iter()
      .map(|id| id.snowflake().get())
      .collect();
    assert_eq!(order, vec![2, 4, 1, 3]);
  }

  fn conversation(raw: i64, kind: ConversationKind) -> ConversationResponse {
    ConversationResponse {
      id: ConversationId::from_snowflake(Snowflake::new(raw).unwrap()),
      maybe_space_id: None,
      kind,
      maybe_name: None,
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy: HistoryPolicy::Full,
      maybe_category_id: None,
      is_archived: false,
      moderation: Default::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: None,
    }
  }
}
