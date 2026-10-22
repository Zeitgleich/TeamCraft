//! The client-api actions (`teamcraft_client_api::actions`) on top of the local store: what
//! [`CoreChatBackend`] reads directly, and the commands it queues for the engine.

use teamcraft_client_api::actions::{
  ForwardedFrom, LinkPreview, MessageEdit, MessageExtras, MessagePriority, OutgoingMessage, PermalinkTarget,
  ReplyPreview, SavedItem, WorkspaceEvent,
};
use teamcraft_client_api::model::{Bookmark, ConversationId, Message, MessageId, ThreadId, UserId};
use teamcraft_proto::ClientMessageId;

use crate::client::previews::queue_preview;
use crate::client::{apply_control, forward_content, reply_ref, stored_account_data};
use crate::link_preview::is_allowed;
use crate::read_state;
use crate::store::{
  BookmarkItem, ConversationEvent, LocalStore, MessageContent, MessageMeta, Priority, StoredMessage,
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
    let names: std::collections::BTreeMap<ConversationId, String> = self
      .conversations_raw()
      .iter()
      .map(|conversation| (conversation.id, self.to_conversation(conversation, &users).name))
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
      .map(|target| reply_ref(&target));
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
    let users = self.summaries();
    let author_name = users
      .get(&original.sender_user_id)
      .map(|summary| summary.display_name.clone())
      .unwrap_or_default();
    let maybe_conversation_name = self
      .conversations_raw()
      .iter()
      .find(|conversation| conversation.id == from)
      .and_then(|conversation| conversation.maybe_name.clone());
    match forward_content(&original, author_name, maybe_conversation_name, comment) {
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

  pub(crate) fn ui_first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    let response = self
      .conversations_raw()
      .into_iter()
      .find(|candidate| candidate.id == conversation)?;
    self
      .unread_messages(&response)
      .first()
      .and_then(|stored| stored.maybe_server_id)
  }

  pub(crate) fn ui_message_extras(
    &self,
    conversation: ConversationId,
    messages: &[MessageId],
  ) -> Vec<MessageExtras> {
    let all = self.all_messages(conversation);
    let data = self.read(stored_account_data(self.store_ref()));
    messages
      .iter()
      .filter_map(|id| {
        let stored = all.iter().find(|stored| ui_message_id(stored) == *id)?;
        let mut extras = MessageExtras::new(*id);
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
          MessageContent::Text { meta, .. } => self.fill_from_meta(&mut extras, meta, &all),
          MessageContent::Event { event } => {
            extras.maybe_workspace_event = workspace_event(event, stored.sender_user_id);
          },
          _ => {},
        }
        extras.files = self.files_of(stored);
        Some(extras)
      })
      .collect()
  }

  fn fill_from_meta(&self, extras: &mut MessageExtras, meta: &MessageMeta, all: &[StoredMessage]) {
    extras.maybe_reply = meta.maybe_reply_to.as_ref().map(|reply| ReplyPreview {
      maybe_message: all
        .iter()
        .find(|stored| stored.client_message_id == reply.target)
        .map(ui_message_id),
      author: reply.author,
      excerpt: reply.excerpt.clone(),
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

/// The UI's view of a conversation event, sent by `actor`; `None` for events the UI shows through
/// its own system events only (joins, leaves, renames, topics).
fn workspace_event(event: &ConversationEvent, actor: UserId) -> Option<WorkspaceEvent> {
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
    ConversationEvent::Archived => WorkspaceEvent::Archived { by: actor },
    ConversationEvent::Unarchived => WorkspaceEvent::Unarchived { by: actor },
    ConversationEvent::MadePrivate => WorkspaceEvent::MadePrivate { by: actor },
    ConversationEvent::MadePublic => WorkspaceEvent::MadePublic { by: actor },
    ConversationEvent::Joined
    | ConversationEvent::Left
    | ConversationEvent::Renamed { .. }
    | ConversationEvent::TopicChanged { .. }
    | ConversationEvent::Unknown => return None,
  })
}
