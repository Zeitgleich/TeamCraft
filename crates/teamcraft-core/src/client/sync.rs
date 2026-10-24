//! Full state sync over REST, key package top-up and key bag upload.

use teamcraft_crypto::mls::{create_key_package, create_last_resort_key_package};
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, ConversationResponse, ListThreadsQuery, SpaceId,
  UploadKeyPackagesRequest, UserSummary,
};

use super::Client;
use crate::api::ServerApi;
use crate::clock::{Clock, SystemClock};
use crate::error::CoreError;
use crate::store::{LocalStore, Table};
use crate::{read_state, records};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Brings the store up to date after a fresh gateway session (or at start): the last steps of an
  /// interrupted enrollment, spaces, conversations and members, welcomes, MLS backlogs, new
  /// messages, then the outbox, key packages and the key bag.
  pub async fn full_sync(&mut self) -> Result<(), CoreError> {
    // Removal records of the user's devices count before anything is decrypted, and this device's
    // own (also from older versions) reach the others.
    self.apply_shared_removals()?;
    self.share_removed_senders()?;
    self.finish_enrollment().await?;
    self.sync_workspace().await?;
    self.refresh_spaces().await?;
    self.refresh_own_summary().await;
    let conversations = self.api.list_conversations().await?.conversations;
    let known: Vec<ConversationId> = self.cached_conversations()?.into_keys().collect();
    for stale in known
      .iter()
      .filter(|id| !conversations.iter().any(|conversation| conversation.id == **id))
    {
      self.store.delete(Table::Conversations, &stale.to_string())?;
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
    // Direct and group messages are named after their members: theirs first, so their names
    // follow right after the list.
    for conversation in members_order(&conversations) {
      self.refresh_members(conversation).await?;
    }
    self.sync_welcomes().await?;
    // After the welcomes: a member's add (with a Welcome) wins over claiming the place ourselves.
    self.join_missing_groups().await?;
    for conversation in &conversations {
      if self.has_group(conversation.id)? {
        self.sync_mls(conversation.id).await?;
      }
      self.catch_up_messages(conversation.id).await?;
      self.retry_undecrypted(conversation.id)?;
      // Senders this device has no verified key for yet (it isn't in the group, or just joined).
      self.learn_sender_keys(conversation.id).await?;
    }
    self.refresh_read_state().await?;
    self.refresh_notification_settings().await?;
    // A `device_add` missed while offline beyond the resume window would leave them waiting.
    self.recheck_waiting_additions().await?;
    self.apply_pending_changes().await?;
    self.flush_outbox().await?;
    self.top_up_key_packages().await?;
    // Also picks up saved items and drafts other devices changed while this one was away.
    self.sync_account_data().await
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
  /// [`ClientSettings::key_package_check_interval`]: super::ClientSettings::key_package_check_interval
  pub async fn renew_key_packages_if_due(&mut self) -> Result<bool, CoreError> {
    let interval_ms = i64::try_from(self.settings.key_package_check_interval.as_millis()).unwrap_or(i64::MAX);
    if self.now_ms().saturating_sub(self.last_key_package_check_ms) < interval_ms {
      return Ok(false);
    }
    self.top_up_key_packages().await?;
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
    if missing == 0 && status.has_last_resort {
      return Ok(());
    }
    // openmls checks lifetimes against the system clock, so they are made from it too (not from
    // the injectable clock, which tests move freely).
    let now_unix_secs = u64::try_from(SystemClock.now_ms().div_euclid(1000)).unwrap_or(0);
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
    self
      .api
      .upload_key_packages(
        self.account.device_id,
        &UploadKeyPackagesRequest {
          key_packages,
          maybe_last_resort,
        },
      )
      .await?;
    tracing::info!(
      uploaded = missing,
      expiring_soon = status.expiring_soon,
      is_last_resort_rotated = !status.has_last_resort,
      "key packages renewed"
    );
    Ok(())
  }

  /// The user's own name and kind. Other people's summaries come with the member lists, but someone
  /// who is in no conversation yet would otherwise have no name in the app.
  pub(crate) async fn refresh_own_summary(&mut self) {
    let me = self.user_id();
    let key = me.to_string();
    match records::get_json::<UserSummary>(&self.store, Table::Users, &key) {
      Ok(Some(_)) => return,
      Ok(None) => {},
      Err(error) => {
        tracing::debug!(%error, "could not read the own user summary");
        return;
      },
    }
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
    for space in self.api.list_spaces().await?.spaces {
      records::put_json(&self.store, Table::Spaces, &space.id.to_string(), &space)?;
      if space.maybe_my_role.is_some() {
        self.refresh_space_members(space.id).await?;
      }
    }
    Ok(())
  }

  async fn refresh_space_members(&mut self, space: SpaceId) -> Result<(), CoreError> {
    match self.api.list_space_members(space).await {
      Ok(members) => members
        .members
        .iter()
        .try_for_each(|member| self.remember_user(&member.user)),
      // Guests may not list a space's members; they find the people of their conversations.
      Err(error) if error.is_forbidden() || error.is_not_found() => Ok(()),
      Err(error) => Err(error.into()),
    }
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

  async fn refresh_members(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let members = match self.api.list_members(conversation).await {
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
      maybe_my_membership: None,
      participants: Vec::new(),
    }
  }
}
