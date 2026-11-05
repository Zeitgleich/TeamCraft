//! Message actions beyond plain text: rich sending (replies, priority, acknowledgement requests,
//! silent, "also send to channel"), pins, forwards, acknowledgements and marking unread.

use teamcraft_proto::{AttachmentId, ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

use super::messaging::apply_control;
use super::{Client, PendingAction};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::imported::{Authorship, ImportedAuthor};
use crate::read_state;
use crate::store::{
  AttachmentRef, Forwarded, LocalStore, MessageContent, MessageMeta, ReplyAttachment, ReplyRef, StoredMessage,
};
use crate::thumbnail::MAX_THUMBNAIL_BYTES;

/// Characters of a quoted message kept in a reply (the rest is in the original).
pub const REPLY_EXCERPT_CHARS: usize = 140;

/// A text message with everything the composer can set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutgoingText {
  pub maybe_thread: Option<ThreadId>,
  pub body: String,
  pub mentions: Vec<UserId>,
  pub meta: MessageMeta,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sends a text with metadata; returns its client message ID.
  pub async fn send(
    &mut self,
    conversation: ConversationId,
    text: OutgoingText,
  ) -> Result<ClientMessageId, CoreError> {
    let body = text.body.clone();
    let content = MessageContent::Text {
      body: text.body,
      mentions: text.mentions,
      meta: text.meta,
    };
    let id = self.enqueue(conversation, text.maybe_thread, content)?;
    let policy = self.workspace_policy()?.link_previews;
    if super::previews::queue_preview(&self.store, &policy, conversation, id, &body)? {
      self.generate_pending_previews().await?;
    }
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Pins or unpins a message for everyone: an encrypted pin message for the members, and the
  /// server's flag so pinned messages survive retention.
  pub async fn set_pinned(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  ) -> Result<(), CoreError> {
    let content = MessageContent::Pin { target, is_pinned };
    self.enqueue(conversation, None, content.clone())?;
    apply_control(
      &self.store,
      conversation,
      self.account.user_id,
      &content,
      self.now_ms(),
    )?;
    self.flush_outbox().await?;
    self.set_server_pin(conversation, target, is_pinned).await
  }

  /// The server's pin flag (retention). Queued: it follows once the device is online and the
  /// message has its server ID.
  pub(crate) async fn set_server_pin(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  ) -> Result<(), CoreError> {
    self
      .queue_action(PendingAction::ServerPin {
        conversation,
        target,
        is_pinned,
      })
      .await
  }

  /// Sends a copy of a message to another conversation with an optional comment. The copy is a new
  /// message, encrypted for the target; files go along (their keys travel inside the message).
  pub async fn forward(
    &mut self,
    from: ConversationId,
    target: ClientMessageId,
    to: ConversationId,
    comment: &str,
  ) -> Result<ClientMessageId, CoreError> {
    let original = self
      .store
      .message(from, target)?
      .ok_or_else(|| CoreError::Corrupt("forwarding a message that isn't stored".into()))?;
    let authorship = Authorship::of(&original, crate::imported::importer(&self.store));
    let author_name = match authorship.maybe_account() {
      Some(author) => self.display_name(author)?,
      None => imported_author_name(&authorship),
    };
    let maybe_conversation_name = self.conversation_name(from)?;
    let content = forward_content(
      &authorship,
      &original,
      ForwardScope::Message,
      author_name,
      maybe_conversation_name,
      comment,
    )
    .ok_or_else(|| CoreError::Corrupt("this kind of message can't be forwarded".into()))?;
    let id = self.enqueue(to, None, content)?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Confirms having read a message that asked for it; the sender sees it once the server has it
  /// (queued while offline).
  pub async fn acknowledge(
    &mut self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<(), CoreError> {
    read_state::note_acknowledgement(&self.store, conversation, message, self.account.user_id)?;
    self
      .queue_action(PendingAction::Acknowledge {
        conversation,
        message,
      })
      .await
  }

  /// Reads how far the other members of a DM or group DM read: receipts sent while this device was
  /// offline never arrive as events. Receipts only move forward, so a stale answer changes nothing.
  pub async fn refresh_read_receipts(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let response = self.api.read_receipts(conversation).await?;
    for receipt in response.receipts {
      read_state::note_read_receipt(&self.store, conversation, receipt.user_id, receipt.message_id)?;
    }
    Ok(())
  }

  /// Marks the conversation unread from `from` on (synced to the user's devices).
  pub fn mark_unread(&mut self, conversation: ConversationId, from: MessageId) -> Result<(), CoreError> {
    self.update_account_data(|data, now| {
      data.mark_unread(conversation, from, now);
      true
    })
  }

  fn display_name(&self, user: UserId) -> Result<String, CoreError> {
    Ok(
      crate::records::get_json::<teamcraft_proto::UserSummary>(
        &self.store,
        crate::store::Table::Users,
        &user.to_string(),
      )?
      .map(|summary| summary.display_name)
      .unwrap_or_default(),
    )
  }

  fn conversation_name(&self, conversation: ConversationId) -> Result<Option<String>, CoreError> {
    Ok(
      crate::records::get_json::<teamcraft_proto::ConversationResponse>(
        &self.store,
        crate::store::Table::Conversations,
        &conversation.to_string(),
      )?
      .and_then(|response| response.maybe_name),
    )
  }
}

/// The name a forward quotes for an imported author without an account: a bot's name in the old
/// system, nothing for a deleted account (shown like one).
pub fn imported_author_name(authorship: &Authorship<'_>) -> String {
  match authorship.maybe_origin.map(|origin| &origin.author) {
    Some(ImportedAuthor::Named { name }) => name.clone(),
    _ => String::new(),
  }
}

/// The reference a reply carries to the quoted message, naming the author the list shows (the
/// original one of an imported message, see [`Authorship`]). With `maybe_attachment` it quotes that
/// one picture of an album; a message with a single file is quoted whole, as is an attachment it
/// doesn't have (any more).
pub fn reply_ref(
  target: &StoredMessage,
  maybe_importer: Option<UserId>,
  maybe_attachment: Option<AttachmentId>,
) -> ReplyRef {
  let maybe_quoted = maybe_attachment
    .and_then(|id| album_attachment(target, id))
    .map(|attachment| Box::new(reply_attachment(attachment)));
  let text = match (&target.content, &maybe_quoted) {
    (MessageContent::Text { body, .. }, Some(quoted)) if body.trim().is_empty() => quoted.file_name.as_str(),
    (MessageContent::Text { body, .. }, _) => body.as_str(),
    (MessageContent::File { attachment }, _) => attachment.file_name.as_str(),
    _ => "",
  };
  ReplyRef {
    target: target.client_message_id,
    author: Authorship::of(target, maybe_importer).author,
    excerpt: text.chars().take(REPLY_EXCERPT_CHARS).collect(),
    maybe_attachment: maybe_quoted,
  }
}

/// `attachment` of a message that has several: only then it means one picture of an album.
pub fn album_attachment(message: &StoredMessage, attachment: AttachmentId) -> Option<&AttachmentRef> {
  match &message.content {
    MessageContent::Text { meta, .. } if meta.attachments.len() > 1 => meta
      .attachments
      .iter()
      .find(|candidate| candidate.attachment_id == attachment),
    _ => None,
  }
}

/// A thumbnail larger than what a sender makes is no thumbnail a quote copies: it would count
/// against the server's message limit twice.
fn reply_attachment(attachment: &AttachmentRef) -> ReplyAttachment {
  ReplyAttachment {
    attachment_id: attachment.attachment_id,
    file_name: attachment.file_name.clone(),
    maybe_thumbnail: attachment
      .maybe_thumbnail
      .clone()
      .filter(|thumbnail| thumbnail.as_slice().len() <= MAX_THUMBNAIL_BYTES),
  }
}

/// What of a message a forward sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardScope {
  Message,
  /// One file. Of a message with several, only this one and not the text, which may speak of the
  /// others; a message with just this file goes whole. Never all of them when it is gone.
  Attachment(AttachmentId),
}

/// The content of a forward of `scope` of `original`: the comment as text, the original quoted with
/// its author and time (`authorship`, of `original`), its files attached again. `None` for what
/// can't be forwarded (deleted messages, reactions and other control messages, a file the message
/// doesn't have).
pub fn forward_content(
  authorship: &Authorship<'_>,
  original: &StoredMessage,
  scope: ForwardScope,
  author_name: String,
  maybe_conversation_name: Option<String>,
  comment: &str,
) -> Option<MessageContent> {
  let (body, attachments) = forwarded_parts(original, scope)?;
  Some(MessageContent::Text {
    body: comment.to_string(),
    mentions: Vec::new(),
    meta: MessageMeta {
      maybe_forwarded: Some(Forwarded {
        author_name,
        maybe_conversation_name,
        sent_at_ms: authorship.sent_at_ms,
        body,
      }),
      attachments,
      ..MessageMeta::default()
    },
  })
}

/// The text and files a forward of `scope` of `original` carries.
fn forwarded_parts(original: &StoredMessage, scope: ForwardScope) -> Option<(String, Vec<AttachmentRef>)> {
  let (body, attachments) = match &original.content {
    MessageContent::Text { body, meta, .. } => (body, meta.attachments.as_slice()),
    MessageContent::File { attachment } => return forwarded_file(attachment, scope),
    _ => return None,
  };
  let ForwardScope::Attachment(id) = scope else {
    return Some((body.clone(), attachments.to_vec()));
  };
  let chosen = attachments
    .iter()
    .find(|attachment| attachment.attachment_id == id)?;
  match attachments.len() {
    1 => Some((body.clone(), attachments.to_vec())),
    _ => Some((String::new(), vec![chosen.clone()])),
  }
}

/// [`forwarded_parts`] of a `File` message, which is that one file.
fn forwarded_file(attachment: &AttachmentRef, scope: ForwardScope) -> Option<(String, Vec<AttachmentRef>)> {
  match scope {
    ForwardScope::Attachment(id) if id != attachment.attachment_id => None,
    _ => Some((String::new(), vec![attachment.clone()])),
  }
}

#[cfg(test)]
mod tests {
  use serde::Deserialize;
  use teamcraft_proto::Base64Bytes;

  use super::*;
  use crate::store::tests::{id, message};

  const IMPORTER: Option<UserId> = None;

  #[test]
  fn a_quoted_picture_has_a_stable_wire_form() {
    let reply = ReplyRef {
      target: ClientMessageId::from_bytes([1; 16]),
      author: id(7),
      excerpt: "c.jpg".into(),
      maybe_attachment: Some(Box::new(ReplyAttachment {
        attachment_id: id(3),
        file_name: "c.jpg".into(),
        maybe_thumbnail: Some(Base64Bytes::from(vec![1, 2, 3])),
      })),
    };
    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(
      wire["attachment"],
      serde_json::json!({ "attachment_id": "3", "file_name": "c.jpg", "thumbnail": "AQID" })
    );
    assert_eq!(serde_json::from_value::<ReplyRef>(wire).unwrap(), reply);
  }

  #[test]
  fn a_reply_to_a_whole_message_looks_as_it_always_did() {
    let wire = serde_json::to_value(reply_ref(&album("look"), IMPORTER, None)).unwrap();
    assert!(wire.get("attachment").is_none());
    let older: ReplyRef = serde_json::from_value(serde_json::json!({
      "target": wire["target"],
      "author": wire["author"],
      "excerpt": "look",
    }))
    .unwrap();
    assert_eq!(older.maybe_attachment, None);
  }

  /// `ReplyRef` of a client from before issue #241.
  #[derive(Deserialize)]
  struct OlderReplyRef {
    target: ClientMessageId,
    excerpt: String,
  }

  #[test]
  fn an_older_client_quotes_the_message_of_a_reply_to_one_picture() {
    let captioned = album("the site");
    let wire = serde_json::to_string(&reply_ref(&captioned, IMPORTER, Some(id(3)))).unwrap();
    let older: OlderReplyRef = serde_json::from_str(&wire).unwrap();
    assert_eq!(older.target, captioned.client_message_id);
    assert_eq!(older.excerpt, "the site");

    let wire = serde_json::to_string(&reply_ref(&album(""), IMPORTER, Some(id(3)))).unwrap();
    let older: OlderReplyRef = serde_json::from_str(&wire).unwrap();
    assert_eq!(
      older.excerpt, "c.jpg",
      "no text: the picture's name, not an empty quote"
    );
  }

  mod replies {
    use super::*;

    #[test]
    fn one_picture_of_an_album_is_quoted_with_a_copy_of_its_thumbnail_and_no_key() {
      let reply = reply_ref(&album("the site"), IMPORTER, Some(id(3)));
      let quoted = reply.maybe_attachment.unwrap();
      assert_eq!(quoted.attachment_id, id(3));
      assert_eq!(quoted.file_name, "c.jpg");
      assert_eq!(quoted.maybe_thumbnail, Some(Base64Bytes::from(vec![3])));
      let wire = serde_json::to_value(&quoted).unwrap();
      assert!(wire.get("key").is_none());
    }

    #[test]
    fn a_message_with_one_picture_is_quoted_as_today() {
      let single = with_attachments("the site", vec![picture(3)]);
      assert_eq!(reply_ref(&single, IMPORTER, Some(id(3))).maybe_attachment, None);
      let file = StoredMessage {
        content: MessageContent::File {
          attachment: picture(3),
        },
        ..single
      };
      let reply = reply_ref(&file, IMPORTER, Some(id(3)));
      assert_eq!(reply.maybe_attachment, None);
      assert_eq!(reply.excerpt, "c.jpg");
    }

    #[test]
    fn a_picture_that_is_gone_falls_back_to_quoting_the_message() {
      let reply = reply_ref(&album("the site"), IMPORTER, Some(id(99)));
      assert_eq!(reply.maybe_attachment, None);
      assert_eq!(reply.excerpt, "the site");
    }

    #[test]
    fn an_oversized_thumbnail_is_not_copied() {
      let mut huge = picture(3);
      huge.maybe_thumbnail = Some(Base64Bytes::from(vec![0; MAX_THUMBNAIL_BYTES + 1]));
      let message = with_attachments("", vec![picture(1), huge]);
      let quoted = reply_ref(&message, IMPORTER, Some(id(3)))
        .maybe_attachment
        .unwrap();
      assert_eq!(quoted.maybe_thumbnail, None);
    }
  }

  mod forwards {
    use super::*;

    #[test]
    fn one_picture_goes_alone_without_the_albums_text() {
      let (body, attachments) =
        forwarded(&album("the secret site"), ForwardScope::Attachment(id(3))).unwrap();
      assert_eq!(body, "");
      assert_eq!(ids(&attachments), [id(3)]);
    }

    #[test]
    fn a_whole_message_keeps_everything() {
      let (body, attachments) = forwarded(&album("the site"), ForwardScope::Message).unwrap();
      assert_eq!(body, "the site");
      assert_eq!(attachments.len(), 4);
    }

    #[test]
    fn a_picture_alone_in_its_message_goes_with_the_text_as_today() {
      let single = with_attachments("the site", vec![picture(3)]);
      let (body, attachments) = forwarded(&single, ForwardScope::Attachment(id(3))).unwrap();
      assert_eq!(body, "the site");
      assert_eq!(ids(&attachments), [id(3)]);
    }

    #[test]
    fn a_picture_that_is_gone_sends_nothing_rather_than_the_album() {
      assert_eq!(
        forwarded(&album("the site"), ForwardScope::Attachment(id(99))),
        None
      );
      let single = with_attachments("the site", vec![picture(3)]);
      assert_eq!(forwarded(&single, ForwardScope::Attachment(id(99))), None);
      let file = StoredMessage {
        content: MessageContent::File {
          attachment: picture(3),
        },
        ..single
      };
      assert_eq!(forwarded(&file, ForwardScope::Attachment(id(99))), None);
      let deleted = StoredMessage {
        content: MessageContent::Deleted,
        ..file
      };
      assert_eq!(forwarded(&deleted, ForwardScope::Attachment(id(3))), None);
    }

    fn forwarded(message: &StoredMessage, scope: ForwardScope) -> Option<(String, Vec<AttachmentRef>)> {
      let authorship = Authorship::of(message, IMPORTER);
      match forward_content(&authorship, message, scope, "Alice".into(), None, "")? {
        MessageContent::Text { meta, .. } => Some((meta.maybe_forwarded.unwrap().body, meta.attachments)),
        _ => panic!("a forward is a text message"),
      }
    }

    fn ids(attachments: &[AttachmentRef]) -> Vec<AttachmentId> {
      attachments
        .iter()
        .map(|attachment| attachment.attachment_id)
        .collect()
    }
  }

  fn album(body: &str) -> StoredMessage {
    with_attachments(body, (1..=4).map(picture).collect())
  }

  fn with_attachments(body: &str, attachments: Vec<AttachmentRef>) -> StoredMessage {
    StoredMessage {
      content: MessageContent::Text {
        body: body.into(),
        mentions: Vec::new(),
        meta: MessageMeta {
          attachments,
          ..MessageMeta::default()
        },
      },
      ..message(id(10), 1, Some(101), None, 5_000)
    }
  }

  /// `a.jpg`, `b.jpg`, `c.jpg`, … with the ID as its one-byte thumbnail.
  fn picture(number: i64) -> AttachmentRef {
    let letter = char::from(b'a' + u8::try_from(number - 1).unwrap());
    AttachmentRef {
      attachment_id: id(number),
      key: Base64Bytes::from(vec![9; 32]),
      ciphertext_sha256: Base64Bytes::from(vec![8; 32]),
      file_name: format!("{letter}.jpg"),
      media_type: "image/jpeg".into(),
      size_bytes: 10,
      maybe_thumbnail: Some(Base64Bytes::from(vec![u8::try_from(number).unwrap()])),
      maybe_dimensions: None,
      maybe_duration_ms: None,
    }
  }
}
