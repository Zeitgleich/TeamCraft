//! Runs the requests the UI adapter queued ([`CoreCommand`]) against the server.

use teamcraft_client_api::model::PresenceStatus;
use teamcraft_proto::{ManualPresence, SetPresenceRequest, UnixMillis, UpdateConversationMemberRequest};

use super::{Client, PendingAction};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageFilter, Table};
use crate::ui_backend::{CoreCommand, history_jump_key, newest_server_id};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub async fn run_ui_command(&mut self, command: CoreCommand) -> Result<(), CoreError> {
    match command {
      CoreCommand::FlushOutbox => self.flush_outbox().await.map(|_| ()),
      // Only the sync loop has the gateway to announce it on; without one there is no one to tell.
      CoreCommand::Typing { .. } => Ok(()),
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
        // The badge clears now; the server's `unread_update` confirms it.
        crate::read_state::note_unread(&self.store, conversation, 0, 0)?;
        let maybe_latest = newest_server_id(&self.store.messages(conversation, MessageFilter::All)?);
        match maybe_latest {
          // Queued: offline, it reaches the server (and the other devices) once back.
          Some(message) => {
            self
              .queue_action(PendingAction::ReadPosition {
                conversation,
                message,
              })
              .await
          },
          None => Ok(()),
        }
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
      CoreCommand::SetPresence {
        presence,
        maybe_until,
      } => {
        let manual = match presence {
          PresenceStatus::Online => ManualPresence::Auto,
          PresenceStatus::Away => ManualPresence::Away,
          PresenceStatus::DoNotDisturb => ManualPresence::DoNotDisturb,
          PresenceStatus::Offline => ManualPresence::Invisible,
        };
        let request = SetPresenceRequest {
          // Only do-not-disturb ends by itself.
          maybe_until: maybe_until
            .filter(|_| manual == ManualPresence::DoNotDisturb)
            .map(UnixMillis),
          manual,
        };
        self.queue_action(PendingAction::Presence { request }).await
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
      CoreCommand::AddOrgBackground { name, picture } => {
        self.add_org_background(name, picture).await.map(|_| ())
      },
      CoreCommand::DeleteOrgBackground { id } => self.delete_org_background(id).await,
      CoreCommand::SetOrgBackgroundRule { rule } => {
        self
          .set_org_background_rule(rule.mode, rule.maybe_default, rule.guests_exempt, rule.order)
          .await
      },
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
      CoreCommand::MarkReadThrough {
        conversation,
        message,
      } => {
        // Queued: offline, it reaches the server (and the other devices) once back.
        self
          .queue_action(PendingAction::ReadPosition {
            conversation,
            message,
          })
          .await
      },
      CoreCommand::MarkThreadRead {
        conversation,
        thread,
        message,
      } => {
        self
          .queue_action(PendingAction::ThreadReadPosition {
            conversation,
            thread,
            message,
          })
          .await
      },
      CoreCommand::RefreshPresence { users } => self.refresh_presence(&users).await,
      CoreCommand::RefreshReadReceipts { conversation } => self.refresh_read_receipts(conversation).await,
      CoreCommand::SearchDirectory { query } => self.search_directory(&query).await.map(|_| ()),
      CoreCommand::RefreshPrivacy => self.refresh_privacy().await.map(|_| ()),
      CoreCommand::SetLastSeenVisibility { visibility } => self.set_last_seen_visibility(visibility).await,
      CoreCommand::RefreshCallHistory => self.refresh_call_history().await,
      CoreCommand::RefreshOwnDevices => self.refresh_own_devices().await,
      CoreCommand::RevokeOwnDevice { device } => {
        self.revoke_device(device).await?;
        self.refresh_own_devices().await
      },
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
        maybe_expires_in_seconds,
        maybe_max_downloads,
        maybe_password,
      } => {
        let settings = crate::client::files::ShareLinkSettings {
          maybe_expires_in_seconds,
          maybe_max_downloads,
          maybe_password: maybe_password.map(|mut password| {
            // The text is wiped when `password` drops; the secret owns the bytes from here.
            teamcraft_crypto::secret::SecretBytes::new(std::mem::take(&mut password.0).into_bytes())
          }),
        };
        self
          .create_share_link(conversation, attachment, settings)
          .await
          .map(|_| ())
      },
      CoreCommand::RevokeShareLink {
        attachment, share_id, ..
      } => self.revoke_share_link(attachment, &share_id).await,
      CoreCommand::RefreshShareLinks { attachment } => self.refresh_share_links(attachment).await,
      CoreCommand::CreateSpace { request } => self.create_space(&request).await.map(|_| ()),
      CoreCommand::UpdateSpace { space, request } => self.update_space(space, &request).await.map(|_| ()),
      CoreCommand::CreateChannelWith { space, options } => {
        self.create_channel_with(space, *options).await.map(|_| ())
      },
      CoreCommand::UpdateChannel {
        conversation,
        request,
      } => self.update_channel(conversation, &request).await,
      CoreCommand::MoveChannel {
        conversation,
        request,
      } => self.move_channel(conversation, &request).await,
      CoreCommand::RefreshChannelBrowser { space } => self.refresh_channel_browser(space).await.map(|_| ()),
      CoreCommand::JoinChannel { conversation } => self.join_channel(conversation).await,
      CoreCommand::RequestAdminJoin { conversation } => self.request_admin_join(conversation).await,
      CoreCommand::WithdrawAdminJoin { conversation } => self.withdraw_admin_join(conversation).await,
      CoreCommand::ApproveAdminJoin {
        conversation,
        user,
        history,
      } => self.approve_admin_join_sharing(conversation, user, history).await,
      CoreCommand::DeclineAdminJoin { conversation, user } => {
        self.decline_admin_join(conversation, user).await
      },
      CoreCommand::JoinSpace { space } => self.join_space(space).await,
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
      CoreCommand::RetryRemoval { conversation, user } => self.retry_removal(conversation, user).await,
      CoreCommand::RetryDmSetup { conversation } => self.retry_dm_setup(conversation).await,
      CoreCommand::SetUpMeetingChat {
        conversation,
        invitees,
      } => self
        .set_up_meeting_chat(conversation, &invitees)
        .await
        .map(|_| ()),
      CoreCommand::UpdateMeetingChatMembers {
        conversation,
        invitees,
        removed,
      } => self
        .update_meeting_chat_members(conversation, &invitees, &removed)
        .await
        .map(|_| ()),
      CoreCommand::AddPeopleToCall {
        from,
        in_call,
        added,
        maybe_title,
        key,
      } => self
        .add_people_to_call(from, &in_call, &added, maybe_title, key)
        .await
        .map(|_| ()),
      CoreCommand::AcceptCallMove { from, to } => self.accept_call_move(from, to).await,
      CoreCommand::LetIntoMeetingChat { conversation, user } => {
        self.let_into_meeting_chat(conversation, user).await
      },
      CoreCommand::PrepareGuestLink {
        conversation,
        meeting,
      } => self.prepare_guest_link(conversation, meeting).await,
      CoreCommand::LetGuestIntoMeetingChat {
        conversation,
        meeting,
        guest,
        display_name,
        maybe_proof,
        access_ends_at_ms,
      } => {
        self
          .admit_guest(
            conversation,
            meeting,
            guest,
            &display_name,
            maybe_proof,
            access_ends_at_ms,
          )
          .await
      },
      CoreCommand::DiscardRemoval { conversation, user } => self.discard_removal(conversation, user).await,
      CoreCommand::InviteToSpace {
        space,
        email,
        is_guest,
        channels,
        maybe_expires_at,
        maybe_message,
      } => {
        self
          .invite_to_space(space, &email, is_guest, channels, maybe_expires_at, maybe_message)
          .await
      },
      CoreCommand::AddPeopleToChannel {
        conversation,
        people,
        history,
        joins_space,
      } => {
        self
          .add_people_to_channel(conversation, &people, history, joins_space)
          .await
      },
      CoreCommand::RefreshSpaceRoster { space } => {
        self.refresh_space_roster(space).await?;
        self.refresh_space_invites(space).await
      },
      CoreCommand::SetSpaceRole { space, user, role } => self.set_space_role(space, user, role).await,
      CoreCommand::SetChannelRole {
        conversation,
        user,
        role,
      } => self.set_channel_role(conversation, user, role).await,
      CoreCommand::RemoveSpaceMember { space, user } => self.remove_space_member(space, user).await,
      CoreCommand::RevokeSpaceInvite { space, invite } => self.revoke_space_invite(space, invite).await,
      CoreCommand::AddSpaceMembers {
        space,
        users,
        is_guest,
      } => self.add_space_members(space, &users, is_guest).await,
      CoreCommand::SetCustomStatus { maybe_status } => {
        let now = self.now_ms();
        self
          .set_custom_status(maybe_status.and_then(|status| status.to_wire(now)))
          .await
      },
    }
  }
}
