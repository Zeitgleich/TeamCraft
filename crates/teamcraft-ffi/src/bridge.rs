//! The JSON bridge to the chat backend: the same `ChatAction` / `ChatQuery` / `ChatSnapshot`
//! messages the desktop shell exchanges with its webview (teamcraft-client-api `remote`), so the
//! mobile apps reuse the core's chat logic instead of growing their own. The app mirrors the view
//! models as plain data classes.

use teamcraft_client_api::remote::{
  ChatAction, ChatQuery, ChatSnapshot, ContactKeys, MessageWindow, SecuritySnapshot,
};
use teamcraft_core::records;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::org_recovery::RecoveryRequestKey;
use teamcraft_proto::{
  Base64Bytes, CreateRecoveryRequest, RecoveryRequestId, RecoveryRequestResponse, RecoveryResultPayload,
};

use crate::engine::{ClientSlot, Engine, parse_id};
use crate::error::TeamCraftError;
use crate::types::{AccountState, OrgRecoveryStart};

/// A recovery this (new) device asked the organization for (e2ee.md §6.3.1).
pub(crate) struct PendingRecovery {
  key: RecoveryRequestKey,
  request_id: RecoveryRequestId,
  user: teamcraft_proto::UserId,
}

impl Engine {
  pub(crate) fn snapshot_json(&self) -> Result<String, TeamCraftError> {
    let snapshot = ChatSnapshot::read(self.backend()?, self.security_snapshot()?);
    to_json(&snapshot)
  }

  pub(crate) fn window_json(
    &self,
    conversation_id: &str,
    start: u32,
    len: u32,
  ) -> Result<String, TeamCraftError> {
    let window = MessageWindow::read(
      self.backend()?,
      parse_id(conversation_id)?,
      usize::try_from(start).unwrap_or(usize::MAX),
      usize::try_from(len).unwrap_or(usize::MAX),
    );
    to_json(&window)
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
      ChatAction::SetActivity { .. } | ChatAction::SetAttention { .. } => Ok(()),
      ChatAction::PublishPolicy { .. }
      | ChatAction::PublishRoster { .. }
      | ChatAction::AdminJoinChannel { .. } => Err(TeamCraftError::InvalidInput(
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
    let contacts = records::pinned_identities(self.store.as_ref())?
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
      maybe_push_content: Some(teamcraft_core::push_preview::push_content_mode(
        self.store.as_ref(),
      )),
      maybe_owners: {
        let settings = teamcraft_core::ClientSettings::default();
        teamcraft_core::workspace::owner_roster_status(
          self.store.as_ref(),
          settings.workspace_max_age,
          settings.workspace_expiry_warning,
        )
      },
      maybe_presence_policy: teamcraft_core::workspace::presence_policy(self.store.as_ref()),
    })
  }
}

fn to_json(value: &impl serde::Serialize) -> Result<String, TeamCraftError> {
  serde_json::to_string(value).map_err(|error| TeamCraftError::Core(error.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TeamCraftError> {
  serde_json::from_str(text).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))
}
