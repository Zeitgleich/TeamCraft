//! Runs the requests the UI adapter queued ([`CoreCommand`]) against the server.

use teamcraft_client_api::model::PresenceStatus;
use teamcraft_proto::{
  ManualPresence, SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageFilter, Table};
use crate::ui_backend::{CoreCommand, history_jump_key};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub async fn run_ui_command(&mut self, command: CoreCommand) -> Result<(), CoreError> {
    match command {
      CoreCommand::FlushOutbox => self.flush_outbox().await.map(|_| ()),
      CoreCommand::LoadHistoryAt { conversation, at_ms } => {
        let loaded = self.load_messages_from(conversation, at_ms).await;
        // Answered even when loading failed (offline): the UI then shows what is local.
        records::put_json(
          &self.store,
          Table::SyncState,
          &history_jump_key(conversation),
          &at_ms,
        )?;
        loaded.map(|_| ())
      },
      CoreCommand::MarkRead { conversation } => {
        self.update_account_data(|data, now| data.clear_unread_mark(conversation, now))?;
        let maybe_latest = self
          .store
          .messages(conversation, MessageFilter::All)?
          .into_iter()
          .filter_map(|message| message.maybe_server_id)
          .max();
        if let Some(message_id) = maybe_latest {
          self
            .api
            .set_read_position(conversation, &SetReadPositionRequest { message_id })
            .await?;
          self.refresh_conversation(conversation).await?;
        }
        Ok(())
      },
      CoreCommand::SetFavorite {
        conversation,
        is_favorite,
      } => {
        let request = UpdateConversationMemberRequest {
          maybe_is_favorite: Some(is_favorite),
          ..Default::default()
        };
        self
          .api
          .update_member(conversation, self.account.user_id, &request)
          .await?;
        self.refresh_conversation(conversation).await
      },
      CoreCommand::SetPresence { presence } => {
        let manual = match presence {
          PresenceStatus::Online | PresenceStatus::Away => ManualPresence::Auto,
          PresenceStatus::DoNotDisturb => ManualPresence::DoNotDisturb,
          PresenceStatus::Offline => ManualPresence::Invisible,
        };
        Ok(
          self
            .api
            .set_presence(&SetPresenceRequest {
              manual,
              maybe_until: None,
            })
            .await?,
        )
      },
      #[cfg(not(target_arch = "wasm32"))]
      CoreCommand::FetchSearchHits { hits } => self.fetch_search_hits(&hits).await.map(|_| ()),
      CoreCommand::SetSaved {
        conversation,
        client_message_id,
        is_saved,
      } => self.update_account_data(|data, now| {
        data.set_saved(conversation, client_message_id, is_saved, now);
        true
      }),
      CoreCommand::SetDraft {
        conversation,
        maybe_thread,
        body,
      } => self.update_account_data(|data, now| data.set_draft(conversation, maybe_thread, &body, now)),
      CoreCommand::SaveSearch { id, name, query } => self.update_account_data(|data, now| {
        data.save_search(id, &name, &query, now);
        true
      }),
      CoreCommand::DeleteSavedSearch { id } => self.update_account_data(|data, now| {
        data.delete_saved_search(id, now);
        true
      }),
      CoreCommand::AddBackground { name, picture } => self.add_background(name, picture).await.map(|_| ()),
      CoreCommand::DeleteBackground { id } => self.delete_background(id).await,
      CoreCommand::SetServerPin {
        conversation,
        target,
        is_pinned,
      } => self.set_server_pin(conversation, target, is_pinned).await,
      CoreCommand::Acknowledge {
        conversation,
        message,
      } => self.acknowledge(conversation, message).await,
      CoreCommand::MarkUnread { conversation, from } => self.mark_unread(conversation, from),
      CoreCommand::UploadFiles => self.upload_pending().await.map(|_| ()),
      CoreCommand::GeneratePreviews => self.generate_pending_previews().await.map(|_| ()),
      CoreCommand::DownloadFile {
        conversation,
        attachment,
      } => self
        .download_attachment_to_disk(conversation, attachment)
        .await
        .map(|_| ()),
      CoreCommand::CreateShareLink {
        conversation,
        attachment,
        maybe_expires_at,
        maybe_max_downloads,
        maybe_password,
      } => {
        let settings = crate::client::files::ShareLinkSettings {
          maybe_expires_at,
          maybe_max_downloads,
          maybe_password: maybe_password
            .map(|password| teamcraft_crypto::secret::SecretBytes::new(password.0.into_bytes())),
        };
        self
          .create_share_link(conversation, attachment, settings)
          .await
          .map(|_| ())
      },
      CoreCommand::CreateSpace { request } => self.create_space(&request).await.map(|_| ()),
      CoreCommand::UpdateSpace { space, request } => self.update_space(space, &request).await.map(|_| ()),
      CoreCommand::CreateChannelWith { space, options } => {
        self.create_channel_with(space, *options).await.map(|_| ())
      },
      CoreCommand::UpdateChannel {
        conversation,
        request,
      } => self.update_channel(conversation, &request).await,
      CoreCommand::RefreshChannelBrowser { space } => self.refresh_channel_browser(space).await.map(|_| ()),
      CoreCommand::JoinChannel { conversation } => self.join_channel(conversation).await,
      CoreCommand::LeaveChannel { conversation } => self.leave_channel(conversation).await,
      CoreCommand::RefreshCategories { space } => self.refresh_categories(space).await.map(|_| ()),
      CoreCommand::CreateCategory { space, name } => self.create_category(space, &name).await,
      CoreCommand::RenameCategory {
        space,
        category,
        name,
      } => self.rename_category(space, category, &name).await,
      CoreCommand::DeleteCategory { space, category } => self.delete_category(space, category).await,
      CoreCommand::AddMemberWithHistory {
        conversation,
        user,
        history,
      } => {
        self
          .add_member_with_history(conversation, user, Some(history))
          .await
      },
      CoreCommand::RemoveMember { conversation, user } => self.remove_member(conversation, user).await,
      CoreCommand::InviteToSpace {
        space,
        email,
        is_guest,
        channels,
        maybe_expires_at,
      } => {
        self
          .invite_to_space(space, &email, is_guest, channels, maybe_expires_at)
          .await
      },
      CoreCommand::SetCustomStatus { .. } => {
        // The server has no custom status yet (crate README: server gaps).
        tracing::debug!("custom status is not stored on the server yet");
        Ok(())
      },
    }
  }
}
