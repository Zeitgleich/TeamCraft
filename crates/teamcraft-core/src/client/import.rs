//! The import account's side of an import from another chat (e2ee.md §4.10, admin.md §3.8): its
//! messages are encrypted like any send, with the origin inside the ciphertext, and stored by the
//! server below the conversation's live messages. Used by `teamcraft-import`.

use rand_core::OsRng;
use teamcraft_crypto::message::encrypt_message;
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationResponse, Id, ImportDmRequest, ImportMemberStateRequest,
  ImportMessagesRequest, ImportedMessage, MessageId, ProvisionUsersRequest, ProvisionedUser, ThreadId,
  UserId,
};
use zeroize::Zeroizing;

use super::Client;
use super::server_changes::Observed;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::imported::imported_message_id;
use crate::store::{LocalStore, MessageContent};

/// One message to import: its text with [`crate::store::MessageMeta::imported`] set, and the thread it answers.
#[derive(Clone, Debug)]
pub struct ImportDraft {
  pub content: MessageContent,
  /// The server ID of the imported thread root this message answers.
  pub maybe_thread_root: Option<MessageId>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Creates the accounts of `usernames` from the identity provider (admin.md §3.8).
  pub async fn provision_users(&self, usernames: Vec<String>) -> Result<Vec<ProvisionedUser>, CoreError> {
    let response = self
      .api
      .provision_users(&ProvisionUsersRequest { usernames })
      .await?;
    Ok(response.users)
  }

  /// The DM of `participants` (two people), or one person's notes, with this account as a member
  /// that is no participant. Stored like any conversation this device learns of.
  pub async fn open_import_dm(
    &mut self,
    participants: Vec<UserId>,
  ) -> Result<ConversationResponse, CoreError> {
    let response = self.api.import_dm(&ImportDmRequest { participants }).await?;
    self.store_conversation(&response, Observed::FromServer { maybe_head: None })?;
    Ok(response)
  }

  /// Sets up the encryption group of an import DM ([`Self::open_import_dm`]): founded by this
  /// device with the participants, who get the full history, unless somebody founded it already.
  /// A participant without a device yet waits as a pending addition (e2ee.md §4.3). `true` once
  /// this device is in the group.
  pub async fn set_up_import_dm(
    &mut self,
    conversation: ConversationResponse,
    participants: &[UserId],
  ) -> Result<bool, CoreError> {
    let id = self.found_participants_group(conversation, participants).await?;
    self.has_group(id)
  }

  /// Encrypts `drafts` with the conversation's current content key and stores them as imported
  /// messages, oldest first; returns their server IDs in order. A draft sent before keeps its ID.
  /// Nothing is stored locally: the import device never reads its own history.
  pub async fn send_imported(
    &mut self,
    conversation: ConversationId,
    drafts: &[ImportDraft],
  ) -> Result<Vec<MessageId>, CoreError> {
    self.ensure_mls_current(conversation).await?;
    let Some(ring) = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .filter(|ring| ring.current().is_some())
    else {
      return Err(CoreError::InvalidRequest(
        "no content key for this conversation yet".into(),
      ));
    };
    let mut messages = Vec::with_capacity(drafts.len());
    for draft in drafts {
      let MessageContent::Text { meta, .. } = &draft.content else {
        return Err(CoreError::InvalidRequest(
          "only text messages are imported".into(),
        ));
      };
      let Some(origin) = &meta.maybe_imported else {
        return Err(CoreError::InvalidRequest(
          "an imported message needs its origin".into(),
        ));
      };
      let client_message_id = imported_message_id(conversation, origin.source, &origin.source_id);
      let plaintext = Zeroizing::new(
        serde_json::to_vec(&draft.content).map_err(|error| CoreError::Corrupt(error.to_string()))?,
      );
      let context = self.message_context(conversation, client_message_id);
      let encrypted = encrypt_message(ring, &context, &plaintext, &self.account.device, &mut OsRng)?;
      messages.push(ImportedMessage {
        client_message_id,
        content_key_id: Base64Bytes::new(encrypted.content_key_id().as_bytes().to_vec()),
        ciphertext: Base64Bytes::new(encrypted.to_bytes()?),
        maybe_thread_id: draft
          .maybe_thread_root
          .map(|root| ThreadId::from_snowflake(root.snowflake())),
        sent_at_ms: origin.sent_at_ms,
      });
    }
    let response = self
      .api
      .import_messages(
        conversation,
        &ImportMessagesRequest {
          device_id: self.account.device_id,
          messages,
        },
      )
      .await?;
    Ok(response.messages.into_iter().map(|stored| stored.id).collect())
  }

  /// A member's read position, favourite and mute from the old system.
  pub async fn set_import_member_state(
    &self,
    conversation: ConversationId,
    user: UserId,
    state: &ImportMemberStateRequest,
  ) -> Result<(), CoreError> {
    Ok(
      self
        .api
        .set_import_member_state(conversation, user, state)
        .await?,
    )
  }

  /// Marks the conversation's import as finished, so its members load its history's start again.
  pub async fn finish_import(&self, conversation: ConversationId) -> Result<(), CoreError> {
    Ok(self.api.finish_import(conversation).await?)
  }

  /// Leaves a conversation the import is done with, without a goodbye: a signed removal of this
  /// user that a member commits, then the server's membership (also of a DM or notes, e2ee.md
  /// §4.10). `Ok` while it waits for the members' commit; the pending queue finishes it.
  pub async fn leave_after_import(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.account.user_id;
    self.api.check_member_removal(conversation, me).await?;
    self.record_and_run_removal(conversation, me, None).await
  }
}
