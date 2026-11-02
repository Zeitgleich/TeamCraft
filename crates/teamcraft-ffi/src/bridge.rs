//! The JSON bridge to the chat backend: the same `ChatAction` / `ChatQuery` / `ChatSnapshot`
//! messages the desktop shell exchanges with its webview (teamcraft-client-api `remote`), so the
//! mobile apps reuse the core's chat logic instead of growing their own. The app mirrors the view
//! models as plain data classes.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::model::User;
use teamcraft_client_api::remote::{
  ChatAction, ChatQuery, ChatSnapshot, ContactKeys, MessageTail, MessageWindow, SecuritySnapshot,
};
use teamcraft_core::api::ServerApi as _;
use teamcraft_core::records;
use teamcraft_core::store::LocalStore;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::org_recovery::RecoveryRequestKey;
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, Base64Bytes, CreateRecoveryRequest, DeviceActivity, ErrorCode,
  RecoveryRequestId, RecoveryRequestResponse, RecoveryResultPayload,
};

use crate::engine::{ClientSlot, Engine, feed_anchor, older_len, parse_id};
use crate::error::TeamCraftError;
use crate::types::{AccountState, OrgRecoveryStart};

/// A recovery this (new) device asked the organization for (e2ee.md §6.3.1).
pub(crate) struct PendingRecovery {
  key: RecoveryRequestKey,
  request_id: RecoveryRequestId,
  user: teamcraft_proto::UserId,
}

/// The [`ChatSnapshot`] as the apps read it. Its `users` are the people to offer in lists, pickers
/// and mentions; `former_users` are the deactivated and deleted accounts, offered nowhere but still
/// naming their old messages. The apps show a deleted one as their own "Deleted user", as desktop
/// and web do (issue #751).
#[derive(serde::Serialize)]
struct AppSnapshot {
  #[serde(flatten)]
  snapshot: ChatSnapshot,
  former_users: Vec<User>,
}

impl AppSnapshot {
  fn read<S: LocalStore>(backend: &CoreChatBackend<S>, security: SecuritySnapshot) -> Self {
    Self {
      snapshot: ChatSnapshot::read(backend, security),
      former_users: backend.former_users(),
    }
  }
}

impl Engine {
  pub(crate) fn snapshot_json(&self) -> Result<String, TeamCraftError> {
    to_json(&AppSnapshot::read(self.backend()?, self.security_snapshot()?))
  }

  pub(crate) fn window_json(
    &self,
    conversation_id: &str,
    start: u32,
    len: u32,
  ) -> Result<String, TeamCraftError> {
    let conversation = parse_id(conversation_id)?;
    // The next catch-up starts with the conversation the user opened (issue #972).
    self.signals.catch_up_focus.set(conversation);
    let window = MessageWindow::read(
      self.backend()?,
      conversation,
      usize::try_from(start).unwrap_or(usize::MAX),
      usize::try_from(len).unwrap_or(usize::MAX),
    );
    to_json(&window)
  }

  pub(crate) fn tail_json(
    &self,
    conversation_id: &str,
    maybe_oldest_id: Option<&str>,
    oldest_sent_at_ms: i64,
    older: u32,
  ) -> Result<String, TeamCraftError> {
    let conversation = parse_id(conversation_id)?;
    self.signals.catch_up_focus.set(conversation);
    let backend = self.backend()?;
    let tail = backend.feed_tail(
      conversation,
      feed_anchor(maybe_oldest_id, oldest_sent_at_ms)?,
      older_len(older),
    );
    to_json(&MessageTail {
      conversation,
      messages: tail.messages,
      has_older: tail.has_older,
      undecrypted: backend.undecrypted_count(conversation),
    })
  }

  pub(crate) fn query_json(&self, query: &str) -> Result<String, TeamCraftError> {
    let query: ChatQuery = from_json(query)?;
    to_json(&query.answer(self.backend()?))
  }

  /// Runs an action. Actions that change encryption groups or keys need the client; admin
  /// actions stay on desktop and web.
  pub(crate) async fn act(&mut self, action: &str) -> Result<(), TeamCraftError> {
    let action: ChatAction = from_json(action)?;
    match action {
      ChatAction::CreateChannel { space, name } => {
        self
          .with_client(|client| {
            Box::pin(async move { client.create_channel(space, &name).await.map(|_| ()) })
          })
          .await
      },
      ChatAction::OpenDm { user } => {
        self
          .with_client(|client| Box::pin(async move { client.open_dm(user).await.map(|_| ()) }))
          .await
      },
      ChatAction::OpenGroupDm { users } => {
        self
          .with_client(|client| Box::pin(async move { client.open_group_dm(&users).await.map(|_| ()) }))
          .await
      },
      ChatAction::AddMember { conversation, user } => {
        self
          .with_client(|client| Box::pin(async move { client.add_member(conversation, user).await }))
          .await
      },
      ChatAction::ChangePassphrase { passphrase } => {
        let passphrase = teamcraft_crypto::secret::SecretBytes::new(passphrase.into_bytes());
        self
          .with_client(|client| {
            Box::pin(async move {
              client
                .upload_key_bag_with(&passphrase, teamcraft_crypto::backup::Argon2Settings::default())
                .await
            })
          })
          .await
      },
      ChatAction::AcceptKeyChange { user } => {
        self
          .with_client(|client| Box::pin(async move { client.accept_identity_change(user) }))
          .await
      },
      // Unthrottled; the apps report through `TeamCraft::report_activity`, which repeats input at
      // most every 30 s.
      ChatAction::SetActivity { is_idle } => {
        self.report_activity(if is_idle {
          DeviceActivity::Idle
        } else {
          DeviceActivity::Active
        });
        Ok(())
      },
      // Attention holds phone pushes back while a desktop or web session is looked at; a phone
      // reports none.
      ChatAction::SetAttention { .. } => Ok(()),
      ChatAction::PublishPolicy { .. }
      | ChatAction::PublishRoster { .. }
      | ChatAction::AdminJoinChannel { .. }
      | ChatAction::AdminRemoveFromAllChats { .. }
      | ChatAction::AdminDeleteUser { .. }
      | ChatAction::AdminRemoveGroupLeftover { .. } => Err(TeamCraftError::InvalidInput(
        "workspace administration is on desktop and web".into(),
      )),
      other => {
        if let Some(unhandled) = other.apply_to(self.backend()?) {
          return Err(TeamCraftError::InvalidInput(format!(
            "unsupported action: {unhandled:?}"
          )));
        }
        self.dispatch_commands().await
      },
    }
  }

  /// An authenticated call of a plain server endpoint (notification settings, meetings).
  pub(crate) async fn api_request(
    &self,
    method: &str,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<String, TeamCraftError> {
    Ok(
      self
        .api()
        .raw_request(method, path, maybe_body)
        .await
        .map_err(teamcraft_core::CoreError::from)?,
    )
  }

  /// The profile picture of `user` as image bytes (JPEG, PNG or WebP); `None` if the person has
  /// none. The API names a presigned URL (profile pictures are not secret from members) and the
  /// download is capped at `max_bytes`, so a hostile answer can't make the phone buffer a huge body.
  pub(crate) async fn avatar_picture(
    &self,
    user: &str,
    max_bytes: u64,
  ) -> Result<Option<Vec<u8>>, TeamCraftError> {
    let user: teamcraft_proto::UserId = parse_id(user)?;
    let api = self.api();
    let answer = match api
      .raw_request("GET", &format!("/users/{user}/avatar"), None)
      .await
    {
      Ok(answer) => answer,
      Err(error) if error.error_code() == Some(&ErrorCode::AvatarNotFound) => return Ok(None),
      Err(error) => return Err(teamcraft_core::CoreError::from(error).into()),
    };
    // Plain http only for a development server on this computer, which is plain http itself.
    let (url, size_bytes) = avatar_location(&answer, self.server_url.starts_with("http://"))?;
    let picture = api
      .get_blob_capped(&url, size_bytes.min(max_bytes))
      .await
      .map_err(teamcraft_core::CoreError::from)?;
    Ok(Some(picture))
  }

  /// New device without the passphrase: asks the organization's admins to recover the account.
  /// Returns the code the user reads to an admin.
  pub(crate) async fn start_org_recovery(&mut self) -> Result<OrgRecoveryStart, TeamCraftError> {
    use teamcraft_core::api::ServerApi as _;
    let api = self.api();
    let me = api.me().await.map_err(teamcraft_core::CoreError::from)?;
    let key = RecoveryRequestKey::generate(&mut rand_core::OsRng);
    let body = serde_json::to_string(&CreateRecoveryRequest {
      request_public_key: Base64Bytes::new(key.public_key().as_bytes().to_vec()),
    })
    .map_err(|error| TeamCraftError::Core(error.to_string()))?;
    let reply: RecoveryRequestResponse = from_json(
      &self
        .api_request("POST", "/org-recovery/requests", Some(body))
        .await?,
    )?;
    let request = teamcraft_core::client::recovery_request(reply.id, me.id, key.public_key());
    let start = OrgRecoveryStart {
      request_id: reply.id.to_string(),
      verification_code: request.verification_code(),
    };
    self.maybe_recovery = Some(PendingRecovery {
      key,
      request_id: reply.id,
      user: me.id,
    });
    Ok(start)
  }

  /// Once the admins answered: opens the backup key, enrolls this device and switches the account
  /// to a new backup key wrapped with `new_passphrase`. Fails with `RecoveryPending` before that.
  pub(crate) async fn finish_org_recovery(
    &mut self,
    device_name: String,
    new_passphrase: String,
  ) -> Result<AccountState, TeamCraftError> {
    let pending = self
      .maybe_recovery
      .as_ref()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let path = format!("/org-recovery/requests/{}/result", pending.request_id);
    let result: RecoveryResultPayload = match self.api_request("GET", &path, None).await {
      Ok(text) => from_json(&text)?,
      Err(TeamCraftError::Core(_)) | Err(TeamCraftError::Network(_)) => {
        return Err(TeamCraftError::RecoveryPending);
      },
      Err(other) => return Err(other),
    };
    let pending = self
      .maybe_recovery
      .take()
      .ok_or(TeamCraftError::NoLinkInProgress)?;
    let request =
      teamcraft_core::client::recovery_request(pending.request_id, pending.user, pending.key.public_key());
    let options = teamcraft_core::EnrollmentOptions {
      device_name,
      secret: teamcraft_core::EnrollmentSecret::OrgRecovery {
        unlock: teamcraft_core::OrgRecoveryUnlock {
          key: pending.key,
          request,
          sealed_backup_key: result.sealed_backup_key.into_vec(),
        },
        new_passphrase: teamcraft_crypto::secret::SecretBytes::new(new_passphrase.into_bytes()),
      },
      argon2: teamcraft_crypto::backup::Argon2Settings::default(),
      maybe_attestation: None,
    };
    self.complete_enrollment(options).await
  }

  /// Fingerprints of the identity keys this device pinned, for key verification in the app.
  fn security_snapshot(&self) -> Result<SecuritySnapshot, TeamCraftError> {
    if matches!(self.client, ClientSlot::Missing) {
      return Err(TeamCraftError::NotEnrolled);
    }
    let fingerprint = |key: [u8; 32]| IdentityPublicKey::from_bytes(key).fingerprint().to_string();
    let contacts = records::pinned_identities(&self.store)?
      .into_iter()
      .map(|(user, pinned)| ContactKeys {
        user,
        fingerprint: fingerprint(pinned.public_key),
        maybe_new_fingerprint: pinned.maybe_changed_to.map(fingerprint),
        is_attested: pinned.is_attested,
      })
      .collect();
    Ok(SecuritySnapshot {
      my_fingerprint: self.identity_fingerprint.clone(),
      contacts,
      maybe_push_content: Some(teamcraft_core::push_preview::push_content_mode(&self.store)),
      maybe_owners: {
        let settings = teamcraft_core::ClientSettings::default();
        teamcraft_core::workspace::owner_roster_status(
          &self.store,
          settings.workspace_max_age,
          settings.workspace_expiry_warning,
        )
      },
      maybe_presence_policy: teamcraft_core::workspace::presence_policy(&self.store),
      maybe_screen_share_policy: teamcraft_core::workspace::screen_share_policy(&self.store),
      maybe_notetaker: teamcraft_core::notetaker::notetaker_policy(&self.store),
      maybe_backup: teamcraft_core::ui_snapshot::key_backup_state(&self.store).unwrap_or_else(|error| {
        // The app then shows the backup as unknown; the rest of the snapshot still counts.
        tracing::warn!(%error, "the key backup state is unreadable");
        None
      }),
    })
  }
}

fn to_json(value: &impl serde::Serialize) -> Result<String, TeamCraftError> {
  serde_json::to_string(value).map_err(|error| TeamCraftError::Core(error.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TeamCraftError> {
  serde_json::from_str(text).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))
}

/// The download URL and size of `GET /users/{id}/avatar`'s answer; refuses a type that is not a
/// picture and a URL that is not `https` (`http` only if `is_plain_http_allowed`).
fn avatar_location(answer: &str, is_plain_http_allowed: bool) -> Result<(String, u64), TeamCraftError> {
  let location: AvatarResponse =
    serde_json::from_str(answer).map_err(|error| TeamCraftError::Core(error.to_string()))?;
  if let AvatarContentType::Unknown(other) = &location.content_type {
    return Err(TeamCraftError::Core(format!("{other:?} is not a picture type")));
  }
  let is_secure = location.download_url.starts_with("https://");
  if !is_secure && !(is_plain_http_allowed && location.download_url.starts_with("http://")) {
    return Err(TeamCraftError::Core(
      "the picture is not at a secure web address".into(),
    ));
  }
  Ok((location.download_url, location.size_bytes))
}

#[cfg(test)]
mod snapshot_tests {
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_core::clock::ManualClock;
  use teamcraft_core::store::{MemoryStore, Table};
  use teamcraft_proto::{UserKind, UserSummary};

  use super::*;

  #[test]
  fn deactivated_and_deleted_accounts_only_name_their_old_messages() {
    let store = MemoryStore::new();
    for summary in [
      summary(1, "Alex Example", false, false),
      summary(2, "Sam Doe", false, false),
      summary(3, "Former Colleague", true, false),
      summary(4, "Deleted user", true, true),
    ] {
      records::put_json(&store, Table::Users, &summary.id.to_string(), &summary).unwrap();
    }
    let backend = CoreChatBackend::new(store, id_from_raw(1), Box::new(ManualClock::default()));

    let json = serde_json::to_value(AppSnapshot::read(&backend, SecuritySnapshot::default())).unwrap();

    assert_eq!(
      json["me"]["id"], "1",
      "the snapshot's own fields stay at the top level"
    );
    assert_eq!(ids(&json["users"]), ["1", "2"]);
    assert_eq!(ids(&json["former_users"]), ["3", "4"]);
    assert_eq!(json["former_users"][0]["is_deactivated"], true);
    assert!(json["former_users"][0].get("is_deleted").is_none());
    assert_eq!(json["former_users"][1]["is_deleted"], true);
  }

  fn summary(id: u64, display_name: &str, is_deactivated: bool, is_deleted: bool) -> UserSummary {
    UserSummary {
      id: id_from_raw(id),
      maybe_username: (!is_deleted).then(|| display_name.to_lowercase().replace(' ', ".")),
      display_name: display_name.into(),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated,
      is_deleted,
    }
  }

  fn ids(users: &serde_json::Value) -> Vec<&str> {
    users
      .as_array()
      .into_iter()
      .flatten()
      .filter_map(|user| user["id"].as_str())
      .collect()
  }
}

#[cfg(test)]
mod avatar_tests {
  use super::*;

  const ANSWER: &str = r#"{"download_url":"https://files.example.com/a?sig=1","content_type":"image/jpeg","size_bytes":1234,"expires_at":0}"#;

  #[test]
  fn reads_the_location_of_a_picture() {
    let (url, size) = avatar_location(ANSWER, false).unwrap();
    assert_eq!(url, "https://files.example.com/a?sig=1");
    assert_eq!(size, 1234);
  }

  #[test]
  fn refuses_other_content_types() {
    let answer = ANSWER.replace("image/jpeg", "text/html");
    assert!(avatar_location(&answer, false).is_err());
  }

  #[test]
  fn refuses_other_schemes() {
    let answer = ANSWER.replace("https://files.example.com/a?sig=1", "file:///etc/passwd");
    assert!(avatar_location(&answer, false).is_err());
  }

  #[test]
  fn plain_http_only_where_the_server_is_plain_http() {
    let answer = ANSWER.replace("https://", "http://");
    assert!(avatar_location(&answer, false).is_err());
    assert!(avatar_location(&answer, true).is_ok());
  }

  #[test]
  fn refuses_garbage() {
    assert!(avatar_location("<html>login</html>", false).is_err());
  }
}
