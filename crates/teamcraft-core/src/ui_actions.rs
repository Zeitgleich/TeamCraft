//! The client-api actions (`teamcraft_client_api::actions`) on top of the local store: what
//! [`CoreChatBackend`] reads directly, and the commands it queues for the engine.

use teamcraft_client_api::actions::{
  ForwardedFrom, LinkPreview, LinkedMessage, MessageEdit, MessageExtras, MessagePriority, OutgoingMessage,
  PermalinkTarget, ReplyPreview, SavedItem, WorkspaceEvent,
};
use teamcraft_client_api::model::{
  Bookmark, ConversationId, IntegrationSource, Message, MessageId, NewIntegrationSource, PENDING_ID_BIT,
  ThreadId, UserId,
};
use teamcraft_client_api::receipts::{is_read_by_all, seen_by};
use teamcraft_proto::{ClientMessageId, SourceChange, SourceId};

use crate::client::previews::queue_preview;
use crate::client::{apply_control, forward_content, imported_author_name, reply_ref, stored_account_data};
use crate::error::CoreError;
use crate::imported::Authorship;
use crate::integration_sources::{SourceAuthority, SourceLedger};
use crate::link_preview::is_allowed;
use crate::read_state;
use crate::records;
use crate::store::{
  BookmarkItem, ConversationEvent, LocalStore, MessageContent, MessageMeta, MessageSlot, Priority,
  StoredMessage, Table,
};
use crate::ui_backend::{CoreChatBackend, CoreCommand, ui_message_id};

impl<S: LocalStore> CoreChatBackend<S> {
  /// The stored message behind an ID the UI got.
  pub(crate) fn find_stored(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Option<StoredMessage> {
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
    let names: std::collections::BTreeMap<ConversationId, String> = self
      .conversations_raw()
      .iter()
      .map(|conversation| {
        (
          conversation.id,
          self.to_conversation(conversation, &users, &preferences).name,
        )
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
        Some(SavedItem {
          message: self.to_message(stored, &all),
          conversation_name: names.get(&item.conversation_id).cloned().unwrap_or_default(),
          saved_at,
        })
      })
      .collect()
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
  pub(crate) fn ui_send_message(&self, message: OutgoingMessage) {
    let conversation = message.conversation;
    let maybe_reply_to = message
      .maybe_reply_to
      .and_then(|id| self.find_stored(conversation, id))
      .map(|target| reply_ref(&target, self.importer()));
    let meta = MessageMeta {
      maybe_reply_to,
      priority: priority_of(message.priority),
      wants_acknowledgement: message.wants_acknowledgement,
      is_silent: message.is_silent,
      is_also_in_channel: message.also_send_to_channel && message.maybe_thread.is_some(),
      ..MessageMeta::default()
    };
    if !message.files.is_empty() {
      self.ui_send_files(
        conversation,
        message.maybe_thread,
        &message.body,
        meta,
        message.files,
      );
      return;
    }
    if message.body.trim().is_empty() {
      return;
    }
    let mentions = self.mentions_in(conversation, &message.body);
    let body = message.body.clone();
    let content = MessageContent::Text {
      body: message.body,
      mentions,
      meta,
    };
    if let Some(id) = self.enqueue(conversation, message.maybe_thread, content) {
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
      author_name,
      maybe_conversation_name,
      comment,
    ) {
      Some(content) => {
        self.enqueue(to, None, content);
      },
      None => tracing::warn!("this kind of message can't be forwarded"),
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
  pub(crate) fn ui_can_sign_channel_roles(&self, conversation: ConversationId) -> bool {
    let members: Option<teamcraft_proto::ListConversationMembersResponse> = self.read(records::get_json(
      self.store_ref(),
      Table::Members,
      &conversation.to_string(),
    ));
    let is_listed_admin = members
      .into_iter()
      .flat_map(|list| list.members)
      .any(|member| member.user.id == self.me_id() && member.role == teamcraft_proto::ChannelRole::Admin);
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
    signed
      .signing_authorities(self.me_id(), &workspace, self.now_ms(), max_age_ms)
      .into_iter()
      .any(|authority| authority != crate::channel_roles::SignerAuthority::Founder || is_listed_admin)
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
        message: self.to_message(&stored, &[]),
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
            extras.maybe_workspace_event = workspace_event(event, stored);
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
      ReplyPreview {
        maybe_message: maybe_target.as_ref().map(ui_message_id),
        // The author the list shows, also for replies that named the import account as the
        // author of an imported message before it was told apart.
        author: maybe_target.as_ref().map_or(reply.author, |target| {
          Authorship::of(target, maybe_importer).author
        }),
        excerpt: reply.excerpt.clone(),
      }
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
    // Receivers enforce the domain list too (e2ee.md §12): previews of disallowed links are dropped.
    let policy = self.verified_policy().link_previews;
    extras.maybe_link_preview = meta
      .maybe_link_preview
      .as_ref()
      .filter(|preview| url::Url::parse(&preview.url).is_ok_and(|url| is_allowed(&policy, &url)))
      .map(|preview| LinkPreview {
        url: preview.url.clone(),
        maybe_title: preview.maybe_title.clone(),
        maybe_description: preview.maybe_description.clone(),
        maybe_site_name: preview.maybe_site_name.clone(),
        maybe_image: preview
          .maybe_image
          .as_ref()
          .map(|image| image.as_slice().to_vec()),
      });
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
    }
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
    ConversationEvent::Joined
    | ConversationEvent::Left
    | ConversationEvent::Renamed { .. }
    | ConversationEvent::TopicChanged { .. }
    | ConversationEvent::CallMoved { .. }
    | ConversationEvent::Unknown => return None,
  })
}
