//! Integration-bridge sources as a member device of the channel sees them (docs/bridge.md,
//! "Sources"): the sources in force, judged against this device's view of the channel's
//! encryption group, and the one-time look back through history for announcements older than the
//! store (a bridge whose database was lost).

use std::collections::HashSet;

use teamcraft_proto::{ConversationId, SourceChange};

use super::Client;
use super::messaging::apply_control;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::integration_sources::{ActiveSource, SourceAuthority, SourceLedger};
use crate::records;
use crate::store::{LocalStore, MessageContent, Table};

/// The sources of one conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntegrationSources {
  /// [`SourceLedger::revision`]: changes when a source change arrived.
  pub revision: u64,
  pub active: Vec<ActiveSource>,
}

/// Where the look back through a conversation's history stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryScan {
  /// Done before, or reached the beginning (or the history this device may see) now.
  Done,
  /// More pages are left.
  More,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The conversations whose encryption group this device is in.
  pub fn group_conversations(&mut self) -> Result<Vec<ConversationId>, CoreError> {
    let mut ids = Vec::new();
    for conversation in self.cached_conversations()?.into_keys() {
      if self.has_group(conversation)? {
        ids.push(conversation);
      }
    }
    Ok(ids)
  }

  /// The conversation's sources in force (e2ee.md §13.1): created by a current, non-guest member
  /// of its encryption group who is a verified workspace admin or a channel admin, and not revoked.
  /// Reads the store only; [`Self::refresh_source_authority`] brings the member roles up to date.
  pub fn integration_sources(
    &mut self,
    conversation: ConversationId,
  ) -> Result<IntegrationSources, CoreError> {
    let ledger = SourceLedger::load(&self.store, conversation)?;
    if ledger.revision == 0 || !self.has_group(conversation)? {
      return Ok(IntegrationSources {
        revision: ledger.revision,
        active: Vec::new(),
      });
    }
    let group_users: HashSet<_> = self.group_users(conversation)?.into_iter().collect();
    let guests = self.guest_users(group_users.iter().copied())?;
    let workspace = self.workspace()?;
    let max_age_ms = i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    let is_fresh = workspace.is_roster_fresh(self.now_ms(), max_age_ms);
    let authority = SourceAuthority::from_store(
      &self.store,
      conversation,
      &workspace,
      is_fresh,
      Some(group_users),
      guests,
    )?;
    Ok(IntegrationSources {
      revision: ledger.revision,
      active: ledger.active(conversation, &authority),
    })
  }

  /// Announces a change to the conversation's sources: an encrypted, hidden message for the
  /// members (the bridge among them), applied here at once. Works offline like any message.
  pub async fn send_source_change(
    &mut self,
    conversation: ConversationId,
    change: SourceChange,
  ) -> Result<(), CoreError> {
    let content = MessageContent::IntegrationSource { change };
    self.enqueue(conversation, None, content.clone())?;
    apply_control(
      &self.store,
      conversation,
      self.account.user_id,
      &content,
      self.now_ms(),
    )?;
    self.flush_outbox().await?;
    Ok(())
  }

  /// Reads the conversation's member list (with the channel roles) from the server again.
  pub async fn refresh_source_authority(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.refresh_members(conversation).await
  }

  /// Loads one more page of older history of `conversation`, so source announcements from before
  /// this store's first page are read too, until the beginning or `max_pages` pages. Once done it
  /// is remembered and never repeated.
  pub async fn scan_history_for_sources(
    &mut self,
    conversation: ConversationId,
    pages_so_far: u32,
    max_pages: u32,
  ) -> Result<HistoryScan, CoreError> {
    let key = scan_key(conversation);
    if records::get_json::<bool>(&self.store, Table::SyncState, &key)?.is_some() {
      return Ok(HistoryScan::Done);
    }
    let is_done = pages_so_far >= max_pages || self.load_older_messages(conversation).await? == 0;
    if !is_done {
      return Ok(HistoryScan::More);
    }
    if pages_so_far >= max_pages {
      tracing::warn!(conversation_id = %conversation, pages = max_pages, "stopped looking for source announcements in older history");
    }
    records::put_json(&self.store, Table::SyncState, &key, &true)?;
    Ok(HistoryScan::Done)
  }
}

fn scan_key(conversation: ConversationId) -> String {
  format!("integration_sources_scanned/{conversation}")
}
