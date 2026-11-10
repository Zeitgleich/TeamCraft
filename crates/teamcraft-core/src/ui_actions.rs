//! The client-api actions (`teamcraft_client_api::actions`) on top of the local store: what
//! [`CoreChatBackend`] reads directly, and the commands it queues for the engine.

use teamcraft_client_api::actions::{
  ForwardedFrom, LinkPreview, LinkedMessage, MessageEdit, MessageExtras, MessagePriority, OutgoingMessage,
  PermalinkTarget, ReplyAttachmentPreview, ReplyPreview, SavedItem, WorkspaceEvent,
};
use teamcraft_client_api::canvas::{CanvasMark, CanvasOrigin, CanvasState};
use teamcraft_client_api::model::{
  Bookmark, ConversationId, IntegrationSource, Message, MessageId, NewIntegrationSource, PENDING_ID_BIT,
  ThreadId, UserId, is_pending,
};
use teamcraft_client_api::polls::{NewPoll, PollLimits};
use teamcraft_client_api::receipts::{is_delivered, is_read_by_all, seen_by};
use teamcraft_client_api::scheduled::{ScheduledMessage, ScheduledOverview, ScheduledStatus, status_of};
use teamcraft_proto::{Base64Bytes, ClientMessageId, LinkPreviewPolicy, SourceChange, SourceId};

use crate::client::previews::queue_preview;
use crate::client::scheduled::{message_priority, read_list};
use crate::client::{
  ForwardScope, album_attachment, apply_control, forward_content, imported_author_name, reply_ref,
  stored_account_data,
};
use crate::error::CoreError;
use crate::imported::Authorship;
use crate::integration_sources::{SourceAuthority, SourceLedger};
use crate::link_preview::is_allowed;
use crate::read_state;
use crate::records;
use crate::store::{
  BookmarkItem, ConversationEvent, LocalStore, MessageContent, MessageMeta, MessageSlot, Priority,
  ReplyAttachment, ReplyRef, StoredMessage, Table,
};
use crate::ui_backend::{CoreChatBackend, CoreCommand, ui_message_id};

impl<S: LocalStore> CoreChatBackend<S> {
  /// The stored message behind an ID the UI got.
  pub(crate) fn find_stored(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Option<StoredMessage> {
    // A message the server numbered is one keyed read, whatever the conversation's size (it is
    // the common case: replies, pins, saves and edits of messages on screen). The rest (a message
    // still in the outbox, a line this device placed itself) is looked for among all.
    if !is_pending(message)
      && let Ok(Some(stored)) = self.store_ref().message_by_server_id(conversation, message)
    {
      return Some(stored);
    }
    self
      .all_messages(conversation)
      .into_iter()
      .find(|stored| ui_message_id(stored) == message)
  }

  pub(crate) fn ui_set_saved(&self, conversation: ConversationId, message: MessageId, is_saved: bool) {
    match self.find_stored(conversation, message) {
      Some(stored) => self.command(CoreCommand::SetSaved {
        conversation,
        client_message_id: stored.client_message_id,
        is_saved,
      }),
      None => tracing::warn!(conversation_id = %conversation, "saving a message that isn't loaded"),
    }
  }

  pub(crate) fn ui_saved_items(&self) -> Vec<SavedItem> {
    let data = self.read(stored_account_data(self.store_ref()));
    let users = self.summaries();
    let preferences = self.mention_preferences();
    let names: std::collections::BTreeMap<ConversationId, (String, bool)> = self
      .conversations_raw()
      .iter()
      .map(|conversation| {
        let named = self.to_conversation(conversation, &users, &preferences);
        (conversation.id, (named.name, named.is_notes_to_self))
      })
      .collect();
    data
      .saved_items()
      .into_iter()
      .filter_map(|(item, saved_at)| {
        let all = self.all_messages(item.conversation_id);
        let stored = all
          .iter()
          .find(|stored| stored.client_message_id == item.client_message_id)?;
        let (conversation_name, is_notes_to_self) =
          names.get(&item.conversation_id).cloned().unwrap_or_default();
        Some(SavedItem {
          message: self.to_message(stored, &all),
          conversation_name,
          is_notes_to_self,
          saved_at,
        })
      })
      .collect()
  }

  pub(crate) fn ui_scheduled_overview(&self) -> ScheduledOverview {
    let list = self.read(read_list(self.store_ref()));
    let policy = self.verified_policy().scheduled_messages;
    let server_now = self.now_ms().saturating_add(list.server_offset_ms);
    if list.entries.is_empty() {
      return ScheduledOverview {
        messages: Vec::new(),
        policy,
        server_now,
      };
    }
    let users = self.summaries();
    let preferences = self.mention_preferences();
    let names: std::collections::BTreeMap<ConversationId, (String, bool)> = self
      .conversations_raw()
      .iter()
      .map(|conversation| {
        let named = self.to_conversation(conversation, &users, &preferences);
        (conversation.id, (named.name, named.is_notes_to_self))
      })
      .collect();
    let messages = list
      .entries
      .iter()
      .map(|entry| {
        let response = &entry.response;
        let (body, meta) = match &entry.maybe_content {
          Some(MessageContent::Text { body, meta, .. }) => (body.clone(), Some(meta)),
          _ => (String::new(), None),
        };
        let (conversation_name, is_notes_to_self) =
          names.get(&response.conversation_id).cloned().unwrap_or_default();
        ScheduledMessage {
          id: response.id,
          conversation: response.conversation_id,
          conversation_name,
          is_notes_to_self,
          maybe_thread: response.maybe_thread_id,
          body,
          due_at: response.due_at.0,
          status: if entry.is_removed_by_server {
            ScheduledStatus::RemovedByServer
          } else {
            status_of(
              response.maybe_hold.as_ref(),
              response.is_epoch_current,
              entry.maybe_content.is_some(),
            )
          },
          maybe_reply_to: None,
          priority: meta.map_or(MessagePriority::Normal, |meta| message_priority(meta.priority)),
          wants_acknowledgement: meta.is_some_and(|meta| meta.wants_acknowledgement),
          is_silent: meta.is_some_and(|meta| meta.is_silent),
          also_send_to_channel: meta.is_some_and(|meta| meta.is_also_in_channel),
          maybe_removes_at: response.maybe_removes_at.map(|at| at.0),
        }
      })
      .collect();
    ScheduledOverview {
      messages,
      policy,
      server_now,
    }
  }

  pub(crate) fn ui_draft(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) -> Option<String> {
    self
      .read(stored_account_data(self.store_ref()))
      .draft(conversation, maybe_thread)
      .map(str::to_string)
  }
}

/// Scheme and path of permalinks: `teamcraft:/c/<conversation>/m/<message>[/t/<thread>]`. They
/// carry only IDs; only members can open them, since only they have the message.
pub const PERMALINK_PREFIX: &str = "teamcraft:/c/";

impl<S: LocalStore> CoreChatBackend<S> {
  /// What a text message says, with the quote and mentions a send works out. Also for a message that
  /// is sealed now and sent later.
  pub(crate) fn ui_text_content(&self, message: OutgoingMessage) -> MessageContent {
    let meta = self.outgoing_meta(&message);
    self.text_content(&message, meta)
  }

  fn outgoing_meta(&self, message: &OutgoingMessage) -> MessageMeta {
    let maybe_reply_to = message
      .maybe_reply_to
      .and_then(|id| self.find_stored(message.conversation, id))
      .map(|target| reply_ref(&target, self.importer(), message.maybe_reply_attachment));
    MessageMeta {
      maybe_reply_to,
      priority: priority_of(message.priority),
      wants_acknowledgement: message.wants_acknowledgement,
      is_silent: message.is_silent,
      is_also_in_channel: message.also_send_to_channel && message.maybe_thread.is_some(),
      ..MessageMeta::default()
    }
  }

  fn text_content(&self, message: &OutgoingMessage, meta: MessageMeta) -> MessageContent {
    self.text_mentioning(message.conversation, &message.body, meta)
  }

  pub(crate) fn ui_send_message(&self, message: OutgoingMessage) {
    let conversation = message.conversation;
    let meta = self.outgoing_meta(&message);
    if !message.files.is_empty() {
      self.ui_send_files(
        conversation,
        message.maybe_thread,
        &message.body,
        meta,
        message.files,
        message.maybe_client_message_id,
      );
      return;
    }
    if message.body.trim().is_empty() {
      return;
    }
    let body = message.body.clone();
    let (maybe_thread, maybe_id) = (message.maybe_thread, message.maybe_client_message_id);
    let content = self.text_content(&message, meta);
    let maybe_id = self.enqueue_as(conversation, maybe_thread, content, maybe_id);
    if let Some(id) = maybe_id {
      self.queue_link_preview(conversation, id, &body);
    }
  }

  /// Holds the message for a link preview if it links somewhere the verified policy allows.
  pub(crate) fn queue_link_preview(&self, conversation: ConversationId, id: ClientMessageId, body: &str) {
    let policy = self.verified_policy().link_previews;
    match queue_preview(self.store_ref(), &policy, conversation, id, body) {
      Ok(true) => self.command(CoreCommand::GeneratePreviews),
      Ok(false) => {},
      Err(error) => tracing::error!(%error, "could not queue the link preview"),
    }
  }

  /// The verified workspace policy (the default before one was signed).
  pub(crate) fn verified_policy(&self) -> teamcraft_proto::WorkspacePolicy {
    let state: Option<crate::workspace::WorkspaceState> = self.read(crate::records::get_json(
      self.store_ref(),
      crate::store::Table::Workspace,
      crate::client::WORKSPACE_KEY,
    ));
    state.unwrap_or_default().policy
  }

  pub(crate) fn ui_edit_message(&self, conversation: ConversationId, message: MessageId, body: &str) {
    let Some(target) = self.own_text(conversation, message) else {
      return;
    };
    self.control(
      conversation,
      MessageContent::Edit {
        target: target.client_message_id,
        body: body.to_string(),
      },
    );
  }

  pub(crate) fn ui_delete_message(&self, conversation: ConversationId, message: MessageId) {
    let Some(target) = self.find_stored(conversation, message) else {
      return;
    };
    if target.sender_user_id != self.me_id() {
      tracing::warn!("only the sender can delete a message for everyone");
      return;
    }
    self.control(
      conversation,
      MessageContent::Delete {
        target: target.client_message_id,
      },
    );
  }

  /// "Retry" on an own message the server rejected: back into the outbox's line.
  pub(crate) fn ui_retry_send(&self, conversation: ConversationId, message: MessageId) {
    let Some(target) = self.find_stored(conversation, message) else {
      return;
    };
    // A message whose files stopped retrying waits in the outbox; its files are due again.
    match crate::client::files::retry_stalled_upload(self.store_ref(), conversation, target.client_message_id)
    {
      Ok(true) => return self.command(CoreCommand::UploadFiles),
      Ok(false) => {},
      Err(error) => tracing::error!(%error, "could not retry the files"),
    }
    match records::retry_failed(self.store_ref(), conversation, target.client_message_id) {
      Ok(()) => self.command(CoreCommand::FlushOutbox),
      Err(error) => tracing::error!(%error, "could not retry the message"),
    }
  }

  /// "Delete" on an own message the server rejected.
  pub(crate) fn ui_discard_unsent(&self, conversation: ConversationId, message: MessageId) {
    let Some(target) = self.find_stored(conversation, message) else {
      return;
    };
    if let Err(error) = records::discard_failed(self.store_ref(), conversation, target.client_message_id) {
      tracing::error!(%error, "could not discard the message");
    }
  }

  pub(crate) fn ui_forward_message(
    &self,
    from: ConversationId,
    message: MessageId,
    scope: ForwardScope,
    to: ConversationId,
    comment: &str,
  ) {
    let Some(original) = self.find_stored(from, message) else {
      return;
    };
    let authorship = Authorship::of(&original, self.importer());
    let author_name = match authorship.maybe_account() {
      Some(author) => self
        .summaries()
        .get(&author)
        .map(|summary| summary.display_name.clone())
        .unwrap_or_default(),
      None => imported_author_name(&authorship),
    };
    let maybe_conversation_name = self
      .conversations_raw()
      .iter()
      .find(|conversation| conversation.id == from)
      .and_then(|conversation| conversation.maybe_name.clone());
    match forward_content(
      &authorship,
      &original,
      scope,
      author_name,
      maybe_conversation_name,
      comment,
    ) {
      Some(content) => {
        self.enqueue(to, None, content);
      },
      None => tracing::warn!("this can't be forwarded: a control message, or a file that is gone"),
    }
  }

  pub(crate) fn ui_set_pinned(&self, conversation: ConversationId, message: MessageId, is_pinned: bool) {
    let Some(target) = self.find_stored(conversation, message) else {
      return;
    };
    self.control(
      conversation,
      MessageContent::Pin {
        target: target.client_message_id,
        is_pinned,
      },
    );
    self.command(CoreCommand::SetServerPin {
      conversation,
      target: target.client_message_id,
      is_pinned,
    });
  }

  /// Sends a poll (issue #205, e2ee.md §4.12): a text message whose text is what versions before
  /// polls show, with the poll inside the ciphertext.
  pub(crate) fn ui_post_poll(&self, conversation: ConversationId, poll: NewPoll) {
    if let Err(error) = poll.spec.check(&PollLimits::default()) {
      tracing::warn!(conversation_id = %conversation, %error, "not sending a poll the limits refuse");
      return;
    }
    let meta = MessageMeta {
      maybe_poll: Some(Box::new(poll.spec)),
      ..MessageMeta::default()
    };
    let content = MessageContent::Text {
      body: poll.fallback,
      mentions: Vec::new(),
      meta,
    };
    self.enqueue(conversation, None, content);
  }

  /// Votes in the poll `message` (no choices withdraw the vote): a hidden message like a reaction.
  pub(crate) fn ui_vote_in_poll(&self, conversation: ConversationId, message: MessageId, choices: Vec<u32>) {
    let Some(poll) = self.find_stored(conversation, message) else {
      tracing::warn!(conversation_id = %conversation, "voting in a poll that isn't loaded");
      return;
    };
    let is_allowed =
      crate::polls::poll_of(&poll).is_some_and(|spec| spec.choice.allows(&choices, spec.options.len()));
    if !is_allowed {
      tracing::warn!(conversation_id = %conversation, "not sending a vote the poll doesn't take");
      return;
    }
    let content = MessageContent::PollVote {
      target: poll.client_message_id,
      choices,
    };
    self.enqueue(conversation, None, content);
  }

  /// Closes one of the user's own polls. Others' devices only take a close from the poll's sender.
  pub(crate) fn ui_close_poll(&self, conversation: ConversationId, message: MessageId) {
    let Some(poll) = self.find_stored(conversation, message) else {
      return;
    };
    if poll.sender_user_id != self.me_id() || crate::polls::poll_of(&poll).is_none() {
      tracing::warn!(conversation_id = %conversation, "only the poll's sender closes it");
      return;
    }
    let content = MessageContent::PollClose {
      target: poll.client_message_id,
    };
    self.enqueue(conversation, None, content);
  }

  pub(crate) fn ui_set_bookmarks(&self, conversation: ConversationId, bookmarks: Vec<Bookmark>) {
    let items = bookmarks
      .into_iter()
      .map(|bookmark| BookmarkItem {
        emoji: bookmark.emoji,
        title: bookmark.title,
        url: bookmark.url,
        maybe_message: bookmark
          .maybe_message
          .and_then(|message| self.find_stored(conversation, message))
          .map(|stored| stored.client_message_id),
      })
      .collect();
    self.control(conversation, MessageContent::Bookmarks { items });
  }

  /// The channel's shared page as this device holds it (e2ee.md §4.11).
  pub(crate) fn ui_canvas(&self, conversation: ConversationId) -> CanvasState {
    let record = self.read(crate::canvas::load(self.store_ref(), conversation));
    CanvasState {
      revision: record.revision,
      updates: record.updates,
      updates_since_snapshot: record.updates_since_snapshot,
      origins: record
        .origins
        .into_iter()
        .map(|maybe_origin| {
          maybe_origin.map(|origin| CanvasOrigin {
            by: origin.by,
            at_ms: origin.at_ms,
            is_snapshot: origin.is_snapshot,
          })
        })
        .collect(),
      is_compacted: record.is_compacted,
      history: record
        .history
        .into_iter()
        .map(|mark| CanvasMark {
          maybe_by: mark.maybe_by,
          from_ms: mark.from_ms,
          until_ms: mark.until_ms,
          changes: mark.changes,
        })
        .collect(),
    }
  }

  /// Keeps an edit of the page and sends it to the other members, where the user may post: the
  /// server refuses the message of anybody else, and the editor is read-only for them.
  pub(crate) fn ui_edit_canvas(&self, conversation: ConversationId, update: Vec<u8>, is_snapshot: bool) {
    let may_post = self
      .conversations_raw()
      .iter()
      .any(|response| response.id == conversation && response.may_post());
    if !may_post {
      tracing::warn!(%conversation, "a page edit where the user may not post was not sent");
      return;
    }
    self.control(
      conversation,
      MessageContent::Canvas {
        update: Base64Bytes::new(update),
        is_snapshot,
      },
    );
  }

  /// Replaces the page's updates with the editor's merge of them and its history with the one
  /// the editor made of them, if none arrived meanwhile.
  pub(crate) fn ui_compact_canvas(
    &self,
    conversation: ConversationId,
    revision: u64,
    merged: Vec<u8>,
    history: Vec<CanvasMark>,
  ) {
    let history = history
      .into_iter()
      .map(|mark| crate::canvas::CanvasMark {
        maybe_by: mark.maybe_by,
        from_ms: mark.from_ms,
        until_ms: mark.until_ms,
        changes: mark.changes,
      })
      .collect();
    if let Err(error) = crate::canvas::compact(self.store_ref(), conversation, revision, merged, history) {
      tracing::error!(%error, "could not compact the page");
    }
  }

  /// The channel's sources in force as this device judges them (integration_sources.rs). The UI
  /// process doesn't look into the MLS group: decryption already proved the announcer was in it,
  /// and the bridge checks the group itself.
  pub(crate) fn ui_integration_sources(&self, conversation: ConversationId) -> Vec<IntegrationSource> {
    let ledger = self.read(SourceLedger::load(self.store_ref(), conversation));
    if ledger.revision == 0 {
      return Vec::new();
    }
    let Some(authority) = self.source_authority() else {
      return Vec::new();
    };
    ledger
      .active(conversation, &authority)
      .into_iter()
      .map(|source| IntegrationSource {
        id: source.id,
        name: source.name,
        kind: source.kind,
        created_by: source.created_by,
        created_at_ms: source.created_at_ms,
        maybe_picture_sha256: source.maybe_picture_sha256,
      })
      .collect()
  }

  /// Whether the user's sources would be in force: an admin of the verified, fresh roster.
  pub(crate) fn ui_can_manage_integration_sources(&self) -> bool {
    self
      .source_authority()
      .is_some_and(|authority| authority.may_manage(self.me_id()))
  }

  /// Whether this user signs the channel's roles in a way members accept: an admin of the fresh
  /// workspace roster, a signed channel admin, or, while nothing is signed, an admin the server
  /// lists, who founds the log (e2ee.md §4.6.1).
  /// For a workspace admin, always: their look at the channel's roles is the one thing that ends an
  /// alarm or a stretch, or repairs a role nothing else can (e2ee.md §4.6.1). Nobody is asked to: it is
  /// there when they open the channel's members, and says more where it would help. `None` for anybody
  /// else.
  pub(crate) fn ui_channel_roles_review(
    &self,
    conversation: ConversationId,
  ) -> Option<teamcraft_client_api::model::ChannelRolesReview> {
    let workspace: crate::workspace::WorkspaceState = self.read(
      records::get_json(self.store_ref(), Table::Workspace, crate::client::WORKSPACE_KEY)
        .map(Option::unwrap_or_default),
    );
    let signed: crate::channel_roles::ChannelRoleState = self.read(
      records::get_json(
        self.store_ref(),
        Table::Workspace,
        &crate::client::channel_roles_key(conversation),
      )
      .map(Option::unwrap_or_default),
    );
    let max_age_ms =
      i64::try_from(crate::ClientSettings::default().workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    let is_vouching = signed
      .signing_authorities(self.me_id(), &workspace, self.now_ms(), max_age_ms)
      .iter()
      .any(is_workspace_authority);
    let members: Option<teamcraft_proto::ListConversationMembersResponse> = self.read(records::get_json(
      self.store_ref(),
      Table::Members,
      &conversation.to_string(),
    ));
    let lists_a_role_nobody_confirmed = !signed.is_unfounded()
      && members.into_iter().flat_map(|list| list.members).any(|member| {
        member.user.kind == teamcraft_proto::UserKind::Member
          && member.role != teamcraft_proto::ChannelRole::Member
          && signed.role_of(member.user.id) != member.role
      });
    let needs_a_look = signed.must_review_before_vouching()
      || lists_a_role_nobody_confirmed
      || signed.maybe_alarm.is_some()
      // A log this device took nothing from: only such a list gets it out.
      || (!signed.is_signed() && signed.cursor > 0);
    let standings: std::collections::BTreeMap<UserId, crate::channel_roles::RoleKeyStanding> = self.read(
      records::get_json(
        self.store_ref(),
        Table::Workspace,
        &crate::client::role_key_standings_key(conversation),
      )
      .map(Option::unwrap_or_default),
    );
    is_vouching.then(|| teamcraft_client_api::model::ChannelRolesReview {
      log_cursor: signed.cursor,
      needs_a_look,
      key_marks: standings
        .into_iter()
        .map(|(user, standing)| {
          use crate::channel_roles::RoleKeyStanding;
          use teamcraft_client_api::model::RoleKeyMark;
          let mark = match standing {
            RoleKeyStanding::Checked => RoleKeyMark::Checked,
            RoleKeyStanding::Changed => RoleKeyMark::Changed,
            RoleKeyStanding::Uncheckable | RoleKeyStanding::Unknown => RoleKeyMark::Uncheckable,
          };
          (user, mark)
        })
        .collect(),
      snapshot: signed
        .assignments
        .iter()
        .map(|assignment| (assignment.user_id, assignment.role.clone()))
        .collect(),
    })
  }

  /// The channels where this device and a member's disagree about the signed roles.
  pub(crate) fn ui_channel_role_alarms(&self) -> Vec<ConversationId> {
    let alarmed: std::collections::BTreeSet<ConversationId> = self.read(
      records::get_json(
        self.store_ref(),
        Table::Workspace,
        crate::client::channel_roles_alarmed_key(),
      )
      .map(Option::unwrap_or_default),
    );
    alarmed.into_iter().collect()
  }

  fn source_authority(&self) -> Option<SourceAuthority> {
    let workspace: crate::workspace::WorkspaceState = self.read(
      records::get_json(self.store_ref(), Table::Workspace, crate::client::WORKSPACE_KEY)
        .map(Option::unwrap_or_default),
    );
    let max_age_ms =
      i64::try_from(crate::ClientSettings::default().workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    let is_fresh = workspace.is_roster_fresh(self.now_ms(), max_age_ms);
    self.read(SourceAuthority::from_store(self.store_ref(), &workspace, is_fresh, None, []).map(Some))
  }

  pub(crate) fn ui_add_integration_source(&self, conversation: ConversationId, source: NewIntegrationSource) {
    let change = SourceChange::Created {
      source: source.id,
      name: source.name,
      kind: source.kind,
      token_sha256: source.token_sha256,
      options: source.options,
    };
    self.control(conversation, MessageContent::IntegrationSource { change });
  }

  /// A request to the notetaker (e2ee.md §8.2), taken into this device's ledger at once and sent
  /// encrypted like any control message.
  pub(crate) fn ui_request_notetaker(
    &self,
    conversation: ConversationId,
    request: teamcraft_proto::NotetakerRequest,
  ) {
    self.control(conversation, MessageContent::Notetaker { request });
  }

  /// Renames a source; messages sent from now on show the new name.
  pub(crate) fn ui_rename_integration_source(
    &self,
    conversation: ConversationId,
    source: SourceId,
    name: String,
  ) {
    let change = SourceChange::Renamed { source, name };
    self.control(conversation, MessageContent::IntegrationSource { change });
  }

  /// Sets a source's picture (a small PNG), or takes it away.
  pub(crate) fn ui_set_integration_source_picture(
    &self,
    conversation: ConversationId,
    source: SourceId,
    maybe_picture: Option<Vec<u8>>,
  ) {
    let change = SourceChange::PictureChanged {
      source,
      maybe_picture: maybe_picture.map(Base64Bytes::new),
    };
    self.control(conversation, MessageContent::IntegrationSource { change });
  }

  /// A source picture this device keeps for the conversation.
  pub(crate) fn ui_integration_source_picture(
    &self,
    conversation: ConversationId,
    sha256: teamcraft_proto::SourcePictureHash,
  ) -> Option<Vec<u8>> {
    self.read(crate::integration_sources::source_picture(
      self.store_ref(),
      conversation,
      sha256,
    ))
  }

  pub(crate) fn ui_revoke_integration_source(&self, conversation: ConversationId, source: SourceId) {
    let change = SourceChange::Revoked { source };
    self.control(conversation, MessageContent::IntegrationSource { change });
  }

  pub(crate) fn ui_pinned_messages(&self, conversation: ConversationId) -> Vec<Message> {
    let all = self.all_messages(conversation);
    all
      .iter()
      .filter(|stored| stored.is_pinned && !stored.is_hidden())
      .map(|stored| self.to_message(stored, &all))
      .collect()
  }

  pub(crate) fn ui_permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    let stored = self.find_stored(conversation, message)?;
    let server_id = stored.maybe_server_id?;
    Some(match stored.maybe_thread_id {
      Some(thread) => format!("{PERMALINK_PREFIX}{conversation}/m/{server_id}/t/{thread}"),
      None => format!("{PERMALINK_PREFIX}{conversation}/m/{server_id}"),
    })
  }

  pub(crate) fn ui_resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    let target = parse_permalink(link)?;
    // Only conversations this device is a member of, and messages it can read.
    self
      .conversations_raw()
      .iter()
      .any(|conversation| conversation.id == target.conversation)
      .then_some(())?;
    self
      .all_messages(target.conversation)
      .iter()
      .any(|stored| stored.maybe_server_id == Some(target.message))
      .then_some(target)
  }

  /// Point lookups only (the conversation's record, then the message by its server ID): previews
  /// are read again on every store change, so they must not read whole conversations.
  pub(crate) fn ui_linked_message(&self, link: &str) -> LinkedMessage {
    let Some(target) = parse_permalink(link) else {
      return LinkedMessage::Unavailable;
    };
    // Only conversations this device is a member of: the preview never asks the server.
    let is_member = self
      .read(
        self
          .store_ref()
          .get(Table::Conversations, &target.conversation.to_string())
          .map_err(CoreError::from),
      )
      .is_some();
    if !is_member {
      return LinkedMessage::Unavailable;
    }
    let maybe_stored = self.read(
      self
        .store_ref()
        .message_by_server_id(target.conversation, target.message)
        .map_err(CoreError::from),
    );
    let Some(stored) = maybe_stored else {
      // A member whose device hasn't synced (or has purged) the message yet.
      return LinkedMessage::NotLoaded { target };
    };
    match stored.content {
      MessageContent::Deleted => LinkedMessage::Deleted { target },
      // Without the rest of the conversation: a preview shows no reactions or reply counts.
      MessageContent::Text { .. } | MessageContent::File { .. } => LinkedMessage::Visible {
        target,
        message: Box::new(self.to_message(&stored, &[])),
        files: self.files_of(&stored, self.importer()),
      },
      // Reactions, edits, events and the like are no message one can link to.
      _ => LinkedMessage::Unavailable,
    }
  }

  pub(crate) fn ui_first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    let response = self
      .conversations_raw()
      .into_iter()
      .find(|candidate| candidate.id == conversation)?;
    // A line derived from a commit can be the first unread one: its row goes by the commit's ID.
    self.unread_messages(&response).first().map(ui_message_id)
  }

  pub(crate) fn ui_message_extras(
    &self,
    conversation: ConversationId,
    messages: &[MessageId],
  ) -> Vec<MessageExtras> {
    let data = self.read(stored_account_data(self.store_ref()));
    let ticks = Ticks::read(self, conversation);
    let maybe_importer = self.importer();
    messages
      .iter()
      .filter_map(|id| {
        // Looked up by its place: the UI asks for each row it shows, and reading the whole
        // conversation per row kept a core busy in a large channel (issue #1000).
        let slot = if teamcraft_proto::Id::snowflake(*id).get() & PENDING_ID_BIT != 0 {
          MessageSlot::Pending
        } else {
          MessageSlot::At(*id)
        };
        let candidates = self.read(
          self
            .store_ref()
            .messages_at(conversation, slot)
            .map_err(CoreError::from),
        );
        // A server can give a message the ID a derived line sits at (a commit's, issue #811; the
        // announcement a line moved to is hidden, issue #818); the line wins, so the ID clash
        // can't hide it.
        let stored = candidates
          .iter()
          .find(|stored| stored.is_local_line() && ui_message_id(stored) == *id)
          .or_else(|| candidates.iter().find(|stored| ui_message_id(stored) == *id))?;
        let mut extras = MessageExtras::new(*id);
        extras.is_read = ticks.is_read(stored, self.me_id());
        extras.is_delivered = ticks.is_delivered(stored, self.me_id());
        extras.is_pinned = stored.is_pinned;
        extras.is_saved = data.is_saved(conversation, stored.client_message_id);
        extras.is_deleted = matches!(stored.content, MessageContent::Deleted);
        extras.edits = stored
          .edits
          .iter()
          .map(|edit| MessageEdit {
            body: edit.body.clone(),
            replaced_at: edit.replaced_at_ms,
          })
          .collect();
        if let Some(server_id) = stored.maybe_server_id {
          extras.acknowledged_by = self.read(read_state::acknowledgements(
            self.store_ref(),
            conversation,
            server_id,
          ));
        }
        match &stored.content {
          MessageContent::Text { meta, .. } => {
            self.fill_from_meta(&mut extras, meta, conversation, maybe_importer)
          },
          MessageContent::Event { event } => {
            extras.maybe_workspace_event =
              workspace_event(event, stored).map(|event| self.with_room_name(event));
          },
          _ => {},
        }
        extras.files = self.files_of(stored, maybe_importer);
        if stored.maybe_server_id.is_none() {
          extras.is_send_failed = self.read(records::is_send_failed(
            self.store_ref(),
            conversation,
            stored.client_message_id,
          ));
        }
        Some(extras)
      })
      .collect()
  }

  /// The newest `CallMoved` announced in `conversation` (issue #1321), named as
  /// [`Self::with_room_name`] names it: the room's name is empty if the user isn't in the room.
  pub(crate) fn ui_moved_call(
    &self,
    conversation: ConversationId,
  ) -> Option<teamcraft_client_api::notetaker::MovedCall> {
    let mut newest: Option<(i64, UserId, ConversationId)> = None;
    self.with_all_messages(conversation, &mut |all| {
      for stored in all {
        let MessageContent::Event {
          event: ConversationEvent::CallMoved { meeting, .. },
        } = &stored.content
        else {
          continue;
        };
        if newest.is_none_or(|(at, ..)| stored.sent_at_ms >= at) {
          newest = Some((stored.sent_at_ms, stored.sender_user_id, *meeting));
        }
      }
    });
    let (moved_at, by, room) = newest?;
    let WorkspaceEvent::CallMoved { room_name, .. } = self.with_room_name(WorkspaceEvent::CallMoved {
      by,
      room,
      room_name: String::new(),
    }) else {
      return None;
    };
    Some(teamcraft_client_api::notetaker::MovedCall {
      by,
      room,
      room_name,
      moved_at,
    })
  }

  /// `event` with the name of the room a call moved into, as the sidebar shows it, if the user is
  /// in that room (issue #1290): the line links to it then. Other events as they are.
  fn with_room_name(&self, event: WorkspaceEvent) -> WorkspaceEvent {
    let WorkspaceEvent::CallMoved { by, room, .. } = event else {
      return event;
    };
    let is_member = self
      .conversations_raw()
      .iter()
      .any(|conversation| conversation.id == room && conversation.maybe_my_membership.is_some());
    let room_name = is_member
      .then(|| teamcraft_client_api::backend::ChatBackend::conversation(self, room))
      .flatten()
      .map(|conversation| conversation.name)
      .unwrap_or_default();
    WorkspaceEvent::CallMoved { by, room, room_name }
  }

  fn fill_from_meta(
    &self,
    extras: &mut MessageExtras,
    meta: &MessageMeta,
    conversation: ConversationId,
    maybe_importer: Option<UserId>,
  ) {
    extras.maybe_reply = meta.maybe_reply_to.as_ref().map(|reply| {
      let maybe_target = self.read(
        self
          .store_ref()
          .message(conversation, reply.target)
          .map_err(CoreError::from),
      );
      reply_preview(reply, maybe_target.as_ref(), maybe_importer)
    });
    extras.maybe_forwarded = meta.maybe_forwarded.as_ref().map(|forwarded| ForwardedFrom {
      author_name: forwarded.author_name.clone(),
      maybe_conversation_name: forwarded.maybe_conversation_name.clone(),
      sent_at: forwarded.sent_at_ms,
    });
    extras.priority = match meta.priority {
      Priority::Important => MessagePriority::Important,
      Priority::Urgent => MessagePriority::Urgent,
      Priority::Normal | Priority::Unknown => MessagePriority::Normal,
    };
    extras.wants_acknowledgement = meta.wants_acknowledgement;
    extras.is_silent = meta.is_silent;
    extras.is_also_in_channel = meta.is_also_in_channel;
    extras.maybe_link_preview = shown_link_preview(meta, &self.verified_policy().link_previews);
  }

  /// One of the user's own text messages.
  fn own_text(&self, conversation: ConversationId, message: MessageId) -> Option<StoredMessage> {
    self
      .find_stored(conversation, message)
      .filter(|stored| stored.sender_user_id == self.me_id())
      .filter(|stored| matches!(stored.content, MessageContent::Text { .. }))
  }

  /// Sends a control message (edit, delete, pin) and applies it here right away.
  fn control(&self, conversation: ConversationId, content: MessageContent) {
    if let Err(error) = apply_control(
      self.store_ref(),
      conversation,
      self.me_id(),
      &content,
      self.now_ms(),
    ) {
      tracing::error!(%error, "could not apply the change locally");
    }
    self.enqueue(conversation, None, content);
  }
}

/// What the two ticks of the user's own messages need, read once per conversation: its members
/// and their receipts where receipts show as ticks (a DM, #635), nothing elsewhere.
struct Ticks {
  maybe_members: Option<Vec<UserId>>,
  receipts: Vec<(UserId, MessageId)>,
  /// How far every other member has the messages on a device (DMs and group DMs, #598).
  maybe_delivered: Option<MessageId>,
}

impl Ticks {
  fn read<S: LocalStore>(backend: &CoreChatBackend<S>, conversation: ConversationId) -> Self {
    let maybe_members = backend.tick_members(conversation);
    let receipts = match maybe_members {
      Some(_) => backend.read_receipts(conversation),
      None => Vec::new(),
    };
    Self {
      maybe_members,
      receipts,
      maybe_delivered: backend.delivered_up_to(conversation),
    }
  }

  /// Whether `stored`, an own message the server has, reached a device of every other member.
  fn is_delivered(&self, stored: &StoredMessage, me: UserId) -> bool {
    stored.sender_user_id == me
      && stored
        .maybe_server_id
        .is_some_and(|server_id| is_delivered(self.maybe_delivered, server_id))
  }

  /// Whether every other member read `stored`, an own message the server has (#568, #769).
  fn is_read(&self, stored: &StoredMessage, me: UserId) -> bool {
    match (&self.maybe_members, stored.maybe_server_id) {
      (Some(members), Some(server_id)) if stored.sender_user_id == me => {
        is_read_by_all(&seen_by(&self.receipts, server_id), members, me)
      },
      _ => false,
    }
  }
}

/// The sender's preview of a link in `meta`, when the workspace allows previews of its domain:
/// receivers enforce the domain list too (e2ee.md §12), so previews of disallowed links are dropped.
pub(crate) fn shown_link_preview(meta: &MessageMeta, policy: &LinkPreviewPolicy) -> Option<LinkPreview> {
  meta
    .maybe_link_preview
    .as_ref()
    .filter(|preview| url::Url::parse(&preview.url).is_ok_and(|url| is_allowed(policy, &url)))
    .map(|preview| LinkPreview {
      url: preview.url.clone(),
      maybe_title: preview.maybe_title.clone(),
      maybe_description: preview.maybe_description.clone(),
      maybe_site_name: preview.maybe_site_name.clone(),
      maybe_image: preview
        .maybe_image
        .as_ref()
        .map(|image| image.as_slice().to_vec()),
    })
}

/// What a reader sees of the picture a reply quotes. The quoted message's own thumbnail wins while
/// it has the picture, so a sender can't show another picture under someone else's name; once the
/// picture is deleted or expired, the copy in the reply is what's left.
/// A quote as the apps show it: of `reply`, with what `maybe_target` says now where it is there.
pub(crate) fn reply_preview(
  reply: &ReplyRef,
  maybe_target: Option<&StoredMessage>,
  maybe_importer: Option<UserId>,
) -> ReplyPreview {
  ReplyPreview {
    maybe_message: maybe_target.map(ui_message_id),
    // The author the list shows, also for replies that named the import account as the
    // author of an imported message before it was told apart.
    author: maybe_target.map_or(reply.author, |target| {
      Authorship::of(target, maybe_importer).author
    }),
    excerpt: reply.excerpt.clone(),
    maybe_attachment: reply
      .maybe_attachment
      .as_deref()
      .map(|quoted| quoted_attachment(quoted, maybe_target)),
  }
}

fn quoted_attachment(
  quoted: &ReplyAttachment,
  maybe_target: Option<&StoredMessage>,
) -> ReplyAttachmentPreview {
  let maybe_live = maybe_target.and_then(|target| album_attachment(target, quoted.attachment_id));
  let maybe_thumbnail = maybe_live
    .and_then(|attachment| attachment.maybe_thumbnail.as_ref())
    .or(quoted.maybe_thumbnail.as_ref());
  ReplyAttachmentPreview {
    file_id: quoted.attachment_id.to_string(),
    file_name: maybe_live
      .map_or(&quoted.file_name, |attachment| &attachment.file_name)
      .clone(),
    maybe_thumbnail_png: maybe_thumbnail.map(|thumbnail| thumbnail.as_slice().to_vec()),
    can_open: maybe_live.is_some(),
  }
}

fn priority_of(priority: MessagePriority) -> Priority {
  match priority {
    MessagePriority::Normal => Priority::Normal,
    MessagePriority::Important => Priority::Important,
    MessagePriority::Urgent => Priority::Urgent,
  }
}

fn parse_permalink(link: &str) -> Option<PermalinkTarget> {
  let rest = link.trim().strip_prefix(PERMALINK_PREFIX)?;
  let mut parts = rest.split('/');
  let conversation = parts.next()?.parse().ok()?;
  if parts.next()? != "m" {
    return None;
  }
  let message = parts.next()?.parse().ok()?;
  let maybe_thread = match (parts.next(), parts.next()) {
    (Some("t"), Some(thread)) => Some(thread.parse().ok()?),
    (None, None) => None,
    _ => return None,
  };
  if parts.next().is_some() {
    return None;
  }
  Some(PermalinkTarget {
    conversation,
    message,
    maybe_thread,
  })
}

/// The UI's view of the conversation event `stored` holds; `None` for events the UI shows through
/// its own system events only (joins, leaves, renames, topics). A change the server reports names
/// whoever the line names, if anybody (issue #818); other events their sender.
fn workspace_event(event: &ConversationEvent, stored: &StoredMessage) -> Option<WorkspaceEvent> {
  let actor = stored.sender_user_id;
  let maybe_by = stored.maybe_actor();
  Some(match event {
    ConversationEvent::MemberAdded { user, history } => WorkspaceEvent::MemberAdded {
      user: *user,
      by: actor,
      history: history.clone(),
    },
    ConversationEvent::MemberRemoved { user } => WorkspaceEvent::MemberRemoved {
      user: *user,
      by: actor,
    },
    // Only the line derived from the commit shows (issue #811): its author is who let the admin
    // in, or the admin for their own self-join.
    ConversationEvent::AdminJoined { user, is_approved } => WorkspaceEvent::AdminJoined {
      user: *user,
      maybe_approved_by: is_approved.then_some(actor),
      maybe_added_by: (!is_approved && actor != *user).then_some(actor),
    },
    ConversationEvent::Archived => WorkspaceEvent::Archived { maybe_by },
    ConversationEvent::Unarchived => WorkspaceEvent::Unarchived { maybe_by },
    ConversationEvent::MadePrivate => WorkspaceEvent::MadePrivate { maybe_by },
    ConversationEvent::MadePublic => WorkspaceEvent::MadePublic { maybe_by },
    ConversationEvent::Moved { space } => WorkspaceEvent::Moved {
      maybe_by,
      space: space.clone(),
    },
    // The room's name is filled in by the caller ([`CoreChatBackend::with_room_name`]).
    ConversationEvent::CallMoved { meeting, .. } => WorkspaceEvent::CallMoved {
      by: actor,
      room: *meeting,
      room_name: String::new(),
    },
    ConversationEvent::Joined
    | ConversationEvent::Left
    | ConversationEvent::Renamed { .. }
    | ConversationEvent::TopicChanged { .. }
    | ConversationEvent::Unknown => return None,
  })
}

fn is_workspace_authority(authority: &crate::channel_roles::SignerAuthority) -> bool {
  matches!(authority, crate::channel_roles::SignerAuthority::Workspace { .. })
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Base64Bytes;

  use super::*;
  use crate::client::reply_ref;
  use crate::store::tests::{id, message};
  use crate::store::{AttachmentRef, MessageMeta};

  #[test]
  fn the_quote_of_a_picture_that_expired_shows_the_copy_in_the_reply() {
    let quoted = quoted_copy(b"copy");
    let preview = quoted_attachment(&quoted, None);
    assert_eq!(preview.maybe_thumbnail_png.as_deref(), Some(&b"copy"[..]));
    assert_eq!(preview.file_name, "c.jpg");
    assert!(!preview.can_open);
  }

  #[test]
  fn the_quoted_message_decides_while_it_has_the_picture() {
    let album = album_of(&[id(3), id(4)]);
    let preview = quoted_attachment(&quoted_copy(b"another picture"), Some(&album));
    assert_eq!(preview.maybe_thumbnail_png.as_deref(), Some(&b"real"[..]));
    assert!(preview.can_open);
  }

  #[test]
  fn a_picture_removed_from_its_message_can_not_be_opened() {
    let album = album_of(&[id(4), id(5)]);
    let preview = quoted_attachment(&quoted_copy(b"copy"), Some(&album));
    assert_eq!(preview.maybe_thumbnail_png.as_deref(), Some(&b"copy"[..]));
    assert!(!preview.can_open);
  }

  fn quoted_copy(thumbnail: &[u8]) -> ReplyAttachment {
    let album = album_of(&[id(3), id(4)]);
    let mut quoted = reply_ref(&album, None, Some(id(3))).maybe_attachment.unwrap();
    quoted.maybe_thumbnail = Some(Base64Bytes::from(thumbnail.to_vec()));
    *quoted
  }

  fn album_of(ids: &[teamcraft_proto::AttachmentId]) -> StoredMessage {
    let attachments = ids.iter().map(|&attachment_id| AttachmentRef {
      attachment_id,
      key: Base64Bytes::from(vec![9; 32]),
      ciphertext_sha256: Base64Bytes::from(vec![8; 32]),
      file_name: "c.jpg".into(),
      media_type: "image/jpeg".into(),
      size_bytes: 10,
      maybe_thumbnail: Some(Base64Bytes::from(b"real".to_vec())),
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_waveform: None,
    });
    StoredMessage {
      content: MessageContent::Text {
        body: String::new(),
        mentions: Vec::new(),
        meta: MessageMeta {
          attachments: attachments.collect(),
          ..MessageMeta::default()
        },
      },
      ..message(id(10), 1, Some(101), None, 5_000)
    }
  }
}
