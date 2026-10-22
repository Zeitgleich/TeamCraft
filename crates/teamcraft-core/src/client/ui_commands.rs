//! Runs the requests the UI adapter queued ([`CoreCommand`]) against the server.

use teamcraft_proto::{
  ManualPresence, SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest,
};
use teamcraft_ui::model::PresenceStatus;

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::{LocalStore, MessageFilter};
use crate::ui_backend::CoreCommand;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub async fn run_ui_command(&mut self, command: CoreCommand) -> Result<(), CoreError> {
    match command {
      CoreCommand::FlushOutbox => self.flush_outbox().await.map(|_| ()),
      CoreCommand::MarkRead { conversation } => {
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
      CoreCommand::SetCustomStatus { .. } => {
        // The server has no custom status yet (crate README: server gaps).
        tracing::debug!("custom status is not stored on the server yet");
        Ok(())
      },
    }
  }
}
