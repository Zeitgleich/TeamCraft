//! Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md
//! §2.4.1, issue #799), for the mobile apps: the channel owner's list with Approve and Decline, the
//! notifications for new requests, and the admin's own "Ask to join" and "Withdraw".
//!
//! The decisions are the core's (`teamcraft-core` `client::join_requests`): the server's list is
//! only a hint, only requests for a private channel this device is in are listed, Approve is
//! offered only for an admin of the fresh verified roster, and approving is this device's own MLS
//! add, sharing the history from the join on unless the owner explicitly chose the earlier history
//! too. The apps get plain records and an outcome per action; the channel browser marks requested
//! channels through the `browse_channels` query (`join: requested`).

use std::cell::Cell;
use std::rc::Rc;

use teamcraft_client_api::activity::{JoinRequest, JoinRequestStanding as Standing};
use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::join_request_alerts::{self, JoinRequestAlerts};
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_core::CoreError;
use teamcraft_core::store::{LocalStore, Table};
use teamcraft_proto::{ErrorCode, HistoryPolicy};

use crate::TeamCraft;
use crate::engine::{Engine, parse_id};
use crate::error::TeamCraftError;

/// Where this device remembers which requests it notified about (`JoinRequestAlerts` as JSON), so
/// a restart doesn't notify again. Device-local state next to the sync cursors; no screen reads it.
const ALERTS_KEY: &str = "admin_join_alerts";

/// An admin's request to join a private channel the user owns, waiting for a decision.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JoinRequestItem {
  pub conversation_id: String,
  /// The channel's name as this device knows it; empty if it doesn't.
  pub conversation_name: String,
  pub user_id: String,
  /// The admin's name as the server gave it: they may share nothing else with the user yet.
  pub user_name: String,
  /// Their username, so two people with one name differ; absent from an older server.
  pub maybe_username: Option<String>,
  /// How the apps name the admin: "Name (@username)", or the name alone without a username.
  pub user_label: String,
  /// Server time, for ordering ("oldest first") and display.
  pub requested_at_ms: i64,
  /// When the server closes the request if nobody decides (its clock).
  pub expires_at_ms: i64,
  pub standing: JoinRequestStanding,
  /// The tag of the request's notification: one per admin and channel, so a newer request
  /// replaces an older one's notification.
  pub notification_tag: String,
}

/// What the owner can do with a request, as this device's verified roster says (admin.md §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JoinRequestStanding {
  /// The fresh signed roster lists the person as an admin: Approve adds them to the channel.
  Approvable,
  /// Only the server calls the person an admin, or the roster is too old to trust: Decline only.
  NotAnAdmin,
}

/// What approving shares with the admin (e2ee.md §4.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JoinHistory {
  /// What a self-join gets: the history from the join on. The default, and always from a
  /// notification.
  FromJoin,
  /// The earlier history too: only when the owner explicitly chose it.
  Full,
}

/// What changed for the notifications since the app last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct JoinRequestAlertChanges {
  /// Requests to notify about now (once per request and device, also across restarts).
  pub new: Vec<JoinRequestItem>,
  /// Tags of notifications whose request is gone: decided anywhere, withdrawn or expired.
  pub gone_tags: Vec<String>,
}

/// How asking, withdrawing or deciding ended. Failures to reach the server and the rest are errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JoinRequestOutcome {
  Done,
  /// The request is no longer open: decided on another device or by another owner, withdrawn, or
  /// expired. Nothing changed; the lists update by themselves.
  Gone,
  /// Asking: this user isn't an admin in the fresh signed roster. Approving: the person isn't.
  NotAnAdmin,
  /// Asking again too soon after a decline or a withdrawal: possible once that request would have
  /// expired.
  TooSoon,
  /// Approving: this device's own state disagrees with the request (the verified policy no longer
  /// asks for approval, or it isn't a private channel this device knows).
  NotApprovable,
}

/// One action on a request, run on the client.
#[derive(Clone, Debug)]
enum JoinStep {
  Request(ConversationId),
  Withdraw(ConversationId),
  Approve(ConversationId, UserId, HistoryPolicy),
  Decline(ConversationId, UserId),
}

#[uniffi::export]
impl TeamCraft {
  /// The requests waiting for this user's decision as the owner of the channels they name, oldest
  /// first. Read it again when the app is told about a change.
  pub async fn join_requests(&self) -> Result<Vec<JoinRequestItem>, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.join_requests() }))
      .await
  }

  /// The requests to notify about and the notifications to withdraw since the last call; this
  /// device remembers what it notified about. Call it on changes while the app runs.
  pub async fn join_request_alerts(&self) -> Result<JoinRequestAlertChanges, TeamCraftError> {
    self
      .call(|engine| Box::pin(async move { engine.join_request_alerts() }))
      .await
  }

  /// This admin asks the owners of the private channel to let them in ("Ask to join"); asking
  /// again while the request is open changes nothing.
  pub async fn request_admin_join(
    &self,
    conversation_id: String,
  ) -> Result<JoinRequestOutcome, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          let step = JoinStep::Request(parse_id(&conversation_id)?);
          engine.run_join_step(step).await
        })
      })
      .await
  }

  /// This admin takes their request back.
  pub async fn withdraw_admin_join(
    &self,
    conversation_id: String,
  ) -> Result<JoinRequestOutcome, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          let step = JoinStep::Withdraw(parse_id(&conversation_id)?);
          engine.run_join_step(step).await
        })
      })
      .await
  }

  /// The channel owner lets the admin in: the server membership, this device's own MLS add
  /// sharing `history` (`Full` only when the owner explicitly chose it; a notification's Approve
  /// always passes `FromJoin`), and the visible "joined as a workspace admin, approved by"
  /// message. Only for an admin of the fresh verified roster (`NotAnAdmin`) and a private channel
  /// this device is in under the verified `owner_approval` (`NotApprovable`).
  pub async fn approve_admin_join(
    &self,
    conversation_id: String,
    user_id: String,
    history: JoinHistory,
  ) -> Result<JoinRequestOutcome, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          let policy = match history {
            JoinHistory::FromJoin => HistoryPolicy::FromJoin,
            JoinHistory::Full => HistoryPolicy::Full,
          };
          let step = JoinStep::Approve(parse_id(&conversation_id)?, parse_id(&user_id)?, policy);
          engine.run_join_step(step).await
        })
      })
      .await
  }

  /// The channel owner declines; the admin may ask again.
  pub async fn decline_admin_join(
    &self,
    conversation_id: String,
    user_id: String,
  ) -> Result<JoinRequestOutcome, TeamCraftError> {
    self
      .call(move |engine| {
        Box::pin(async move {
          let step = JoinStep::Decline(parse_id(&conversation_id)?, parse_id(&user_id)?);
          engine.run_join_step(step).await
        })
      })
      .await
  }
}

impl Engine {
  /// Reads the stored list without the activity feed (cheap: the apps ask on every change), oldest
  /// first like Activity on desktop.
  fn join_requests(&self) -> Result<Vec<JoinRequestItem>, TeamCraftError> {
    let backend = self.backend()?;
    let mut requests = backend.join_requests();
    requests.sort_by_key(|request| (request.requested_at, request.conversation, request.user));
    let channel_name = |conversation| backend.conversation(conversation).map(|channel| channel.name);
    Ok(join_request_items(&requests, channel_name))
  }

  fn join_request_alerts(&mut self) -> Result<JoinRequestAlertChanges, TeamCraftError> {
    let backend = self.backend()?;
    let requests = backend.join_requests();
    let store = self.store.as_ref();
    let maybe_saved = store
      .get(Table::SyncState, ALERTS_KEY)?
      .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let mut alerts = maybe_saved
      .as_deref()
      .map(JoinRequestAlerts::from_json)
      .unwrap_or_default();
    let channel_name = |conversation| backend.conversation(conversation).map(|channel| channel.name);
    let changes = alert_changes(&mut alerts, &requests, channel_name);
    let updated = alerts.to_json();
    if maybe_saved.as_deref() != Some(updated.as_str()) {
      store.put(Table::SyncState, ALERTS_KEY, updated.as_bytes())?;
    }
    Ok(changes)
  }

  /// Runs `step` on the client with syncing paused (approving commits to the channel's group), so
  /// its failure reaches the app; a refusal the app words is an outcome ([`join_outcome`]).
  async fn run_join_step(&mut self, step: JoinStep) -> Result<JoinRequestOutcome, TeamCraftError> {
    let outcome = Rc::new(Cell::new(JoinRequestOutcome::Done));
    let reported = Rc::clone(&outcome);
    let logged = step.clone();
    self
      .with_client(move |client| {
        Box::pin(async move {
          let result = match step {
            JoinStep::Request(conversation) => client.request_admin_join(conversation).await,
            JoinStep::Withdraw(conversation) => client.withdraw_admin_join(conversation).await,
            JoinStep::Approve(conversation, user, history) => {
              client
                .approve_admin_join_sharing(conversation, user, history)
                .await
            },
            JoinStep::Decline(conversation, user) => client.decline_admin_join(conversation, user).await,
          };
          reported.set(join_outcome(result)?);
          Ok(())
        })
      })
      .await?;
    if outcome.get() != JoinRequestOutcome::Done {
      tracing::info!(step = ?logged, outcome = ?outcome.get(), "admin join request not changed");
    }
    Ok(outcome.get())
  }
}

/// The app's records for `requests`, named with the channels this device knows.
fn join_request_items(
  requests: &[JoinRequest],
  channel_name: impl Fn(ConversationId) -> Option<String>,
) -> Vec<JoinRequestItem> {
  requests
    .iter()
    .map(|request| join_request_item(request, &channel_name))
    .collect()
}

fn join_request_item(
  request: &JoinRequest,
  channel_name: &impl Fn(ConversationId) -> Option<String>,
) -> JoinRequestItem {
  JoinRequestItem {
    conversation_id: request.conversation.to_string(),
    conversation_name: channel_name(request.conversation).unwrap_or_default(),
    user_id: request.user.to_string(),
    user_name: request.user_name.clone(),
    maybe_username: request.maybe_username.clone(),
    user_label: match &request.maybe_username {
      Some(username) => format!("{} (@{username})", request.user_name),
      None => request.user_name.clone(),
    },
    requested_at_ms: request.requested_at,
    expires_at_ms: request.expires_at,
    standing: match request.standing {
      Standing::Approvable => JoinRequestStanding::Approvable,
      Standing::NotAnAdmin => JoinRequestStanding::NotAnAdmin,
    },
    notification_tag: join_request_alerts::tag(request).as_str().to_owned(),
  }
}

/// Takes the requests waiting now into `alerts` and says what to notify and withdraw.
fn alert_changes(
  alerts: &mut JoinRequestAlerts,
  requests: &[JoinRequest],
  channel_name: impl Fn(ConversationId) -> Option<String>,
) -> JoinRequestAlertChanges {
  let changes = alerts.update(requests);
  JoinRequestAlertChanges {
    new: join_request_items(&changes.new, channel_name),
    gone_tags: changes.gone.iter().map(|tag| tag.as_str().to_owned()).collect(),
  }
}

/// A request that is gone, asking too soon, a missing admin role and a request this device won't
/// approve are answers the app words; anything else stays an error.
fn join_outcome(result: Result<(), CoreError>) -> Result<JoinRequestOutcome, CoreError> {
  match result {
    Ok(()) => Ok(JoinRequestOutcome::Done),
    Err(CoreError::NotAnAdmin(_)) => Ok(JoinRequestOutcome::NotAnAdmin),
    Err(CoreError::JoinRequestNotApprovable(_)) => Ok(JoinRequestOutcome::NotApprovable),
    Err(CoreError::Api(api)) => match api.error_code() {
      Some(ErrorCode::AdminJoinRequestNotFound) => Ok(JoinRequestOutcome::Gone),
      Some(ErrorCode::AdminJoinRequestTooSoon) => Ok(JoinRequestOutcome::TooSoon),
      _ => Err(CoreError::Api(api)),
    },
    Err(other) => Err(other),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_core::JoinRequestRefusal;
  use teamcraft_core::api::ApiError;
  use teamcraft_proto::ErrorBody;

  use super::*;

  const CHANNEL: u64 = 50;
  const ADMIN: u64 = 2;
  const STRANGER: u64 = 3;

  #[test]
  fn requests_become_records_with_the_channel_name_and_what_the_owner_may_do() {
    let items = join_request_items(
      &[
        request(ADMIN, 100, Standing::Approvable),
        request(STRANGER, 200, Standing::NotAnAdmin),
      ],
      named,
    );
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].conversation_id, CHANNEL.to_string());
    assert_eq!(items[0].conversation_name, "planning");
    assert_eq!(items[0].user_id, ADMIN.to_string());
    assert_eq!(items[0].user_name, "Admin 2");
    assert_eq!(items[0].maybe_username.as_deref(), Some("admin.2"));
    assert_eq!(items[0].user_label, "Admin 2 (@admin.2)");
    assert_eq!(items[0].requested_at_ms, 100);
    assert_eq!(items[0].expires_at_ms, 1_100);
    assert_eq!(items[0].standing, JoinRequestStanding::Approvable);
    assert_eq!(items[0].notification_tag, format!("admin-join:{CHANNEL}:{ADMIN}"));
    assert_eq!(items[1].standing, JoinRequestStanding::NotAnAdmin);
    // An older server sends no username; a channel this device doesn't name stays unnamed.
    let older = JoinRequest {
      maybe_username: None,
      ..request(ADMIN, 100, Standing::Approvable)
    };
    let unknown = join_request_items(&[older], |_| None);
    assert_eq!(unknown[0].conversation_name, "");
    assert_eq!(unknown[0].user_label, "Admin 2");
  }

  #[test]
  fn a_request_notifies_once_and_its_notification_goes_with_it() {
    let mut alerts = JoinRequestAlerts::default();
    let waiting = [request(ADMIN, 100, Standing::Approvable)];
    let first = alert_changes(&mut alerts, &waiting, named);
    assert_eq!(first.new.len(), 1);
    assert!(first.gone_tags.is_empty());
    // Every later change reads the list again: nothing new.
    let mut restored = JoinRequestAlerts::from_json(&alerts.to_json());
    assert_eq!(
      alert_changes(&mut restored, &waiting, named),
      JoinRequestAlertChanges::default()
    );
    let decided = alert_changes(&mut restored, &[], named);
    assert!(decided.new.is_empty());
    assert_eq!(decided.gone_tags, [format!("admin-join:{CHANNEL}:{ADMIN}")]);
  }

  #[test]
  fn refusals_the_app_words_are_outcomes_and_the_rest_errors() {
    assert_eq!(join_outcome(Ok(())).unwrap(), JoinRequestOutcome::Done);
    assert_eq!(
      join_outcome(Err(CoreError::NotAnAdmin(id_from_raw(ADMIN)))).unwrap(),
      JoinRequestOutcome::NotAnAdmin
    );
    assert_eq!(
      join_outcome(Err(refused(ErrorCode::AdminJoinRequestNotFound))).unwrap(),
      JoinRequestOutcome::Gone
    );
    assert_eq!(
      join_outcome(Err(refused(ErrorCode::AdminJoinRequestTooSoon))).unwrap(),
      JoinRequestOutcome::TooSoon
    );
    for refusal in [
      JoinRequestRefusal::PolicyWithoutApproval,
      JoinRequestRefusal::NotAPrivateChannel,
    ] {
      assert_eq!(
        join_outcome(Err(CoreError::JoinRequestNotApprovable(refusal))).unwrap(),
        JoinRequestOutcome::NotApprovable
      );
    }
    assert!(join_outcome(Err(refused(ErrorCode::Forbidden))).is_err());
    assert!(join_outcome(Err(CoreError::Api(ApiError::Network("offline".into())))).is_err());
    assert!(join_outcome(Err(CoreError::NoGroup(id_from_raw(CHANNEL)))).is_err());
  }

  fn request(user: u64, requested_at: i64, standing: Standing) -> JoinRequest {
    JoinRequest {
      conversation: id_from_raw(CHANNEL),
      user: id_from_raw(user),
      user_name: format!("Admin {user}"),
      maybe_username: Some(format!("admin.{user}")),
      requested_at,
      expires_at: requested_at + 1_000,
      standing,
    }
  }

  fn named(conversation: ConversationId) -> Option<String> {
    (conversation == id_from_raw(CHANNEL)).then(|| "planning".to_owned())
  }

  fn refused(error_code: ErrorCode) -> CoreError {
    CoreError::Api(ApiError::Status {
      status: 404,
      maybe_body: Some(ErrorBody::new(error_code, "refused")),
      maybe_retry_after: None,
    })
  }
}
