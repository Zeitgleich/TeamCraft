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
use teamcraft_core::ui_backend::FeedAnchor;
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::org_recovery::RecoveryRequestKey;
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, Base64Bytes, ConversationId, CreateRecoveryRequest, DeviceActivity,
  ErrorCode, RecoveryRequestId, RecoveryRequestResponse, RecoveryResultPayload,
};

use crate::engine::{Api, Engine, feed_anchor, older_len, parse_id};
use crate::error::TeamCraftError;
use crate::reads::Reads;
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

impl Reads {
  pub(crate) fn snapshot_json(&mut self) -> Result<String, TeamCraftError> {
    let security = self.security_snapshot()?;
    let changes = self.store.take_changes();
    let account = self.account_mut()?;
    let (backend, snapshots) = account.parts();
    to_json(&AppSnapshot {
      snapshot: snapshots.snapshot(backend, changes, || Some(security)),
      former_users: backend.former_users(),
    })
  }

  pub(crate) fn window_json(
    &self,
    conversation_id: &str,
    start: u32,
    len: u32,
  ) -> Result<String, TeamCraftError> {
    let conversation = parse_id(conversation_id)?;
    // The next catch-up starts with the conversation the user opened (issue #972).
    self.focus(conversation);
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
    self.focus(conversation);
    self.tail_of(
      conversation,
      feed_anchor(maybe_oldest_id, oldest_sent_at_ms)?,
      older_len(older),
    )
  }

  /// The newest `len` messages, read ahead for a conversation the user may open next: unlike
  /// [`Self::tail_json`] it leaves the catch-up focus where the user is.
  pub(crate) fn tail_ahead_json(&self, conversation_id: &str, len: u32) -> Result<String, TeamCraftError> {
    self.tail_of(parse_id(conversation_id)?, None, older_len(len))
  }

  fn tail_of(
    &self,
    conversation: ConversationId,
    maybe_anchor: Option<FeedAnchor>,
    older: usize,
  ) -> Result<String, TeamCraftError> {
    let backend = self.backend()?;
    let tail = backend.feed_tail(conversation, maybe_anchor, older);
    let gaps = tail.gaps_among(backend.history_gaps(conversation));
    to_json(&MessageTail {
      conversation,
      messages: tail.messages,
      has_older: tail.has_older,
      undecrypted: backend.undecrypted_count(conversation),
      gaps,
    })
  }

  pub(crate) fn query_json(&self, query: &str) -> Result<String, TeamCraftError> {
    let started = std::time::Instant::now();
    let started_at_ms = crate::perf_jobs::now_ms();
    let kind = query_kind(query);
    // Tracing (docs/tracing.md): the layer keeps the name only when the kind is a code name.
    let span = tracing::trace_span!("db.query", name = tracing::field::Empty);
    if !span.is_disabled() {
      span.record("name", format!("query_{kind}").as_str());
    }
    let _entered = span.enter();
    let parsed: ChatQuery = from_json(query)?;
    let answer = to_json(&parsed.answer(self.backend()?));
    crate::perf_jobs::query_ran(&kind, started_at_ms, started.elapsed());
    crate::logging::slow_query(kind, started.elapsed());
    answer
  }

  /// An action that only needs the backend ([`needs_client`]): its local half.
  pub(crate) fn apply_action(&self, action: ChatAction) -> Result<(), TeamCraftError> {
    match action.apply_to(self.backend()?) {
      Some(unhandled) => Err(TeamCraftError::InvalidInput(format!(
        "unsupported action: {unhandled:?}"
      ))),
      None => Ok(()),
    }
  }

  /// The notetaker in the call of `conversation_id` with `participants` (the others), as desktop
  /// and web show it (`teamcraft_client_api::notetaker`). The mobile apps can move every call, so
  /// the notetaker can be brought into any call where the policy lets the user ask it in (added
  /// where the user can, `Engine::add_notetaker`, moved there otherwise).
  pub(crate) fn notetaker_in_call(
    &self,
    conversation_id: &str,
    participants: &[String],
  ) -> Result<Option<crate::NotetakerCallInfo>, TeamCraftError> {
    let conversation = parse_id(conversation_id)?;
    let participants = participants
      .iter()
      .map(|id| parse_id(id))
      .collect::<Result<Vec<_>, _>>()?;
    let backend = self.backend()?;
    let Some(policy) = backend.notetaker_policy() else {
      return Ok(None);
    };
    let me = backend.me();
    let member_ids = backend
      .conversation(conversation)
      .map(|found| found.member_ids)
      .unwrap_or_default();
    Ok(
      teamcraft_client_api::notetaker::notetaker_in_call(
        &policy,
        !me.is_guest && !me.is_bot,
        backend.can_manage_integration_sources(),
        &member_ids,
        &participants,
        true,
      )
      .map(Into::into),
    )
  }

  fn security_snapshot(&self) -> Result<SecuritySnapshot, TeamCraftError> {
    let my_fingerprint = self.identity_fingerprint()?;
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
      my_fingerprint,
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
      maybe_voice_message_policy: teamcraft_core::workspace::voice_message_policy(&self.store),
      maybe_notetaker: teamcraft_core::notetaker::notetaker_policy(&self.store),
      // The Android screen doesn't name them yet.
      bug_report_recipients: Vec::new(),
      maybe_backup: teamcraft_core::ui_snapshot::key_backup_state(&self.store).unwrap_or_else(|error| {
        // The app then shows the backup as unknown; the rest of the snapshot still counts.
        tracing::warn!(%error, "the key backup state is unreadable");
        None
      }),
    })
  }
}

/// Whether an action takes the client (encryption groups, keys) or the engine, and so runs there;
/// the others run on the backend ([`Reads::apply_action`]).
pub(crate) fn needs_client(action: &ChatAction) -> bool {
  matches!(
    action,
    ChatAction::ChangePassphrase { .. }
      | ChatAction::AcceptKeyChange { .. }
      | ChatAction::SetActivity { .. }
      | ChatAction::SetAttention { .. }
      | ChatAction::PublishPolicy { .. }
      | ChatAction::PublishRoster { .. }
      | ChatAction::AdminJoinChannel { .. }
      | ChatAction::AdminRemoveFromAllChats { .. }
      | ChatAction::AdminDeleteUser { .. }
      | ChatAction::AdminRemoveGroupLeftover { .. }
      | ChatAction::ScheduleMessage { .. }
      | ChatAction::UpdateScheduled { .. }
      | ChatAction::CancelScheduled { .. }
      | ChatAction::SendScheduledNow { .. }
      | ChatAction::SendBugReport { .. }
  )
}

impl Engine {
  /// Runs an action that [`needs_client`]; admin actions stay on desktop and web.
  pub(crate) async fn act(&mut self, action: ChatAction) -> Result<(), TeamCraftError> {
    match action {
      ChatAction::ScheduleMessage { message, due_at } => {
        self
          .with_client(|client| Box::pin(async move { client.schedule_now_for(message, due_at).await }))
          .await
      },
      ChatAction::UpdateScheduled { id, body, due_at } => {
        self
          .with_client(|client| Box::pin(async move { client.change_scheduled(id, body, due_at).await }))
          .await
      },
      ChatAction::CancelScheduled { id } => {
        self
          .with_client(|client| Box::pin(async move { client.drop_scheduled(id).await }))
          .await
      },
      ChatAction::SendBugReport { report } => {
        self
          .with_client(|client| Box::pin(async move { client.queue_bug_report(report).await }))
          .await
      },
      ChatAction::SendScheduledNow { id } => {
        self
          .with_client(|client| Box::pin(async move { client.release_scheduled(id).await }))
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
      other => Err(TeamCraftError::InvalidInput(format!(
        "unsupported action: {other:?}"
      ))),
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

  /// An integration source's picture, from the local store.
  pub(crate) fn integration_source_picture(
    &self,
    conversation: &str,
    sha256: &str,
  ) -> Result<Option<Vec<u8>>, TeamCraftError> {
    let conversation: teamcraft_proto::ConversationId = parse_id(conversation)?;
    let Ok(sha256) = teamcraft_proto::SourcePictureHash::from_hex(sha256) else {
      return Ok(None);
    };
    Ok(teamcraft_core::integration_sources::source_picture(
      &self.store,
      conversation,
      sha256,
    )?)
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
}

pub(crate) fn to_json(value: &impl serde::Serialize) -> Result<String, TeamCraftError> {
  serde_json::to_string(value).map_err(|error| TeamCraftError::Core(error.to_string()))
}

pub(crate) fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, TeamCraftError> {
  serde_json::from_str(text).map_err(|error| TeamCraftError::InvalidInput(error.to_string()))
}

/// Which kind of query `json` is, for a log line (never its content: a search's text, a name).
fn query_kind(json: &str) -> String {
  #[derive(serde::Deserialize)]
  struct Kind {
    kind: String,
  }
  serde_json::from_str::<Kind>(json).map_or_else(|_| "unknown".to_owned(), |kind| kind.kind)
}

/// The profile picture of `user` as image bytes (JPEG, PNG or WebP); `None` if the person has
/// none. The API names a presigned URL (profile pictures are not secret from members) and the
/// download is capped at `max_bytes`, so a hostile answer can't make the phone buffer a huge body.
/// Runs on the HTTP runtime, beside the engine: two round trips that held up every job behind them
/// at the app's start, when the screens ask for many pictures at once.
pub(crate) async fn avatar_picture(
  api: &Api,
  user: &str,
  max_bytes: u64,
) -> Result<Option<Vec<u8>>, TeamCraftError> {
  let user: teamcraft_proto::UserId = parse_id(user)?;
  let answer = match api
    .raw_request("GET", &format!("/users/{user}/avatar"), None)
    .await
  {
    Ok(answer) => answer,
    Err(error) if error.error_code() == Some(&ErrorCode::AvatarNotFound) => return Ok(None),
    Err(error) => return Err(teamcraft_core::CoreError::from(error).into()),
  };
  // Plain http only for a development server on this computer, which is plain http itself.
  let (url, size_bytes) = avatar_location(&answer, api.base_url().starts_with("http://"))?;
  let picture = api
    .get_blob_capped(&url, size_bytes.min(max_bytes))
    .await
    .map_err(teamcraft_core::CoreError::from)?;
  Ok(Some(picture))
}

/// The picture of a space as PNG bytes (`GET /spaces/{id}/image`, at most `max_bytes`); `None`
/// when the space has none (any more). The app caches by the version the space list names, so
/// it asks only for a version it doesn't hold. Runs on the HTTP runtime, like [`avatar_picture`].
pub(crate) async fn space_picture(
  api: &Api,
  space: &str,
  max_bytes: u64,
) -> Result<Option<Vec<u8>>, TeamCraftError> {
  let space: teamcraft_proto::SpaceId = parse_id(space)?;
  match api
    .get_bytes_capped(&format!("/spaces/{space}/image"), max_bytes)
    .await
  {
    Ok(picture) => Ok(Some(picture)),
    Err(error) if error.error_code() == Some(&ErrorCode::SpaceImageNotFound) => Ok(None),
    Err(error) => Err(teamcraft_core::CoreError::from(error).into()),
  }
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
    let backend = teamcraft_core::ui_backend::CoreChatBackend::new(
      store,
      id_from_raw(1),
      Box::new(ManualClock::default()),
    );

    let json = serde_json::to_value(AppSnapshot {
      snapshot: ChatSnapshot::read(&backend, SecuritySnapshot::default()),
      former_users: backend.former_users(),
    })
    .unwrap();

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
